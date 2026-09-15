use std::{
    fs,
    net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener, TcpStream},
    path::PathBuf,
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};
use tauri::{AppHandle, Manager, State};

use crate::{
    config::{dsh_home_for, read_config, validate_config, LaunchMode, LauncherConfig},
    exec::{command_for_invocation, kill_child_tree},
    state::{
        current_status, emit_status, ops_in_progress_error, snapshot, spawn_log_reader, AppState,
        LauncherStatus, Phase,
    },
    util::truncate_chars,
};

/// `dsh web` 的命令行参数。抽出来是为了能在测试里检查参数拼装，
/// 不必真的去执行 dsh。
///
/// 传 `--no-open`：Launcher 自己内嵌页面，dsh web 默认额外弹的系统浏览器
/// 窗口是多余的。该开关自 `@deepseek-ai/dsh-web-app` 0.1.0-rc.8 起提供；
/// dsh 的 commander 没开 allowUnknownOption，所以 rc.7 及更早的版本收到它
/// 会以 “unknown option” 直接退出。最低支持版本因此是 0.1.0-rc.8。
fn web_args(config: &LauncherConfig) -> Vec<String> {
    let mut args = vec![
        "web".to_string(),
        "--host".to_string(),
        "127.0.0.1".to_string(),
        "--port".to_string(),
        config.port.to_string(),
        "--no-open".to_string(),
    ];
    for host in &config.trusted_hosts {
        args.extend(["--trusted-host".to_string(), host.clone()]);
    }
    args
}

fn command_for_config(config: &LauncherConfig) -> Result<Command, String> {
    command_for_invocation(config, &web_args(config))
}

// —— 安全模式：一次性隔离 DSH_HOME ——
// 借鉴 dsh-desktop 的 Safe Mode：绝不读正式 ~/.dsh，配一次性目录，退出即删。
// 区别是我们把 settings.yaml / .credentials.yaml 尽力拷过去——安全模式里的
// 会话要能调 AI 修东西，没有钥匙就白进了。插件、profile 等可能致坏的东西一概不带。

/// 安全模式的一次性 DSH_HOME（配置目录下 safe-mode/dsh-home）。
fn safe_mode_home(app: &AppHandle) -> Result<PathBuf, String> {
    app.path()
        .app_config_dir()
        .map(|dir| dir.join("safe-mode").join("dsh-home"))
        .map_err(|_| "无法确定配置目录".into())
}

/// 重建一次性目录：先删旧的，再建新的，最后尽力带上钥匙（模型与 API 配置）。
fn reset_safe_mode_home(app: &AppHandle) -> Result<PathBuf, String> {
    let home = safe_mode_home(app)?;
    if let Err(error) = fs::remove_dir_all(&home) {
        if error.kind() != std::io::ErrorKind::NotFound {
            return Err(format!("清理旧的安全模式目录失败：{error}"));
        }
    }
    fs::create_dir_all(&home).map_err(|error| format!("创建安全模式目录失败：{error}"))?;
    let real_home = dsh_home_for(&read_config(app)?);
    for name in ["settings.yaml", ".credentials.yaml"] {
        let from = real_home.join(name);
        if from.is_file() {
            let _ = fs::copy(&from, home.join(name));
        }
    }
    Ok(home)
}

/// 退出安全模式时删掉整个一次性目录。
fn cleanup_safe_mode_home(app: &AppHandle) {
    if let Ok(home) = safe_mode_home(app) {
        let _ = fs::remove_dir_all(home);
    }
}

/// 让系统分配一个空闲端口（安全模式专用：与正式服务互不干扰，可同时运行）。
fn free_port() -> Option<u16> {
    TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .ok()
        .and_then(|listener| listener.local_addr().ok())
        .map(|addr| addr.port())
}

fn wait_for_port_free(port: u16) -> bool {
    // 重启场景里旧进程刚被结束，给系统一点时间释放监听端口。
    for _ in 0..10 {
        match TcpListener::bind((Ipv4Addr::LOCALHOST, port)) {
            Ok(listener) => {
                drop(listener);
                return true;
            }
            Err(_) => thread::sleep(Duration::from_millis(200)),
        }
    }
    false
}

