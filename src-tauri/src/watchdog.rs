//! WebView2 子进程看门狗：窗口偶发假死时渲染进程会持续吃满一个核，
//! 但冷静期抓不到现场。这里后台每 5 秒扫一遍本应用名下的 msedgewebview2
//! 进程树，某个进程连续 20 秒 CPU 超过 0.6 核就把它降到 BELOW_NORMAL
//! 优先级（dsh iframe 与壳子同站，WebView2 不支持 iframe 级进程隔离，
//! 两者共用一个渲染进程，不加限制时壳子的输入与绘制会被一起挤死），
//! 冷却后恢复 NORMAL；进程信息连同启动器状态同时追加到 watchdog 日志。

use std::{
    collections::HashMap,
    fs::{self, OpenOptions},
    io::Write,
    path::PathBuf,
    sync::atomic::{AtomicI64, Ordering as AtomicOrdering},
    thread,
    time::{Duration, Instant},
};

use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};
use tauri::{AppHandle, Manager};

use crate::state::{current_status, AppState};
#[cfg(target_os = "windows")]
use crate::windows_ui::sync_webview_suspension;

/// 采样间隔；sysinfo 的 cpu_usage 是两次刷新之间的均值。
const POLL: Duration = Duration::from_secs(5);
/// 连续超过阈值多久算“持续高 CPU”（4 次 × 5 秒）。
const HOT_STREAK: u32 = 4;
/// 单核百分比阈值（sysinfo 里 100 = 吃满一个核）。
const CPU_LIMIT: f32 = 60.0;
/// 热进程重复记录的最小间隔，避免日志被刷爆。
const RELOG_EVERY: Duration = Duration::from_secs(60);
/// 日志上限，超过就轮转成 .old。
const LOG_LIMIT: u64 = 512 * 1024;

/// 主线程心跳（Unix 秒）。看门狗每轮向主线程投递一个任务，任务执行时
/// 更新时间戳；主线程卡死时时间戳停止推进，日志里立刻能分辨
/// 「主线程卡死」还是「进程已死」。投递本身不等待，看门狗不会被连带冻住。
static MAIN_THREAD_HEARTBEAT: AtomicI64 = AtomicI64::new(0);

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

fn log_path(app: &AppHandle) -> Option<PathBuf> {
    app.path()
        .app_config_dir()
        .ok()
        .map(|dir| dir.join("webview-watchdog.log"))
}

pub(crate) fn append_log(app: &AppHandle, line: &str) {
    let Some(path) = log_path(app) else { return };
    if let Ok(metadata) = fs::metadata(&path) {
        if metadata.len() > LOG_LIMIT {
            let old = path.with_extension("log.old");
            let _ = fs::rename(&path, old);
        }
    }
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(&path) {
        let _ = writeln!(file, "{} {}", timestamp(), line);
    }
}

/// 本地时间（UTC+8）的紧凑时间戳；chrono 不在依赖树里，手写转换。
fn timestamp() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        + 8 * 3600;
    let days = secs / 86_400;
    let tod = secs % 86_400;
    // civil-from-days（Howard Hinnant 算法），把 Unix 天数转成 Y/M/D。
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}+08:00",
        y,
        m,
        d,
        tod / 3600,
        tod % 3600 / 60,
        tod % 60
    )
}

