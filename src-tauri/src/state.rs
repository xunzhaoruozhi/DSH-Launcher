use serde::Serialize;
use std::{
    collections::{HashMap, VecDeque},
    io::{BufRead, BufReader, Read},
    process::Child,
    sync::{Arc, Mutex},
    thread,
};
use tauri::{AppHandle, Emitter, State};

use crate::market::MarketCatalog;

pub const MAX_LOG_LINES: usize = 400;

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Stopped,
    Starting,
    Ready,
    Stopping,
    Failed,
}

#[derive(Debug, Clone, Serialize)]
pub struct LauncherStatus {
    pub phase: Phase,
    pub message: String,
    /// 给人看的地址（状态栏与日志），永远是不带令牌的干净地址。
    pub url: String,
    /// 内嵌页面真正要装载的地址。dsh 自 0.1.2 起给 Web 加了浏览器认证，
    /// 只有启动时打印的那行 `dsh web: http://…/?token=…` 才换得到 cookie；
    /// 留 None 表示沿用 `url`。
    pub web_url: Option<String>,
    /// 端口上的服务拒绝无凭证访问（401）：需要把 dsh 启动时打印的地址粘进来。
    pub auth_required: bool,
    /// 内嵌页面已经完成过一次令牌交换（cookie 已落进 WebView）：不用再提示粘贴。
    pub auth_satisfied: bool,
    pub pid: Option<u32>,
    pub external: bool,
    /// 当前服务以安全模式运行：一次性隔离 DSH_HOME，正式数据未加载。
    pub safe_mode: bool,
    /// 连续启动失败次数（就绪后清零）。达到阈值时界面提供安全模式入口。
    pub consecutive_failures: u32,
    pub logs: Vec<String>,
    pub busy: Option<String>,
}

pub struct RuntimeState {
    pub phase: Phase,
    pub message: String,
    pub url: String,
    /// 带认证令牌的地址（自动从启动输出里认出来，或由用户从终端粘贴）。
    pub web_url: Option<String>,
    /// 当前端口上的服务要求浏览器认证：启动器自己拼的裸地址会被 401 拒绝。
    pub auth_required: bool,
    /// 本进程的 WebView 已经认证过：探测请求不带它的 cookie，只能靠这个标记
    /// 压住重复提示。
    pub auth_satisfied: bool,
    pub child: Option<Child>,
    pub pid: Option<u32>,
    // 端口上的服务由用户在 Launcher 之外启动：只沿用不接管，停止/退出都不碰它。
    pub external: bool,
    // 本次启动是否走安全模式（一次性隔离 DSH_HOME）。
    pub safe_mode: bool,
    // 连续启动失败计数：就绪清零、失败自增，界面据此亮出安全模式入口。
    pub consecutive_failures: u32,
    pub logs: VecDeque<String>,
    pub generation: u64,
    // 互斥操作（插件安装/卸载/更新、dsh 更新）进行中的描述文字。期间前端
    // 禁用启动入口，后端 start_dsh/restart_dsh 也会拒绝手动启动（防呆：
    // 这些操作会先停服务再改写安装目录，中途启动会跑在半成品目录上）。
    pub busy: Option<String>,
    // 收到过多少行子进程输出。启动守护用它判断 dsh 是否还在干活（升级后首次
    // 启动要现装 profile 依赖，几分钟不监听端口但一直在刷日志），只要还有新
    // 输出就不能当成卡死。
    pub log_ticks: u64,
}

impl Default for RuntimeState {
    fn default() -> Self {
        Self {
            phase: Phase::Stopped,
            message: "dsh 尚未启动".into(),
            url: "http://127.0.0.1:3080".into(),
            web_url: None,
            auth_required: false,
            auth_satisfied: false,
            child: None,
            pid: None,
            external: false,
            safe_mode: false,
            consecutive_failures: 0,
            logs: VecDeque::new(),
            generation: 0,
            busy: None,
            log_ticks: 0,
        }
    }
}

#[derive(Clone)]
pub struct AppState {
    pub runtime: Arc<Mutex<RuntimeState>>,
    pub market: Arc<Mutex<Option<MarketCatalog>>>,
    // 拖出标签新建窗口时，按窗口 label 暂存它要带走的标签标题。
    pub pending_tabs: Arc<Mutex<HashMap<String, String>>>,
    // 串行化会改写安装目录/profile 的互斥操作（插件安装/卸载/更新、dsh 更新），
    // 防止并发的 npm/dsh 进程互相破坏同一份 package.json 或服务停启状态。
    pub ops: Arc<Mutex<()>>,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            runtime: Arc::new(Mutex::new(RuntimeState::default())),
            market: Arc::new(Mutex::new(None)),
            pending_tabs: Arc::new(Mutex::new(HashMap::new())),
            ops: Arc::new(Mutex::new(())),
        }
    }
}

pub fn snapshot(runtime: &RuntimeState) -> LauncherStatus {
    LauncherStatus {
        phase: runtime.phase,
        message: runtime.message.clone(),
        url: runtime.url.clone(),
        web_url: runtime.web_url.clone(),
        auth_required: runtime.auth_required,
        auth_satisfied: runtime.auth_satisfied,
        pid: runtime.pid,
        external: runtime.external,
        safe_mode: runtime.safe_mode,
        consecutive_failures: runtime.consecutive_failures,
        logs: runtime.logs.iter().cloned().collect(),
        busy: runtime.busy.clone(),
    }
}

pub fn emit_status(app: &AppHandle, state: &AppState) {
    if let Ok(runtime) = state.runtime.lock() {
        let _ = app.emit("launcher-status", snapshot(&runtime));
    }
}