/// 端口上 Web 服务的根路径状态码；连不上或不是 HTTP 时返回 None。
///
/// 401 是 dsh 自 0.1.2 起加的浏览器认证：服务本身活着，但要求先用启动时
/// 打印的带令牌地址换一次 cookie，不带凭证的裸地址一律被拒。
fn http_root_status(port: u16) -> Option<u16> {
    let url = format!("http://127.0.0.1:{port}/");
    match ureq::AgentBuilder::new()
        .timeout(Duration::from_millis(1500))
        .build()
        .get(&url)
        .call()
    {
        Ok(response) => Some(response.status()),
        Err(ureq::Error::Status(code, _)) => Some(code),
        Err(ureq::Error::Transport(_)) => None,
    }
}

fn http_service_alive(port: u16) -> bool {
    // 端口有监听者时，用一次真实的 HTTP 往返确认对面是活着的 Web 服务，
    // 而不是残留的半死进程或非 HTTP 程序；任何状态码都算有响应。
    http_root_status(port).is_some()
}

fn attach_external(
    app: AppHandle,
    state: AppState,
    generation: u64,
    port: u16,
) -> Result<LauncherStatus, String> {
    // 外部启动的 dsh 不会把带令牌的那行输出交给我们。要不要请用户动手，取决于
    // 两件事同时成立：服务端要认证，且这个 WebView 手里还没有它的 Cookie
    // （之前认过的 Cookie 默认能用 30 天，那种情况直接进，不该弹提示）。
    let auth_required = http_root_status(port) == Some(401) && !has_auth_cookie(&app, port);
    {
        let mut runtime = state.runtime.lock().map_err(|_| "启动器状态锁已损坏")?;
        if runtime.generation != generation {
            drop(runtime);
            return current_status(state);
        }
        runtime.phase = Phase::Ready;
        runtime.consecutive_failures = 0;
        runtime.external = true;
        runtime.pid = None;
        runtime.web_url = None;
        runtime.auth_required = auth_required;
        runtime.auth_satisfied = !auth_required;
        runtime.message = if auth_required {
            format!(
                "已连接到端口 {port} 上的 dsh：它的 Web 界面需要认证，粘贴 dsh 启动时打印的那行带 token 的地址即可（一次认证默认管 30 天）"
            )
        } else {
            format!("已连接到端口 {port} 上已在运行的 dsh 服务（外部启动，Launcher 不会停止它）")
        };
        runtime.logs.push_back(if auth_required {
            format!("[launcher] 端口 {port} 上的 dsh 要求浏览器认证，需要带 token 的地址才能进入")
        } else {
            "[launcher] 检测到端口已有 Web 服务在运行，直接沿用外部 dsh".into()
        });
    }
    emit_status(&app, &state);
    let monitor_state = state.clone();
    thread::spawn(move || monitor_external(app, monitor_state, generation, port));
    current_status(state)
}

fn monitor_external(app: AppHandle, state: AppState, generation: u64, port: u16) {
    // 外部服务不归 Launcher 管，但它退出后要把界面从“运行中”翻回可启动状态，
    // 不能让用户对着一个连不上的 iframe。连续三次探测失败才算真的没了。
    let address = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port);
    let mut failures = 0;
    let mut ticks = 0u32;
    loop {
        thread::sleep(Duration::from_millis(900));
        match state.runtime.lock() {
            Ok(runtime) if runtime.generation == generation => {}
            _ => return,
        }
        if TcpStream::connect_timeout(&address, Duration::from_millis(400)).is_ok() {
            failures = 0;
            // 认证开关是服务端的事，隔一阵复查一次就够（约每 9 秒）。
            ticks = ticks.wrapping_add(1);
            if ticks % 10 == 0 {
                refresh_auth_required(&app, &state, generation, port);
            }
            continue;
        }
        failures += 1;
        if failures < 3 {
            continue;
        }
        if let Ok(mut runtime) = state.runtime.lock() {
            if runtime.generation != generation {
                return;
            }
            runtime.phase = Phase::Stopped;
            runtime.external = false;
            runtime.web_url = None;
            runtime.auth_required = false;
            runtime.auth_satisfied = false;
            runtime.message = "外部 dsh 服务已停止，可以在这里重新启动".into();
        }
        emit_status(&app, &state);
        return;
    }
}

