use std::{
    io::Read,
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use crate::config::{LaunchMode, LauncherConfig};

/// 查询类命令（dsh --version、npm view/search）。npx 首次运行可能要现场下载包。
pub const QUERY_TIMEOUT: Duration = Duration::from_secs(120);
/// 单个插件安装/卸载（网络差时 npm 安装可能很慢）。
pub const PLUGIN_ACTION_TIMEOUT: Duration = Duration::from_secs(300);
/// npm install -g 全量更新 dsh。
pub const GLOBAL_INSTALL_TIMEOUT: Duration = Duration::from_secs(600);

#[derive(Debug, Clone, serde::Serialize)]
pub struct OperationResult {
    pub success: bool,
    pub output: String,
}

/// 按 PATH 顺序找可执行文件，返回第一个存在的完整路径。
///
/// 直接用进程环境里的 PATH 拼路径，不经过 where.exe：后者按控制台代码页
/// 输出，中文安装路径会被读成乱码，而 PATH 本身在 Rust 里是 Unicode 的。
#[cfg(windows)]
fn find_in_path(name: &str) -> Option<String> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        if dir.as_os_str().is_empty() {
            continue;
        }
        let full = dir.join(name);
        if full.is_file() {
            return Some(full.to_string_lossy().into_owned());
        }
    }
    None
}

#[cfg(windows)]
pub fn hide_console(command: &mut Command) {
    // GUI 进程派生控制台子进程（npm/taskkill/dsh 等）时，缺少该标志会在
    // release 版弹出黑色控制台窗口。
    //
    // 必须连 CREATE_UNICODE_ENVIRONMENT 一起给：creation_flags 会替换默认
    // 标志，少了它，环境块里的中文（本机 dsh 装在 E:\项目开发\...）会被按
    // ANSI 解释成乱码，含中文路径的 .cmd 垫片直接起不来。
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    const CREATE_UNICODE_ENVIRONMENT: u32 = 0x0000_0400;
    command.creation_flags(CREATE_NO_WINDOW | CREATE_UNICODE_ENVIRONMENT);
}

#[cfg(not(windows))]
pub fn hide_console(_command: &mut Command) {}

/// 把用户级环境变量里缺失的 DSH_* 补给子进程。
///
/// 桌面图标/资源管理器启动的 GUI 进程拿到的是「登录那一刻」的环境快照：
/// 用户后来用 setx 写的 DSH_CLI_BIN、DSH_NODE_BIN 并不在里面，而本机 dsh 的
/// 启动垫片（npm-global\dsh.cmd）正是靠这两个变量去找 CLI 入口，缺了就直接
/// exit 1 且什么都不打印 —— 表现为启动器点启动没反应。这里从注册表补回缺的
/// 两个值，让 GUI 里的行为和用户在终端敲 dsh 完全一致。
#[cfg(windows)]
pub fn merge_user_environment(command: &mut Command) {
    for name in ["DSH_CLI_BIN", "DSH_NODE_BIN"] {
        if std::env::var_os(name).is_some() {
            continue;
        }
        if let Some(value) = read_user_environment(name) {
            command.env(name, value);
        }
    }
    merge_user_path(command);
}

/// 把注册表里用户级 PATH 的目录并进子进程 PATH。
///
/// 用户在设置面板里改过 PATH 之后，已经运行着的资源管理器不会刷新自己的
/// 环境，于是从桌面图标启动的程序拿到的还是旧 PATH —— 恰好在 npm 全局目录
/// （这里是自定义的 E:\开发环境\nodejs\npm-global）是后加的时候，启动器就
/// 找不到 dsh，而同一台机器上新开的终端却能找到。这里按目录去重后补齐，
/// 让两条路看到同一个 PATH。
#[cfg(windows)]
fn merge_user_path(command: &mut Command) {
    let Some(user_path) = read_user_environment("PATH") else {
        return;
    };
    let current = std::env::var("PATH").unwrap_or_default();
    let existing: Vec<String> = current
        .split(';')
        .map(|part| part.trim().to_ascii_lowercase())
        .collect();
    let mut merged = current.clone();
    for dir in user_path.split(';') {
        let dir = dir.trim();
        if dir.is_empty() || existing.iter().any(|item| item == &dir.to_ascii_lowercase()) {
            continue;
        }
        if !merged.is_empty() {
            merged.push(';');
        }
        merged.push_str(dir);
    }
    if merged != current {
        command.env("PATH", merged);
    }
}

#[cfg(not(windows))]
pub fn merge_user_environment(_command: &mut Command) {}