pub fn start(app: AppHandle) {
    thread::spawn(move || {
        let me = Pid::from_u32(std::process::id());
        let mut system = System::new();
        // pid -> 连续超阈值次数；上次记录时间按 pid 记。
        let mut streaks: HashMap<u32, u32> = HashMap::new();
        let mut last_report: HashMap<u32, Instant> = HashMap::new();
        // 已被降级的渲染进程；冷却或退出后恢复正常优先级。
        let mut demoted: Vec<u32> = Vec::new();
        append_log(&app, &format!("watchdog 启动，launcher pid={}", me));
        let mut last_hang_log: Option<Instant> = None;
        loop {
            thread::sleep(POLL);
            // Windows：把渲染开关对齐任务投递到主线程执行。绝不能在后台
            // 线程直接调 is_visible()——它向主线程发消息并无限期等待，
            // 主线程被 WebView2 同步调用拖住时，看门狗会一起被冻死，
            // 黑匣子恰恰在最需要它的时刻失明（此前复现过 22 分钟无日志）。
            #[cfg(target_os = "windows")]
            {
                let app_handle = app.clone();
                let _ = app.run_on_main_thread(move || {
                    MAIN_THREAD_HEARTBEAT.store(now_secs(), AtomicOrdering::Relaxed);
                    sync_webview_suspension(&app_handle);
                });
                // 主线程心跳超过 30 秒没推进 = 主线程卡死（热键/托盘/窗口
                // 全会失灵，其他程序也切不了前台）。1 分钟最多记一条。
                let last = MAIN_THREAD_HEARTBEAT.load(AtomicOrdering::Relaxed);
                let now = now_secs();
                if last > 0
                    && now - last > 30
                    && last_hang_log.is_none_or(|t| t.elapsed() >= Duration::from_secs(60))
                {
                    append_log(&app, &format!("主线程疑似卡死：心跳已 {} 秒未推进", now - last));
                    last_hang_log = Some(Instant::now());
                }
            }
            // 默认刷新不包含命令行；渲染进程识别依赖 --type=renderer，
            // 必须显式要求刷新 cmd，否则 cmd() 恒为空、降级永远不触发。
            system.refresh_processes_specifics(
                ProcessesToUpdate::All,
                true,
                ProcessRefreshKind::nothing()
                    .with_memory()
                    .with_cpu()
                    .with_cmd(UpdateKind::Always),
            );
            // WebView2 broker 的父进程是启动器本体；顺着父子链收出整棵
            // msedgewebview2 树，node 等其他子进程天然排除。
            let brokers: Vec<Pid> = system
                .processes()
                .values()
                .filter(|p| {
                    p.parent() == Some(me)
                        && p.name().to_string_lossy().eq_ignore_ascii_case("msedgewebview2.exe")
                })
                .map(|p| p.pid())
                .collect();
            if brokers.is_empty() {
                continue;
            }
            let mut tree: Vec<Pid> = brokers.clone();
            loop {
                let parents = tree.clone();
                let children: Vec<Pid> = system
                    .processes()
                    .values()
                    .filter(|p| {
                        p.parent().is_some_and(|parent| parents.contains(&parent))
                            && p.name().to_string_lossy().eq_ignore_ascii_case("msedgewebview2.exe")
                            && !tree.contains(&p.pid())
                    })
                    .map(|p| p.pid())
                    .collect();
                if children.is_empty() {
                    break;
                }
                tree.extend(children);
            }
            let mut hottest: Option<(Pid, f32)> = None;
            for pid in &tree {
                let Some(process) = system.process(*pid) else { continue };
                let cpu = process.cpu_usage();
                if hottest.is_none_or(|(_, top)| cpu > top) {
                    hottest = Some((*pid, cpu));
                }
            }
            let Some((pid, cpu)) = hottest else { continue };
            let pid32 = pid.as_u32();
            let hottest_is_renderer = system.process(pid).is_some_and(|p| {
                p.cmd().iter().any(|arg| arg.to_string_lossy().contains("--type=renderer"))
            });
            if cpu >= CPU_LIMIT {
                let streak = streaks.entry(pid32).or_insert(0);
                *streak += 1;
                if *streak == HOT_STREAK {
                    report(&app, &system, pid, cpu, &tree, "持续高 CPU 开始");
                    last_report.insert(pid32, Instant::now());
                    // 连续 20 秒高 CPU 的渲染进程降到 BELOW_NORMAL，让它
                    // 只能吃剩余 CPU，壳子的输入、绘制与系统前台切换不再
                    // 被挤死；冷却后自动恢复（见下方 retain）。
                    if !hottest_is_renderer {
                        append_log(&app, &format!("持续高 CPU 非渲染进程，不降级: pid={pid32}"));
                    } else if set_process_priority(pid32, true) {
                        demoted.push(pid32);
                        append_log(
                            &app,
                            &format!(
                                "已降级优先级: pid={pid32} cpu={cpu:.0}%/核 → BELOW_NORMAL"
                            ),
                        );
                    } else {
                        append_log(
                            &app,
                            &format!("降级失败（OpenProcess/SetPriorityClass）: pid={pid32}"),
                        );
                    }
                }
                if *streak > HOT_STREAK {
                    let fresh = last_report
                        .get(&pid32)
                        .is_none_or(|t| t.elapsed() >= RELOG_EVERY);
                    if fresh {
                        report(&app, &system, pid, cpu, &tree, "持续高 CPU 持续中");
                        last_report.insert(pid32, Instant::now());
                    }
                }
            } else if streaks.remove(&pid32).is_some_and(|streak| streak >= HOT_STREAK) {
                report(&app, &system, pid, cpu, &tree, "持续高 CPU 结束");
                last_report.remove(&pid32);
            }
            demoted.retain(|&hot_pid| match system.process(Pid::from_u32(hot_pid)) {
                Some(process) if process.cpu_usage() >= CPU_LIMIT => true,
                Some(_) => {
                    set_process_priority(hot_pid, false);
                    append_log(&app, &format!("优先级已恢复: pid={hot_pid}"));
                    false
                }
                None => {
                    append_log(&app, &format!("优先级恢复跳过（进程已退出）: pid={hot_pid}"));
                    false
                }
            });
        }
    });
}

fn report(
    app: &AppHandle,
    system: &System,
    pid: Pid,
    cpu: f32,
    tree: &[Pid],
    verdict: &str,
) {
    let mut parts: Vec<String> = Vec::new();
    for member in tree {
        if let Some(process) = system.process(*member) {
            parts.push(format!(
                "{}={}m {:.0}%",
                member.as_u32(),
                process.memory() / 1024 / 1024,
                process.cpu_usage()
            ));
        }
    }
    let status = app
        .try_state::<AppState>()
        .map(|state| current_status(state.inner().clone()).map(|s| s.message).unwrap_or_default())
        .unwrap_or_default();
    append_log(
        app,
        &format!(
            "{}: msedgewebview2(pid={pid}) cpu={cpu:.0}%/核，全树[{}]，状态消息「{status}」",
            verdict,
            parts.join(" ")
        ),
    );
}

/// 把进程优先级降到 BELOW_NORMAL / 恢复 NORMAL。
#[cfg(windows)]
fn set_process_priority(pid: u32, below_normal: bool) -> bool {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{
        OpenProcess, SetPriorityClass, BELOW_NORMAL_PRIORITY_CLASS, NORMAL_PRIORITY_CLASS,
        PROCESS_SET_INFORMATION,
    };
    unsafe {
        let handle = OpenProcess(PROCESS_SET_INFORMATION, 0, pid);
        if handle.is_null() {
            return false;
        }
        let class = if below_normal {
            BELOW_NORMAL_PRIORITY_CLASS
        } else {
            NORMAL_PRIORITY_CLASS
        };
        let ok = SetPriorityClass(handle, class) != 0;
        CloseHandle(handle);
        ok
    }
}

#[cfg(not(windows))]
fn set_process_priority(_pid: u32, _below_normal: bool) -> bool {
    false
}