/// 复查此刻是否真的需要用户来认证。
///
/// 只看服务端状态码是不够的：启动器后台的探测请求不带 WebView 的 Cookie，
/// 只要 dsh 开了认证，它看到的永远是 401，提示条就永远挂着——即使页面早就
/// 正常打开了。真正的凭证在 WebView 的 Cookie 里，所以这里先问 WebView 有没有
/// 这个端口的认证 Cookie，有就当已认证，只有「服务端要认证 + WebView 没凭证」
/// 才需要用户动手。
fn refresh_auth_required(app: &AppHandle, state: &AppState, generation: u64, port: u16) {
    let required = http_root_status(port) == Some(401) && !has_auth_cookie(app, port);
    let mut changed = false;
    if let Ok(mut runtime) = state.runtime.lock() {
        if runtime.generation != generation {
            return;
        }
        if runtime.auth_required != required {
            runtime.auth_required = required;
            changed = true;
        }
        // Cookie 在手就意味着这个 WebView 已经认过，别再提示。
        if !required && !runtime.auth_satisfied {
            runtime.auth_satisfied = true;
            changed = true;
        }
    }
    if changed {
        emit_status(app, state);
    }
}

/// WebView 里是否已经有这个端口的 dsh 认证 Cookie。
///
/// dsh 的 Cookie 名字是 `dsh-auth-<把 host:port 哈希过的串>`，名字里认不出端口，
/// 所以按 URL 查（Cookie 本身是 host-only 且带端口绑定，查到即对应这个服务）。
/// 读不到 WebView（窗口还没建好等）时按“没有凭证”处理，宁可多提示一次。
fn has_auth_cookie(app: &AppHandle, port: u16) -> bool {
    let Ok(url) = format!("http://localhost:{port}/").parse::<tauri::Url>() else {
        return false;
    };
    // tauri-runtime-wry 的 cookies_for_url 在事件循环没应答时不是返回 Err 而是
    // 直接 panic（lib.rs 里的 rx.recv().unwrap()）。启动早期或窗口半死时查
    // cookie 会踩中，把整个 start 任务一起带走——界面卡在“正在启动”就是它。
    // catch_unwind 兜住：拿不到就当没有，宁可多提示一次认证。
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        app.webview_windows().values().any(|window| {
            window
                .cookies_for_url(url.clone())
                .map(|cookies| {
                    cookies
                        .iter()
                        .any(|cookie| cookie.name().starts_with("dsh-auth-"))
                })
                .unwrap_or(false)
        })
    }))
    .unwrap_or(false)
}

