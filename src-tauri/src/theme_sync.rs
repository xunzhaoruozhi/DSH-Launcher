//! 顶栏主题跟随：定时采样 dsh 页面在顶栏正下方的像素颜色，推给前端
//! 把标题栏涂成同色。dsh 页面在跨域 iframe 里，读不到它的主题状态，
//! 但「顶栏永远和紧挨着的内容同色」用采样实现最稳——日间/夜间切换、
//! 任何主题都能无缝跟随。

use std::thread;
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager};
use windows_sys::Win32::Foundation::RECT;
use windows_sys::Win32::Graphics::Gdi::{
    CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC, DeleteObject, GetPixel, GetWindowDC,
    ReleaseDC, SelectObject,
};

// windows-sys 0.59 还没收录 PrintWindow（0.60 才有），直接声明系统函数。
// PW_RENDERFULLCONTENT = 2：WebView 的 DirectComposition 内容也要能截到。
#[link(name = "user32")]
extern "system" {
    fn PrintWindow(hwnd: *mut core::ffi::c_void, hdc: *mut core::ffi::c_void, flags: u32) -> i32;
}
const PW_RENDERFULLCONTENT: u32 = 2;
use windows_sys::Win32::UI::WindowsAndMessaging::GetWindowRect;

/// 采样行：顶栏 35px 之下 10px，已进入页面内容区。
const SAMPLE_Y: i32 = 45;
/// 采样条高度：只要顶栏下方一小条，别整窗截图。
const STRIP_HEIGHT: i32 = 60;
const POLL_MS: u64 = 2000;
/// 颜色变化小于该阈值不广播，避免抖动刷事件。
const CHANGE_EPSILON: i32 = 12;

pub fn start(app: AppHandle) {
    thread::spawn(move || {
        let mut last: Option<[i32; 3]> = None;
        loop {
            thread::sleep(Duration::from_millis(POLL_MS));
            let Some(rgb) = sample_below_titlebar(&app) else {
                continue;
            };
            if let Some(prev) = last {
                let dist = (prev[0] - rgb[0]).abs() + (prev[1] - rgb[1]).abs() + (prev[2] - rgb[2]).abs();
                if dist < CHANGE_EPSILON {
                    continue;
                }
            }
            last = Some(rgb);
            let hex = format!("#{:02x}{:02x}{:02x}", rgb[0], rgb[1], rgb[2]);
            let _ = app.emit("dsh-theme", hex);
        }
    });
}

/// 截主窗口顶部一条，取标题栏正下方三个点的中位色（RGB 三个分量）。
fn sample_below_titlebar(app: &AppHandle) -> Option<[i32; 3]> {
    let window = app.get_webview_window("control")?;
    // tauri 返回的是 windows crate 的 HWND（isize 新类型），windows-sys 0.59
    // 的句柄是裸指针，转一下。
    let hwnd = window.hwnd().ok()?;
    let hwnd = hwnd.0 as *mut core::ffi::c_void;
    let mut rect = RECT { left: 0, top: 0, right: 0, bottom: 0 };
    if unsafe { GetWindowRect(hwnd, &mut rect) } == 0 {
        return None;
    }
    let width = rect.right - rect.left;
    if width < 200 {
        return None;
    }
    unsafe {
        let hdc_window = GetWindowDC(hwnd);
        if hdc_window.is_null() {
            return None;
        }
        let hdc_mem = CreateCompatibleDC(hdc_window);
        let bitmap = CreateCompatibleBitmap(hdc_window, width, STRIP_HEIGHT);
        let old = SelectObject(hdc_mem, bitmap);
        PrintWindow(hwnd, hdc_mem, PW_RENDERFULLCONTENT);
        let xs = [width * 3 / 10, width / 2, width * 7 / 10];
        let mut samples: Vec<[i32; 3]> = xs
            .iter()
            .map(|&x| GetPixel(hdc_mem, x, SAMPLE_Y))
            .filter(|&c| c != 0xFFFF_FFFF)
            .map(|c| [(c & 0xFF) as i32, ((c >> 8) & 0xFF) as i32, ((c >> 16) & 0xFF) as i32])
            .collect();
        SelectObject(hdc_mem, old);
        DeleteObject(bitmap);
        DeleteDC(hdc_mem);
        ReleaseDC(hwnd, hdc_window);
        if samples.is_empty() {
            return None;
        }
        // 三个点取各通道中位数：躲开偶尔压在图标或文字上的点。
        let mut result = [0i32; 3];
        for channel in 0..3 {
            samples.sort_by_key(|c| c[channel]);
            result[channel] = samples[samples.len() / 2][channel];
        }
        Some(result)
    }
}