/// 从 dsh web 的启动输出里取出带认证令牌的地址。
///
/// dsh 自 0.1.2 起给 Web 加了浏览器认证：启动时打印
/// `dsh web: http://127.0.0.1:3080/?token=…`（同一行后面还可能跟一个
/// `(LAN: …)`，取第一个地址即可）。带令牌访问一次才会下发 cookie，不带
/// 令牌的裸地址一律 401。更早的版本不打印这行，返回 None。
pub fn extract_auth_url(line: &str) -> Option<String> {
    let url = line
        .trim()
        .strip_prefix("dsh web:")?
        .split_whitespace()
        .next()?;
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return None;
    }
    match url.split_once("?token=") {
        Some((_, token)) if !token.is_empty() => Some(url.to_string()),
        _ => None,
    }
}

pub fn push_log(app: &AppHandle, state: &AppState, generation: u64, source: &str, line: String) {
    if let Ok(mut runtime) = state.runtime.lock() {
        if runtime.generation != generation {
            return;
        }
        // 带令牌的地址只在这一行里出现一次，错过就只能让用户手动粘贴了。
        if let Some(url) = extract_auth_url(&line) {
            runtime.web_url = Some(url);
            runtime.auth_required = false;
        }
        runtime.logs.push_back(format!("[{source}] {line}"));
        runtime.log_ticks = runtime.log_ticks.wrapping_add(1);
        while runtime.logs.len() > MAX_LOG_LINES {
            runtime.logs.pop_front();
        }
    }
    emit_status(app, state);
}

pub fn spawn_log_reader<R>(
    app: AppHandle,
    state: AppState,
    generation: u64,
    source: &'static str,
    stream: R,
) where
    R: Read + Send + 'static,
{
    thread::spawn(move || {
        for line in BufReader::new(stream).lines().map_while(Result::ok) {
            push_log(&app, &state, generation, source, line);
        }
    });
}

pub fn current_status(state: AppState) -> Result<LauncherStatus, String> {
    state
        .runtime
        .lock()
        .map(|runtime| snapshot(&runtime))
        .map_err(|_| "启动器状态锁已损坏".into())
}

pub fn busy_label(state: &AppState) -> Option<String> {
    state
        .runtime
        .lock()
        .ok()
        .and_then(|runtime| runtime.busy.clone())
}

/// 手动启动 dsh 被互斥操作挡下时给用户的解释。
pub fn ops_in_progress_error(state: &AppState) -> String {
    let label = busy_label(state).unwrap_or_else(|| "有插件或更新操作正在进行".into());
    format!("{label}，完成后会自动恢复服务；请等待操作结束")
}

/// RAII：互斥操作期间把 busy 描述广播给所有窗口，无论操作从哪条路径返回
/// （包括提前出错），Drop 都会清掉标记再广播一次，不会把界面卡在“维护中”。
pub struct OpsBusy {
    app: AppHandle,
    state: AppState,
}

impl OpsBusy {
    pub fn begin(app: &AppHandle, state: &AppState, label: &str) -> Self {
        if let Ok(mut runtime) = state.runtime.lock() {
            runtime.busy = Some(label.into());
        }
        emit_status(app, state);
        Self {
            app: app.clone(),
            state: state.clone(),
        }
    }
}

impl Drop for OpsBusy {
    fn drop(&mut self) {
        if let Ok(mut runtime) = self.state.runtime.lock() {
            runtime.busy = None;
        }
        emit_status(&self.app, &self.state);
    }
}

#[tauri::command]
pub fn get_status(state: State<'_, AppState>) -> Result<LauncherStatus, String> {
    current_status(state.inner().clone())
}

/// 内嵌页面已经带着令牌装载过一次（cookie 已经换到手）：丢掉一次性地址，
/// 回到干净的裸地址。令牌每个进程新生成，dsh 下次重启后它必然失效，留着
/// 反而会把页面重新推回 401。
#[tauri::command]
pub fn auth_url_consumed(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<LauncherStatus, String> {
    {
        let mut runtime = state.runtime.lock().map_err(|_| "启动器状态锁已损坏")?;
        runtime.web_url = None;
        runtime.auth_satisfied = true;
    }
    emit_status(&app, state.inner());
    current_status(state.inner().clone())
}

#[tauri::command]
pub fn clear_logs(state: State<'_, AppState>) -> Result<LauncherStatus, String> {
    let mut runtime = state.runtime.lock().map_err(|_| "启动器状态锁已损坏")?;
    runtime.logs.clear();
    Ok(snapshot(&runtime))
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOKEN_URL: &str = "http://127.0.0.1:3080/?token=Oxw8sxqCz4LUroe9dPYOltdo5JpMl0KWOV00F9x8C7o";

    #[test]
    fn reads_the_local_token_url_from_the_startup_line() {
        assert_eq!(
            extract_auth_url(&format!("dsh web: {TOKEN_URL}")).as_deref(),
            Some(TOKEN_URL)
        );
    }

    #[test]
    fn keeps_the_local_address_when_a_lan_address_follows() {
        let line = format!("dsh web: {TOKEN_URL} (LAN: http://192.168.1.5:3080/?token=lan)");
        assert_eq!(extract_auth_url(&line).as_deref(), Some(TOKEN_URL));
    }

    #[test]
    fn ignores_lines_without_a_token() {
        assert_eq!(extract_auth_url("dsh web: http://127.0.0.1:3080/"), None);
        assert_eq!(extract_auth_url("dsh web: http://127.0.0.1:3080/?token="), None);
    }

    #[test]
    fn ignores_other_output() {
        assert_eq!(extract_auth_url("[stdout] 正在安装 profile 依赖"), None);
        assert_eq!(extract_auth_url("web-app: listening on 3080"), None);
        assert_eq!(extract_auth_url(""), None);
    }
}