/// `safe_mode = true` 时用一次性隔离 DSH_HOME 启动：正式 ~/.dsh 原封不动，
/// 插件与 profile 一概不加载，只尽力带上模型/API 钥匙，让安全会话能调 AI 修东西。
fn start_process_with(
    app: AppHandle,
    state: AppState,
    safe_mode: bool,
) -> Result<LauncherStatus, String> {
    let mut config = read_config(&app)?;
    validate_config(&config)?;
    // 安全模式自己找一个空闲端口：正式服务（哪怕坏着）继续占着配置端口
    // 也不影响，两边可以同时运行，互不干扰。
    if safe_mode {
        config.port = free_port().ok_or("没有可用端口，无法进入安全模式")?;
    }
    // 先把一次性目录准备好（失败早退，不动运行状态）。
    let safe_home = if safe_mode {
        Some(reset_safe_mode_home(&app)?)
    } else {
        None
    };
    let generation;
    {
        let mut runtime = state.runtime.lock().map_err(|_| "启动器状态锁已损坏")?;
        if matches!(
            runtime.phase,
            Phase::Starting | Phase::Ready | Phase::Stopping
        ) {
            return Ok(snapshot(&runtime));
        }
        runtime.generation += 1;
        generation = runtime.generation;
        runtime.phase = Phase::Starting;
        runtime.external = false;
        runtime.safe_mode = safe_mode;
        runtime.message = if safe_mode {
            "正在以安全模式启动 dsh（一次性隔离环境）…".into()
        } else {
            "正在启动 dsh web…".into()
        };
        runtime.url = format!("http://127.0.0.1:{}", config.port);
        runtime.web_url = None;
        runtime.auth_required = false;
        runtime.auth_satisfied = false;
        runtime.logs.clear();
    }
    emit_status(&app, &state);

    // 端口被占用不再直接报错：先探测占用者是否是可用的 Web 服务（多半是用户
    // 提前手动启动的 dsh web）。是就直接沿用——不登记子进程、不接管、退出时
    // 也不结束它；不是（比如刚结束的进程还没释放端口）才照旧等待释放。
    let port_free = match TcpListener::bind((Ipv4Addr::LOCALHOST, config.port)) {
        Ok(listener) => {
            drop(listener);
            true
        }
        Err(_) => false,
    };
    if !port_free {
        if http_service_alive(config.port) {
            return attach_external(app, state, generation, config.port);
        }
        if !wait_for_port_free(config.port) {
            return Err(format!(
                "端口 {} 已被占用，且占用者不像一个可用的 Web 服务。请结束占用进程，或在“管理”里换一个端口。",
                config.port
            ));
        }
    }

    let mut command = command_for_config(&config)?;
    command
        .current_dir(&config.working_directory)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(home) = &safe_home {
        // 安全模式：一次性目录优先于配置里的 dsh_home。
        command.env("DSH_HOME", home);
    } else if !config.dsh_home.trim().is_empty() {
        command.env("DSH_HOME", config.dsh_home.trim());
    }
    #[cfg(unix)]
    {
        // 独立进程组：停止时可以对整组发信号，不会留下孤儿 node 进程。
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let mut child = command.spawn().map_err(|error| {
        if matches!(config.launch_mode, LaunchMode::Command) {
            format!("无法启动 dsh：{error}。请确认已执行 npm install -g @deepseek-ai/dsh，或切换为 npx。")
        } else {
            format!("无法启动 npx：{error}。请确认 Node.js 已安装并在 PATH 中。")
        }
    })?;
    let pid = child.id();
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    {
        let mut runtime = state.runtime.lock().map_err(|_| "启动器状态锁已损坏")?;
        if runtime.generation != generation {
            // 启动期间被 stop 抢先（generation 已推进）：不登记这个子进程，直接收尾。
            drop(runtime);
            kill_child_tree(child);
            return current_status(state);
        }
        runtime.pid = Some(pid);
        runtime.child = Some(child);
        runtime.message = format!("等待 {} 响应…", runtime.url);
    }

    if let Some(stdout) = stdout {
        spawn_log_reader(app.clone(), state.clone(), generation, "stdout", stdout);
    }
    if let Some(stderr) = stderr {
        spawn_log_reader(app.clone(), state.clone(), generation, "stderr", stderr);
    }

    let startup_timeout = match config.launch_mode {
        // npx 首次运行可能要现场下载整个包。
        LaunchMode::Npx => Duration::from_secs(180),
        LaunchMode::Command => Duration::from_secs(60),
    };
    let monitor_app = app.clone();
    let monitor_state = state.clone();
    let port = config.port;
    thread::spawn(move || {
        monitor_process(
            monitor_app,
            monitor_state,
            generation,
            port,
            startup_timeout,
        )
    });
    emit_status(&app, &state);
    current_status(state)
}

/// 一直有输出但始终不监听端口时的总时长上限。升级后首次启动要现装
/// `~/.dsh/profiles/web` 的依赖，慢是正常的，但不能无限等下去。
const STARTUP_HARD_LIMIT: Duration = Duration::from_secs(900);

/// 启动超过这么久还没就绪就把“可能在装依赖”的提示写进状态栏。
const SLOW_START_HINT_AFTER: Duration = Duration::from_secs(45);

/// 启动守护对当前等待状态的判断。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StartupWait {
    /// 还在合理范围内，继续等。
    Keep,
    /// 太久没有任何输出，判为卡死。
    Silent,
    /// 一直在输出却始终不监听端口，超过总时长上限。
    Exhausted,
}