/// 读一个用户级环境变量（HKCU\Environment）的当前值。
#[cfg(windows)]
fn read_user_environment(name: &str) -> Option<String> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    // reg.exe 按控制台代码页输出，中文路径会变成乱码；先把代码页切到 UTF-8
    // 再查，读回来的才是真路径。
    const CREATE_UNICODE_ENVIRONMENT: u32 = 0x0000_0400;
    let output = Command::new("cmd")
        .args([
            "/d",
            "/s",
            "/c",
            &format!("chcp 65001 >nul && reg query HKCU\\Environment /v {name}"),
        ])
        .creation_flags(CREATE_NO_WINDOW | CREATE_UNICODE_ENVIRONMENT)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    for line in text.lines() {
        let Some(rest) = line.trim_start().strip_prefix(name) else {
            continue;
        };
        let rest = rest.trim_start();
        // 列之间是制表/空格；值本身可能带空格（C:\Program Files\...），
        // 所以按类型标记切一刀，后面原样保留。
        let value = rest
            .strip_prefix("REG_EXPAND_SZ")
            .or_else(|| rest.strip_prefix("REG_SZ"))
            .unwrap_or(rest)
            .trim();
        if !value.is_empty() {
            return Some(value.to_string());
        }
    }
    None
}

#[cfg(target_os = "macos")]
pub fn adopt_login_shell_path() {
    // macOS 的 GUI 进程从 launchd 继承极简 PATH（不含 Homebrew/nvm 等目录），
    // 直接找 node/npm/dsh 会失败；用登录 shell 输出的 PATH 覆盖当前进程。
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".into());
    if let Ok(output) = Command::new(shell)
        .args(["-lc", "printf %s \"$PATH\""])
        .output()
    {
        if output.status.success() {
            let path = String::from_utf8_lossy(&output.stdout);
            let trimmed = path.trim();
            if !trimmed.is_empty() {
                std::env::set_var("PATH", trimmed);
            }
        }
    }

    // 登录 shell 用 `-l` 启动时只读 .zprofile；nvm/pnpm 等通常把 PATH 写在
    // .zshrc（交互 shell 才读），因此上面的探测拿不到这些目录。再按已知的
    // node/npm 生态安装位置补齐，保证 GUI 进程能找到 `npm install -g
    // @deepseek-ai/dsh` 生成的 dsh、以及 npx/npm 本身。
    if let Some(home) = std::env::var_os("HOME").map(std::path::PathBuf::from) {
        let merged = merge_existing_path_dirs(
            known_path_dirs(&home),
            &std::env::var_os("PATH").unwrap_or_default(),
        );
        std::env::set_var("PATH", merged);
    }
}

/// 收集 macOS 上 node/npm 生态常见的可执行目录（含 nvm 各版本 bin），
/// 越新的 nvm 版本越靠前。只返回候选目录，是否真实存在由合并方过滤。
#[cfg(target_os = "macos")]
fn known_path_dirs(home: &std::path::Path) -> Vec<std::path::PathBuf> {
    use std::path::PathBuf;

    let mut dirs = Vec::new();
    let home = home.to_path_buf();
    dirs.push(home.join(".npm-global").join("bin"));
    // npm prefix 直接指向 $HOME 时，全局命令在 $HOME/node_modules/.bin。
    dirs.push(home.join("node_modules").join(".bin"));
    if let Ok(entries) = std::fs::read_dir(home.join(".nvm").join("versions").join("node")) {
        let mut versions: Vec<PathBuf> = entries.flatten().map(|entry| entry.path()).collect();
        versions.sort_by(|a, b| version_key(a).cmp(&version_key(b)).reverse());
        for version in versions {
            let bin = version.join("bin");
            if bin.is_dir() {
                dirs.push(bin);
            }
        }
    }
    dirs.push(PathBuf::from("/opt/homebrew/bin"));
    dirs.push(PathBuf::from("/opt/homebrew/opt/node@24/bin"));
    dirs.push(PathBuf::from("/opt/homebrew/opt/node@22/bin"));
    dirs.push(PathBuf::from("/opt/homebrew/opt/node@20/bin"));
    dirs.push(PathBuf::from("/opt/homebrew/opt/node@18/bin"));
    dirs.push(PathBuf::from("/usr/local/bin"));
    dirs.push(PathBuf::from("/opt/local/bin")); // MacPorts
    dirs
}

/// 解析 nvm 版本目录名（去掉前导 v）为可比较的 (major, minor, patch)。
#[cfg(target_os = "macos")]
fn version_key(path: &std::path::Path) -> (u64, u64, u64) {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("")
        .trim_start_matches('v');
    let mut parts = name.split('.');
    let parse = |part: Option<&str>| part.and_then(|value| value.parse().ok()).unwrap_or(0);
    (
        parse(parts.next()),
        parse(parts.next()),
        parse(parts.next()),
    )
}

