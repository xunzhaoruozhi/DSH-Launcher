// 自定义通知音：Windows toast 只认系统内置音名，放不了自定义音频，
// 这里用 winmm 的 MCI 接口在弹窗之外单独播放（弹窗本身保持静音）。
// 内置音效随 tauri resources 打包（src-tauri/sounds/*.mp3）；
// 用户自选的声音文件存 app_config_dir/sounds/custom.{mp3,wav}。
use std::path::PathBuf;
use tauri::{AppHandle, Manager};

/// 内置音效白名单（sounds/ 目录下的文件名，不含扩展名）。
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

fn config_sounds_dir(app: &AppHandle) -> Option<PathBuf> {
    app.path().app_config_dir().ok().map(|dir| dir.join("sounds"))
}

/// 用户自选声音文件（custom.mp3 / custom.wav，先到先得）。
fn custom_file(app: &AppHandle) -> Option<PathBuf> {
    let dir = config_sounds_dir(app)?;
    ["mp3", "wav"]
        .iter()
        .map(|ext| dir.join(format!("custom.{ext}")))
        .find(|path| path.is_file())
}

/// 播放一个自定义音效；同名别名先关再开，快速连发时打断上一段。
/// 仅 Windows 实现；其他平台静默跳过。
pub fn play(app: &AppHandle, name: &str) {
    let path = if name == "custom" {
        match custom_file(app) {
            Some(path) => path,
            None => return,
        }
    } else {
        if !CUSTOM_SOUNDS.contains(&name) {
            return;
        }
        match app
            .path()
            .resource_dir()
            .ok()
            .map(|dir| dir.join("sounds").join(format!("{name}.mp3")))
            .filter(|path| path.exists())
        {
            Some(path) => path,
            None => return,
        }
    };
    #[cfg(target_os = "windows")]
    play_windows(&path);
}

#[cfg(target_os = "windows")]
fn play_windows(path: &std::path::Path) {
    use windows_sys::Win32::Media::Multimedia::mciSendStringW;
    fn to_wide(value: &str) -> Vec<u16> {
        value.encode_utf16().chain(std::iter::once(0)).collect()
    }
    // mp3 走 mpegvideo 设备，wav 走 waveaudio；MCI 默认类型不认 mp3。
    let device = if path.extension().and_then(|ext| ext.to_str()) == Some("wav") {
        "waveaudio"
    } else {
        "mpegvideo"
    };
    let path = path.to_string_lossy().into_owned();
    std::thread::spawn(move || unsafe {
        let close = to_wide("close dshnotify");
        let _ = mciSendStringW(close.as_ptr(), std::ptr::null_mut(), 0, std::ptr::null_mut());
        let open = to_wide(&format!("open \"{path}\" type {device} alias dshnotify"));
        if mciSendStringW(open.as_ptr(), std::ptr::null_mut(), 0, std::ptr::null_mut()) == 0 {
            let play = to_wide("play dshnotify wait");
            let _ = mciSendStringW(play.as_ptr(), std::ptr::null_mut(), 0, std::ptr::null_mut());
            let close = to_wide("close dshnotify");
            let _ = mciSendStringW(close.as_ptr(), std::ptr::null_mut(), 0, std::ptr::null_mut());
        }
    });
}

/// 是否已设置用户自选声音文件。
#[tauri::command]
pub fn has_custom_notify_sound(app: AppHandle) -> bool {
    custom_file(&app).is_some()
}

/// 把用户选择的声音文件复制进配置目录，作为「自定义」通知音。
#[tauri::command]
pub fn set_custom_notify_sound(app: AppHandle, path: String) -> Result<bool, String> {
    let source = PathBuf::from(&path);
    let ext = source
        .extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.to_ascii_lowercase())
        .ok_or("文件没有扩展名")?;
    if ext != "mp3" && ext != "wav" {
        return Err("只支持 mp3 / wav".into());
    }
    if !source.is_file() {
        return Err("文件不存在".into());
    }
    let dir = config_sounds_dir(&app).ok_or("无法定位配置目录")?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    // 换类型时清掉旧的，避免 custom.mp3 / custom.wav 同时存在。
    for old in ["mp3", "wav"] {
        if old != ext {
            let _ = std::fs::remove_file(dir.join(format!("custom.{old}")));
        }
    }
    std::fs::copy(&source, dir.join(format!("custom.{ext}"))).map_err(|e| e.to_string())?;
    Ok(true)
}