/// 用“静默了多久”而不是“总共等了多久”判断启动是否卡死。
///
/// dsh 发生破坏性升级（如 0.1.0-rc.8）后首次 `dsh web` 会现装 profile 依赖，
/// 好几分钟不监听端口但一直在刷安装日志；按固定 60 秒总时长判超时会把它
/// 强杀掉，还可能留下半装好的 profile，导致之后每次启动都失败 —— 这就是
/// issue #5 里“手动更新后貌似也无法启动了”。只要还有新输出就不算卡死，
/// 另用 STARTUP_HARD_LIMIT 兜底。
fn classify_startup_wait(
    silent_for: Duration,
    elapsed: Duration,
    silence_budget: Duration,
) -> StartupWait {
    if silent_for > silence_budget {
        StartupWait::Silent
    } else if elapsed > STARTUP_HARD_LIMIT {
        StartupWait::Exhausted
    } else {
        StartupWait::Keep
    }
}

fn monitor_process(
    app: AppHandle,
    state: AppState,
    generation: u64,
    port: u16,
    startup_timeout: Duration,
) {
    let started = Instant::now();
    let address = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port);
    let mut ready = false;
    // 最近一次看到子进程有新输出的时刻，以及当时的输出行数。
    let mut last_active = Instant::now();
    let mut seen_ticks = 0u64;
    // “还在装依赖”的提示只播一次。
    let mut hinted = false;
    loop {
        thread::sleep(Duration::from_millis(if ready { 800 } else { 250 }));
        let exited = {
            let mut runtime = match state.runtime.lock() {
                Ok(runtime) => runtime,
                Err(_) => return,
            };
            if runtime.generation != generation {
                return;
            }
            if runtime.log_ticks != seen_ticks {
                seen_ticks = runtime.log_ticks;
                last_active = Instant::now();
            }
            match runtime
                .child
                .as_mut()
                .and_then(|child| child.try_wait().ok())
                .flatten()
            {
                Some(exit) => {
                    runtime.child = None;
                    runtime.pid = None;
                    if runtime.phase == Phase::Stopping {
                        runtime.phase = Phase::Stopped;
                        runtime.message = "dsh 已停止".into();
                    } else {
                        runtime.phase = Phase::Failed;
                        runtime.consecutive_failures += 1;
                        let detail = runtime
                            .logs
                            .iter()
                            .rev()
                            .find(|line| !line.trim().is_empty())
                            .map(|line| truncate_chars(line, 240));
                        let prefix = if ready {
                            "dsh 进程意外退出"
                        } else {
                            "dsh 进程已退出"
                        };
                        runtime.message = match detail {
                            Some(detail) => format!("{prefix}（{exit}）：{detail}"),
                            None => format!("{prefix}（{exit}）"),
                        };
                    }
                    true
                }
                None => false,
            }
        };
        if exited {
            emit_status(&app, &state);
            return;
        }
        if ready {
            // 就绪后继续守护子进程，崩溃时把状态从“运行中”翻成失败，
            // 而不是留着一个连不上的 iframe。
            continue;
        }
        if TcpStream::connect_timeout(&address, Duration::from_millis(180)).is_ok() {
            if let Ok(mut runtime) = state.runtime.lock() {
                if runtime.generation == generation && runtime.phase == Phase::Starting {
                    runtime.phase = Phase::Ready;
                    runtime.consecutive_failures = 0;
                    runtime.message = format!("正在监听 {}", runtime.url);
                }
            }
            // 没能从启动输出里认到带令牌地址时（例如 dsh 关掉了 printUrl），
            // 这一次探测至少能让界面提示用户手动粘贴。
            refresh_auth_required(&app, &state, generation, port);
            emit_status(&app, &state);
            ready = true;
            continue;
        }
        let verdict =
            classify_startup_wait(last_active.elapsed(), started.elapsed(), startup_timeout);
        if verdict == StartupWait::Keep {
            // 起得慢又还在刷日志时告诉用户在等什么，否则界面只写“等待响应”，
            // 用户会以为卡死了（升级后首次启动装依赖要好几分钟）。
            if !hinted && started.elapsed() > SLOW_START_HINT_AFTER {
                hinted = true;
                if let Ok(mut runtime) = state.runtime.lock() {
                    if runtime.generation != generation || runtime.phase != Phase::Starting {
                        return;
                    }
                    runtime.message = format!(
                        "dsh 仍在启动，还在输出日志（升级后首次启动要现装依赖，可能要几分钟）：等待 {} 响应…",
                        runtime.url
                    );
                }
                emit_status(&app, &state);
            }
            continue;
        }
        {
            // 超时后必须终止子进程；否则它继续占着端口，下次启动会把它的
            // Child 悄悄丢掉，留下一个失控的孤儿进程。
            let child = {
                let mut runtime = match state.runtime.lock() {
                    Ok(runtime) => runtime,
                    Err(_) => return,
                };
                if runtime.generation != generation {
                    return;
                }
                runtime.phase = Phase::Failed;
                runtime.consecutive_failures += 1;
                runtime.message = match verdict {
                    StartupWait::Silent => format!(
                        "dsh 启动超时（{} 秒内既没有监听端口 {port}，也没有任何新输出），已终止进程，请检查运行日志",
                        startup_timeout.as_secs()
                    ),
                    _ => format!(
                        "dsh 启动超过 {} 分钟仍未监听端口 {port}，已终止进程，请检查运行日志",
                        STARTUP_HARD_LIMIT.as_secs() / 60
                    ),
                };
                runtime.pid = None;
                runtime.child.take()
            };
            if let Some(child) = child {
                kill_child_tree(child);
            }
            emit_status(&app, &state);
            return;
        }
    }
}