/// 把存在且尚未出现的候选目录追加到当前 PATH，返回合并结果（不覆盖原有值）。
#[cfg(target_os = "macos")]
fn merge_existing_path_dirs(
    dirs: Vec<std::path::PathBuf>,
    current: &std::ffi::OsStr,
) -> std::ffi::OsString {
    use std::env::{join_paths, split_paths};
    use std::path::PathBuf;

    let mut parts: Vec<PathBuf> = split_paths(current).collect();
    for dir in dirs {
        if dir.is_dir() && !parts.contains(&dir) {
            parts.push(dir);
        }
    }
    join_paths(&parts).unwrap_or_else(|_| current.to_os_string())
}

#[cfg(windows)]
pub fn resolve_windows_executable(raw: &str) -> Result<String, String> {
    use std::path::{Path, PathBuf};

    let supplied = PathBuf::from(raw);
    if supplied.is_file() {
        return supplied
            .canonicalize()
            .map(|path| path.to_string_lossy().into_owned())
            .map_err(|error| format!("无法解析 CLI 路径 {raw}：{error}"));
    }

    let has_path_separator = raw.contains(['\\', '/']);
    let extension = supplied.extension().and_then(|value| value.to_str());
    let candidates = if extension.is_some() {
        vec![raw.to_string()]
    } else if has_path_separator {
        vec![format!("{raw}.cmd"), format!("{raw}.exe"), raw.to_string()]
    } else {
        // npm creates both POSIX and Windows shims. Resolve the .cmd shim
        // explicitly so its %~dp0 points at the global npm directory.
        vec![format!("{raw}.cmd"), format!("{raw}.exe")]
    };

    for candidate in &candidates {
        if has_path_separator && Path::new(candidate).is_file() {
            return Ok(candidate.clone());
        }
        // 自己按 PATH 顺序找，不调 where.exe：外部命令按控制台代码页输出，
        // 中文安装路径（本机 dsh 在 E:\项目开发\...）会被读成乱码，拿着乱码
        // 路径去启动进程只会得到一个没有输出的退出码 1。
        if let Some(path) = find_in_path(candidate) {
            return Ok(path);
        }
    }

    // GUI 进程的 PATH 可能缺少 npm 全局目录（用户改过 PATH、装完 Node 没有重新
    // 登录等），where.exe 找不到时再探测几个常见的全局安装位置兜底。
    if !has_path_separator {
        for dir in known_global_bin_dirs() {
            for candidate in &candidates {
                let path = dir.join(candidate);
                if path.is_file() {
                    return Ok(path.to_string_lossy().into_owned());
                }
            }
        }
    }

    Err(format!(
        "找不到命令 {raw}。请确认它能在普通终端中运行，或填写 .cmd/.exe 的完整路径。"
    ))
}

#[cfg(windows)]
fn known_global_bin_dirs() -> Vec<std::path::PathBuf> {
    use std::path::PathBuf;

    let mut result = Vec::new();
    if let Some(appdata) = std::env::var_os("APPDATA") {
        result.push(PathBuf::from(appdata).join("npm"));
    }
    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        let local = PathBuf::from(local);
        result.push(local.join("pnpm"));
        result.push(local.join("Volta").join("bin"));
    }
    if let Some(programs) = std::env::var_os("ProgramFiles") {
        result.push(PathBuf::from(programs).join("nodejs"));
    }
    // 桌面图标启动的进程有时拿不到用户后来改过的 PATH（资源管理器不刷新
    // 自己的环境），把用户级 PATH 的目录也纳入兜底查找，免得连 dsh 都定位不到。
    if let Some(user_path) = read_user_environment("PATH") {
        for dir in user_path.split(';') {
            let dir = dir.trim();
            if !dir.is_empty() {
                result.push(PathBuf::from(dir));
            }
        }
    }
    result
}

pub fn command_for_invocation(config: &LauncherConfig, args: &[String]) -> Result<Command, String> {
    #[cfg(windows)]
    let mut command = match config.launch_mode {
        LaunchMode::Command => Command::new(resolve_windows_executable(config.executable.trim())?),
        LaunchMode::Npx => Command::new(resolve_windows_executable("npx")?),
    };
    #[cfg(not(windows))]
    let mut command = match config.launch_mode {
        LaunchMode::Command => Command::new(config.executable.trim()),
        LaunchMode::Npx => Command::new("npx"),
    };
    if matches!(config.launch_mode, LaunchMode::Npx) {
        command.args(["--yes", &config.npx_package]);
    }
    command.args(args);
    merge_user_environment(&mut command);
    hide_console(&mut command);
    Ok(command)
}

