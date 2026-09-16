use std::{thread, time::Duration};
use tauri::{
    menu::{Menu, MenuItem},
    tray::TrayIconBuilder,
    Manager,
};

use crate::{
    config::read_config,
    service::{enter_safe_mode, start_with_feedback, stop_process},
    state::AppState,
    windows_ui::{open_workspace, show_control},
};

pub fn setup_tray(app: &tauri::App) -> tauri::Result<()> {
    let open = MenuItem::with_id(app, "open", "打开 DSH", true, None::<&str>)?;
    let settings = MenuItem::with_id(app, "settings", "启动设置", true, None::<&str>)?;
    let safe_mode = MenuItem::with_id(app, "safe-mode", "进入安全模式", true, None::<&str>)?;
    let restart = MenuItem::with_id(app, "restart", "重启服务", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "退出", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&open, &settings, &safe_mode, &restart, &quit])?;
    let mut tray = TrayIconBuilder::new().menu(&menu).tooltip("DSH Launcher");
    if let Some(icon) = app.default_window_icon() {
        tray = tray.icon(icon.clone());
    }
    tray.on_menu_event(|app, event| match event.id().as_ref() {
        "open" => {
            let state = app.state::<AppState>();
            if open_workspace(app.clone(), state).is_err() {
                show_control(app);
            }
        }
        "settings" => show_control(app),
        // 安全模式：一次性隔离环境启动，用于正式环境起不来时的修复。
        "safe-mode" => {
            show_control(app);
            let app_handle = app.clone();
            thread::spawn(move || {
                let state = app_handle.state::<AppState>().inner().clone();
                let Ok(_ops) = state.ops.try_lock() else {
                    return;
                };
                let _ = enter_safe_mode(&app_handle, &state);
            });
        }
        "restart" => {
            let app_handle = app.clone();
            thread::spawn(move || {
                let state = app_handle.state::<AppState>().inner().clone();
                // 防呆：插件/更新操作进行中忽略托盘重启，操作结束会自动恢复服务
                //（界面此时也显示“维护中”并禁用启动入口）。
                let Ok(_ops) = state.ops.try_lock() else {
                    return;
                };
                let _ = stop_process(&app_handle, &state);
                let _ = start_with_feedback(&app_handle, &state);
            });
        }
        "quit" => {
            // 停止 dsh 可能要等待 Windows 进程树退出，绝不能在托盘事件线程同步做，
            // 否则会把 Launcher 的窗口消息循环一起卡住，表现为 Alt+Tab 失灵。
            let app_handle = app.clone();
            let state = app.state::<AppState>().inner().clone();
            let stop = read_config(app).unwrap_or_default().stop_dsh_on_exit;
            thread::spawn(move || {
                if stop {
                    let _ = stop_process(&app_handle, &state);
                }
                thread::sleep(Duration::from_millis(120));
                app_handle.exit(0);
            });
        }
        _ => {}
    })
    .build(app)?;
    Ok(())
}
