// 全局唤出快捷键：用系统级热键把隐藏的 control 主窗口唤出/收起。
// 热键在后端注册，因此窗口即便隐藏、失焦也能触发，不依赖页面焦点。
use std::thread;

use tauri::{AppHandle, Manager};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, ShortcutState};

use crate::config::read_config;

/// 读取配置里的 summon_shortcut 并重新注册热键。空字符串＝禁用。
///
/// 注册/注销热键内部会「向主线程投递任务并阻塞等待结果」，所以绝不能在主线程上
/// 同步调用（setup 闭包、同步 #[tauri::command] 都在主线程，会自锁）。这里统一把
/// 实际工作丢到后台线程：先注销本应用的全部热键，再按需注册，保证幂等。
pub fn apply(app: &AppHandle) {
    let shortcut = read_config(app).unwrap_or_default().summon_shortcut.trim().to_string();
    let app = app.clone();
    thread::spawn(move || {
        let gs = app.global_shortcut();
        // 本应用只用热键做唤出，注销全部再按需注册即可保持幂等。
        let _ = gs.unregister_all();
        if shortcut.is_empty() {
            return;
        }
        // 注册失败（比如被别的程序占用）时静默降级，不影响 Launcher 其它功能。
        let _ = gs.on_shortcut(shortcut.as_str(), |app, _shortcut, event| {
            if event.state == ShortcutState::Pressed {
                toggle_control(app);
            }
        });
    });
}

/// 唤出/收起切换：主窗口已可见且拿到焦点就隐藏，否则显示并置顶聚焦。
fn toggle_control(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("control") {
        let visible = window.is_visible().unwrap_or(false);
        let focused = window.is_focused().unwrap_or(false);
        if visible && focused {
            let _ = window.hide();
        } else {
            let _ = window.unminimize();
            let _ = window.show();
            let _ = window.set_focus();
        }
    }
}