pub fn prepare_command(mut command: Command, config: &LauncherConfig) -> Command {
    command.current_dir(&config.working_directory);
    merge_user_environment(&mut command);
    if !config.dsh_home.trim().is_empty() {
        command.env("DSH_HOME", config.dsh_home.trim());
    }
    command
}

fn drain_stream<R>(stream: Option<R>) -> thread::JoinHandle<String>
where
    R: Read + Send + 'static,
{
    thread::spawn(move || {
        let mut buffer = Vec::new();
        if let Some(mut stream) = stream {
            let _ = stream.read_to_end(&mut buffer);
        }
        String::from_utf8_lossy(&buffer).into_owned()
    })
}

/// 一次命令执行的原始结果。stdout/stderr 分开保留，需要解析结构化输出
/// （例如 `npm view --json`）的调用方不会被 npm 的提示信息干扰。
pub struct CommandCapture {
    pub success: bool,
    pub timed_out: bool,
    pub stdout: String,
    pub stderr: String,
}

/// 运行命令并分别收集 stdout/stderr。超时后终止整棵进程树，
/// 避免挂死的 npm/dsh 把调用方（以及被暂停的 Web 服务）永远卡住。
pub fn run_capture(mut command: Command, timeout: Duration) -> Result<CommandCapture, String> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().map_err(|error| error.to_string())?;
    let stdout = drain_stream(child.stdout.take());
    let stderr = drain_stream(child.stderr.take());
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) => {
                if Instant::now() >= deadline {
                    break None;
                }
                thread::sleep(Duration::from_millis(120));
            }
            Err(error) => return Err(error.to_string()),
        }
    };
    let timed_out = status.is_none();
    if timed_out {
        kill_child_tree(child);
    }
    // 进程结束（或被终止）后管道关闭，读取线程随之结束。
    Ok(CommandCapture {
        success: status.is_some_and(|status| status.success()),
        timed_out,
        stdout: stdout.join().unwrap_or_default(),
        stderr: stderr.join().unwrap_or_default(),
    })
}

/// 运行命令并把 stdout/stderr 合成一段给界面看的文本。
pub fn execute_command(command: Command, timeout: Duration) -> Result<OperationResult, String> {
    let capture = run_capture(command, timeout)?;
    let mut combined = [capture.stdout.trim(), capture.stderr.trim()]
        .into_iter()
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    if capture.timed_out {
        let notice = format!("命令超过 {} 秒未结束，已强制终止。", timeout.as_secs());
        combined = if combined.is_empty() {
            notice
        } else {
            format!("{notice}\n{combined}")
        };
    }
    Ok(OperationResult {
        success: capture.success,
        output: combined,
    })
}

pub fn run_dsh_invocation(
    config: &LauncherConfig,
    args: &[String],
    timeout: Duration,
) -> Result<OperationResult, String> {
    execute_command(
        prepare_command(command_for_invocation(config, args)?, config),
        timeout,
    )
}

pub fn npm_command() -> Command {
    #[cfg(windows)]
    let mut command =
        Command::new(resolve_windows_executable("npm").unwrap_or_else(|_| "npm.cmd".into()));
    #[cfg(not(windows))]
    let mut command = Command::new("npm");
    hide_console(&mut command);
    command
}