pub fn stop_process(app: &AppHandle, state: &AppState) -> Result<LauncherStatus, String> {
    let (child, status) = {
        let mut runtime = state.runtime.lock().map_err(|_| "启动器状态锁已损坏")?;
        runtime.generation += 1;
        runtime.pid = None;
        runtime.web_url = None;
        runtime.auth_required = false;
        runtime.auth_satisfied = false;
        match runtime.child.take() {
            Some(child) => {
                runtime.phase = Phase::Stopping;
                runtime.message = "正在停止 dsh…".into();
                (Some(child), snapshot(&runtime))
            }
            None => {
                // 没有登记过子进程：要么本来就没启动，要么沿用的是外部服务。
                // 外部服务只“断开”，绝不结束一个不是我们启动的进程。
                let was_external = runtime.external;
                runtime.external = false;
                runtime.phase = Phase::Stopped;
                runtime.message = if was_external {
                    "已断开与外部 dsh 服务的连接（该服务仍在运行）".into()
                } else {
                    "dsh 尚未启动".into()
                };
                (None, snapshot(&runtime))
            }
        }
    };
    emit_status(app, state);
    let Some(child) = child else {
        return Ok(status);
    };

    kill_child_tree(child);
    let mut runtime = state.runtime.lock().map_err(|_| "启动器状态锁已损坏")?;
    runtime.phase = Phase::Stopped;
    runtime.message = "dsh 已停止".into();
    let status = snapshot(&runtime);
    drop(runtime);
    emit_status(app, state);
    Ok(status)
}

pub fn start_with_feedback(app: &AppHandle, state: &AppState) -> Result<LauncherStatus, String> {
    start_with_feedback_mode(app, state, false)
}

pub fn start_with_feedback_mode(
    app: &AppHandle,
    state: &AppState,
    safe_mode: bool,
) -> Result<LauncherStatus, String> {
    let result = start_process_with(app.clone(), state.clone(), safe_mode);
    if let Err(error) = &result {
        if let Ok(mut runtime) = state.runtime.lock() {
            runtime.phase = Phase::Failed;
            runtime.consecutive_failures += 1;
            runtime.message = error.clone();
        }
        // 广播失败状态，否则界面会一直停在“正在启动”。
        emit_status(app, state);
    }
    result
}

/// 进入安全模式：先停当前服务（若有），再以一次性隔离环境启动。
/// 托盘菜单与界面按钮共用这个入口。
pub fn enter_safe_mode(app: &AppHandle, state: &AppState) -> Result<LauncherStatus, String> {
    let _ = stop_process(app, state);
    start_with_feedback_mode(app, state, true)
}

