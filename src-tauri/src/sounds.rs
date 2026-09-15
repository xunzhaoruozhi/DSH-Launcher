// 自定义通知音：Windows toast 只认系统内置音名，放不了自定义 mp3，
// 这里用 winmm 的 MCI 接口在弹窗之外单独播放（弹窗本身保持静音）。
// 音效文件随 tauri resources 打包（src-tauri/sounds/*.mp3）。
#![cfg(target_os = "windows")]

use std::path::PathBuf;
use tauri::{AppHandle, Manager};
use windows_sys::Win32::Media::Multimedia::mciSendStringW;

/// 自定义音效白名单（同时是 sounds/ 目录下的文件名，不含扩展名）。
pub const CUSTOM_SOUNDS: &[&str] = &[
    "taskCompleted",
    "taskFailed",
    "success",
    "error",
    "warning",
    "terminalBell",
];

/// Windows toast 内置音名（这些由弹窗自己发声，不走这里）。
pub const TOAST_SOUNDS: &[&str] = &[
    "Default", "IM", "Mail", "Reminder", "SMS", "Alarm", "Alarm2", "Call",
];

fn to_wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

fn sound_path(app: &AppHandle, name: &str) -> Option<PathBuf> {
    let path = app
        .path()
        .resource_dir()
        .ok()?
        .join("sounds")
        .join(format!("{name}.mp3"));
    path.exists().then_some(path)
}

/// 后台线程播放一个自定义音效；同名别名先关再开，快速连发时打断上一段。
pub fn play(app: &AppHandle, name: &str) {
    if !CUSTOM_SOUNDS.contains(&name) {
        return;
    }
    let Some(path) = sound_path(app, name) else { return };
    let path = path.to_string_lossy().into_owned();
    std::thread::spawn(move || unsafe {
        let close = to_wide("close dshnotify");
        let _ = mciSendStringW(close.as_ptr(), std::ptr::null_mut(), 0, std::ptr::null_mut());
        // type mpegvideo：MCI 默认设备类型不认 mp3，必须显式指定。
        let open = to_wide(&format!("open \"{path}\" type mpegvideo alias dshnotify"));
        if mciSendStringW(open.as_ptr(), std::ptr::null_mut(), 0, std::ptr::null_mut()) == 0 {
            let play = to_wide("play dshnotify wait");
            let _ = mciSendStringW(play.as_ptr(), std::ptr::null_mut(), 0, std::ptr::null_mut());
            let close = to_wide("close dshnotify");
            let _ = mciSendStringW(close.as_ptr(), std::ptr::null_mut(), 0, std::ptr::null_mut());
        }
    });
}