pub fn kill_child_tree(mut child: Child) {
    let pid = child.id();
    #[cfg(windows)]
    {
        let mut taskkill = Command::new("taskkill");
        hide_console(&mut taskkill);
        let _ = taskkill
            .args(["/PID", &pid.to_string(), "/T"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        // Give dsh and any descendants a short chance to tear down their own
        // windows before using the forceful fallback.
        thread::sleep(Duration::from_millis(180));
        if child.try_wait().ok().flatten().is_none() {
            let mut force_kill = Command::new("taskkill");
            hide_console(&mut force_kill);
            let _ = force_kill
                .args(["/PID", &pid.to_string(), "/T", "/F"])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
    }
    #[cfg(unix)]
    {
        // dsh 以独立进程组启动（见 service::start_process），负 pid 对整组发信号，
        // 保证 npx → node → dsh 的整棵进程树一起结束。
        unsafe { libc::kill(-(pid as i32), libc::SIGKILL) };
    }
    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(test)]
mod tests {
    #[cfg(any(windows, target_os = "macos"))]
    use super::*;
    #[cfg(windows)]
    use std::path::Path;
    #[cfg(target_os = "macos")]
    use std::{ffi::OsStr, fs, path::PathBuf};

    #[cfg(windows)]
    #[test]
    fn resolves_windows_node_shims_to_absolute_cmd_paths() {
        let path = resolve_windows_executable("npx").expect("npx.cmd should be on PATH");
        assert!(Path::new(&path).is_absolute());
        assert!(path.to_ascii_lowercase().ends_with("npx.cmd"));
    }

    #[cfg(windows)]
    #[test]
    fn executes_resolved_windows_cmd_shims_directly() {
        let path = resolve_windows_executable("npx").expect("npx.cmd should be on PATH");
        let output = Command::new(path)
            .arg("--version")
            .output()
            .expect("Rust should execute an absolute .cmd shim");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(!output.stdout.is_empty());
    }

    #[cfg(windows)]
    #[test]
    fn terminates_commands_that_exceed_their_timeout() {
        let mut command = Command::new("ping");
        command.args(["-n", "30", "127.0.0.1"]);
        let started = Instant::now();
        let result = execute_command(command, Duration::from_secs(1)).expect("spawn ping");
        assert!(!result.success);
        assert!(result.output.contains("已强制终止"));
        assert!(started.elapsed() < Duration::from_secs(10));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn collects_nvm_bins_under_home_newest_first() {
        let base = std::env::temp_dir().join(format!("dsh-home-{}", std::process::id()));
        let older = base
            .join(".nvm")
            .join("versions")
            .join("node")
            .join("v22.0.0")
            .join("bin");
        let newer = base
            .join(".nvm")
            .join("versions")
            .join("node")
            .join("v24.1.0")
            .join("bin");
        let home_bin = base.join("node_modules").join(".bin");
        fs::create_dir_all(&older).expect("create fake older nvm bin");
        fs::create_dir_all(&newer).expect("create fake newer nvm bin");
        fs::create_dir_all(&home_bin).expect("create fake home bin");
        let dirs = known_path_dirs(&base);
        let idx_older = dirs
            .iter()
            .position(|dir| dir == &older)
            .expect("older nvm bin should be listed");
        let idx_newer = dirs
            .iter()
            .position(|dir| dir == &newer)
            .expect("newer nvm bin should be listed");
        assert!(
            idx_newer < idx_older,
            "newest nvm version should come first"
        );
        assert!(
            dirs.contains(&home_bin),
            "npm prefix at HOME should be listed"
        );
        let _ = fs::remove_dir_all(&base);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn merges_existing_dirs_without_duplicating_current_path() {
        let base = std::env::temp_dir().join(format!("dsh-path-{}", std::process::id()));
        let a = base.join("a");
        let b = base.join("b");
        let missing = base.join("missing");
        fs::create_dir_all(&a).expect("create dir a");
        fs::create_dir_all(&b).expect("create dir b");
        let current = std::env::join_paths([&a, &b]).expect("join current path");
        // 已存在的 a、b 不再追加；不存在的目录被过滤。
        let merged =
            merge_existing_path_dirs(vec![a.clone(), b.clone(), missing.clone()], &current);
        assert_eq!(merged, current, "existing dirs must not be duplicated");
        // 新出现且存在的目录被追加。
        let c = base.join("c");
        fs::create_dir_all(&c).expect("create dir c");
        let merged = merge_existing_path_dirs(vec![a, c.clone()], &current);
        let parts: Vec<PathBuf> = std::env::split_paths(&merged).collect();
        assert!(parts.contains(&c), "new dir should be appended");
        let _ = fs::remove_dir_all(&base);
    }

    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "host-specific: asserts the real login machine layout"]
    fn host_path_covers_dsh_and_node() {
        // 把附加的候选目录并入 PATH 后，dsh（Node shim）与 node 都必须在 PATH
        // 里可解析，否则 GUI 进程直接 spawn("dsh") 会 ENOENT。
        if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
            let merged = merge_existing_path_dirs(
                known_path_dirs(&home),
                // 模拟 launchd 给 GUI 进程的极简 PATH（进程现场实测值）。
                OsStr::new("/usr/bin:/bin:/usr/sbin:/sbin"),
            );
            std::env::set_var("PATH", &merged);
            for name in ["dsh", "node"] {
                let output = Command::new("sh")
                    .args(["-c", &format!("command -v {name}")])
                    .output()
                    .expect("sh should run");
                assert!(
                    output.status.success(),
                    "{name} not resolvable via merged PATH: {}",
                    String::from_utf8_lossy(&output.stderr)
                );
                eprintln!(
                    "resolved {name}: {}",
                    String::from_utf8_lossy(&output.stdout).trim()
                );
            }
        }
    }
}