// 涉及子进程、网络或窗口创建的命令必须是 async：Tauri 2 的同步命令在主线程
// 上执行，任何阻塞都会冻结所有窗口的事件循环（拖拽、缩放、关闭全部失效）。
#[tauri::command]
pub async fn start_dsh(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<LauncherStatus, String> {
    // 防呆：插件/更新操作会先停服务再改写安装目录，期间手动启动会让 dsh
    // 跑在改到一半的目录上；操作结束后会自动恢复服务，这里直接拒绝。
    // 持有守卫到启动完成，同样挡住反过来的竞争（启动进行中来了插件操作会排队）。
    let Ok(_ops) = state.ops.try_lock() else {
        return Err(ops_in_progress_error(state.inner()));
    };
    start_with_feedback(&app, state.inner())
}

#[tauri::command]
pub async fn stop_dsh(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<LauncherStatus, String> {
    stop_process(&app, state.inner())
}

#[tauri::command]
pub async fn restart_dsh(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<LauncherStatus, String> {
    let Ok(_ops) = state.ops.try_lock() else {
        return Err(ops_in_progress_error(state.inner()));
    };
    stop_process(&app, state.inner())?;
    start_with_feedback(&app, state.inner())
}

/// 进入安全模式：停掉当前服务，用一次性隔离 DSH_HOME 重新启动。
#[tauri::command]
pub async fn start_safe_mode(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<LauncherStatus, String> {
    let Ok(_ops) = state.ops.try_lock() else {
        return Err(ops_in_progress_error(state.inner()));
    };
    enter_safe_mode(&app, state.inner())
}

/// 退出安全模式：停服务、删一次性目录，回到正式环境正常启动。
#[tauri::command]
pub async fn exit_safe_mode(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<LauncherStatus, String> {
    let Ok(_ops) = state.ops.try_lock() else {
        return Err(ops_in_progress_error(state.inner()));
    };
    let _ = stop_process(&app, state.inner());
    cleanup_safe_mode_home(&app);
    start_with_feedback_mode(&app, state.inner(), false)
}

/// 用户从终端粘进来的认证地址：内嵌页面下一次装载会用它换 cookie。
///
/// 只接受指向本机回环、端口与当前设置一致、且带 token 的地址——dsh 启动时
/// 打印的就是这个形状。校验留在后端，前端不必懂认证细节。
#[tauri::command]
pub fn submit_auth_url(
    app: AppHandle,
    state: State<'_, AppState>,
    url: String,
) -> Result<LauncherStatus, String> {
    let config = read_config(&app)?;
    let cleaned = validate_auth_url(url.trim(), config.port)?;
    {
        let mut runtime = state.runtime.lock().map_err(|_| "启动器状态锁已损坏")?;
        runtime.web_url = Some(cleaned);
        runtime.auth_satisfied = false;
    }
    emit_status(&app, state.inner());
    current_status(state.inner().clone())
}

/// 校验并规范化用户粘贴的认证地址。
fn validate_auth_url(value: &str, port: u16) -> Result<String, String> {
    let rest = value
        .strip_prefix("http://")
        .ok_or_else(|| "认证地址要以 http:// 开头，请整行复制 dsh 打印的那条地址".to_string())?;
    let (authority, path) = rest.split_once('/').unwrap_or((rest, ""));
    let (host, raw_port) = authority
        .rsplit_once(':')
        .ok_or_else(|| "认证地址缺少端口，请复制 dsh 打印的完整地址".to_string())?;
    let url_port = raw_port
        .parse::<u16>()
        .map_err(|_| "认证地址的端口无效".to_string())?;
    if !matches!(host, "127.0.0.1" | "localhost" | "[::1]") {
        return Err("认证地址必须指向本机（127.0.0.1 或 localhost）".into());
    }
    if url_port != port {
        return Err(format!(
            "认证地址的端口是 {url_port}，与当前设置里的 {port} 不一致"
        ));
    }
    let query = path.split_once('?').map_or("", |(_, query)| query);
    let has_token = query
        .split('&')
        .any(|pair| pair.strip_prefix("token=").is_some_and(|token| !token.is_empty()));
    if !has_token {
        return Err("地址里没有 token 参数，请复制 dsh 启动时打印的那一整行".into());
    }
    Ok(format!("http://{authority}/{path}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    #[test]
    fn detects_live_http_service_on_busy_port() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind ephemeral port");
        let port = listener.local_addr().expect("local addr").port();
        let server = thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                use std::io::Write;
                let mut buffer = [0u8; 1024];
                let _ = Read::read(&mut stream, &mut buffer);
                let _ = stream.write_all(
                    b"HTTP/1.1 200 OK\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
                );
            }
        });
        assert!(http_service_alive(port));
        let _ = server.join();
    }

    #[test]
    fn treats_free_port_as_no_service() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind ephemeral port");
        let port = listener.local_addr().expect("local addr").port();
        drop(listener);
        assert!(!http_service_alive(port));
    }

    #[test]
    fn ignores_listeners_that_close_without_responding() {
        // 模拟“端口被占但不是 Web 服务”：接受连接后立即断开。
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind ephemeral port");
        let port = listener.local_addr().expect("local addr").port();
        let server = thread::spawn(move || {
            let _ = listener.accept();
        });
        assert!(!http_service_alive(port));
        let _ = server.join();
    }

    #[test]
    fn keeps_waiting_while_dsh_is_still_logging() {
        let budget = Duration::from_secs(60);
        // 升级后首次启动：五分钟没监听端口，但一直有新输出 —— 不能判超时。
        assert_eq!(
            classify_startup_wait(Duration::from_secs(3), Duration::from_secs(300), budget),
            StartupWait::Keep
        );
    }

    #[test]
    fn fails_when_dsh_goes_quiet_without_listening() {
        let budget = Duration::from_secs(60);
        assert_eq!(
            classify_startup_wait(Duration::from_secs(61), Duration::from_secs(61), budget),
            StartupWait::Silent
        );
    }

    #[test]
    fn stops_waiting_after_the_hard_limit_even_if_still_logging() {
        let budget = Duration::from_secs(60);
        assert_eq!(
            classify_startup_wait(
                Duration::from_secs(1),
                STARTUP_HARD_LIMIT + Duration::from_secs(1),
                budget
            ),
            StartupWait::Exhausted
        );
    }

    #[test]
    fn builds_web_args_with_no_open_for_the_embedded_page() {
        // 只检查参数拼装，不真正执行 dsh。
        let config = LauncherConfig {
            port: 3081,
            trusted_hosts: vec!["example.test".into()],
            ..Default::default()
        };
        let args = web_args(&config);
        assert_eq!(
            args,
            vec![
                "web",
                "--host",
                "127.0.0.1",
                "--port",
                "3081",
                "--no-open",
                "--trusted-host",
                "example.test",
            ]
        );
        // Launcher 内嵌页面，必须抑制 dsh 自己弹的系统浏览器窗口。
        assert!(args.contains(&"--no-open".to_string()));
    }

    #[test]
    fn accepts_the_authentication_address_dsh_prints() {
        let url = "http://127.0.0.1:3080/?token=abc123";
        assert_eq!(validate_auth_url(url, 3080).expect("valid"), url);
    }

    #[test]
    fn refuses_authentication_addresses_that_are_not_our_local_service() {
        // 少了 token：这是会被 401 拒绝的裸地址。
        assert!(validate_auth_url("http://127.0.0.1:3080/", 3080).is_err());
        // 不是本机。
        assert!(validate_auth_url("http://example.com:3080/?token=x", 3080).is_err());
        // 端口对不上当前设置。
        assert!(validate_auth_url("http://127.0.0.1:9/?token=x", 3080).is_err());
        // 只认 dsh 直接提供的 http 回环地址。
        assert!(validate_auth_url("https://127.0.0.1:3080/?token=x", 3080).is_err());
        // 空的 token 不算。
        assert!(validate_auth_url("http://127.0.0.1:3080/?token=", 3080).is_err());
    }
}
