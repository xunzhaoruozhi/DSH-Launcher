//! 无外壳模式的窗口拖动：两条路，都由本模块自己的轮询线程驱动。
//!
//! 1. Alt+左键：按下即拖，零等待（推荐）。
//! 2. 长按左键约 0.3 秒不动：触发拖动（位移超过 10px 视为在页面里
//!    拖选/操作，本次按压不触发）。
//!
//! 拖动本体不用系统的 WM_NCLBUTTONDOWN 魔法（实测从后台线程触发时
//! start_dragging 受理但窗口不动——WebView2 子窗口持有鼠标捕获），
//! 而是手动循环：按住期间每 8ms 把窗口位置对齐到光标位移，松手即停。
//! 轮询只用 GetAsyncKeyState / GetCursorPos，开销可忽略。

use std::thread;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Manager, PhysicalPosition, WebviewWindow};
use windows_sys::Win32::Foundation::POINT;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_LBUTTON, VK_MENU};
use windows_sys::Win32::UI::WindowsAndMessaging::{GetAncestor, GetCursorPos, WindowFromPoint, GA_ROOT};

use crate::config::read_config;

const HOLD_MS: u64 = 300;
const POLL_MS: u64 = 40;
/// 按住期间位移超过该半径（像素）视为“在拖选/操作”，本次按压不再触发窗口拖动。
const MOVE_RADIUS_SQ: i32 = 100;
/// 拖动循环的跟踪间隔。
const DRAG_POLL_MS: u64 = 8;

fn button_down() -> bool {
    let state = unsafe { GetAsyncKeyState(VK_LBUTTON as i32) };
    state < 0
}
fn alt_down() -> bool {
    let state = unsafe { GetAsyncKeyState(VK_MENU as i32) };
    state < 0
}
fn cursor() -> Option<(i32, i32)> {
    let mut pt = POINT { x: 0, y: 0 };
    (unsafe { GetCursorPos(&mut pt) } != 0).then_some((pt.x, pt.y))
}

pub fn start(app: AppHandle) {
    thread::spawn(move || {
        eprintln!("[free_drag] 轮询线程已启动");
        // 按压起点（时间 + 坐标）；None 表示当前没在跟踪一次按压。
        let mut press: Option<(Instant, i32, i32)> = None;
        let mut moved = false;
        // 一次按压只判定一次：触发或判定失败后收起，直到松开左键重新武装。
        let mut armed = true;
        loop {
            thread::sleep(Duration::from_millis(POLL_MS));
            let held = button_down();
            if !held {
                press = None;
                moved = false;
                armed = true;
                continue;
            }
            if !armed {
                continue;
            }
            let Some((x, y)) = cursor() else { continue };
            // Alt+左键：按下即拖，不等长按。
            if alt_down() {
                armed = false;
                eprintln!("[free_drag] Alt+左键触发");
                drag_window(&app, x, y);
                continue;
            }
            match press {
                None => press = Some((Instant::now(), x, y)),
                Some((t0, x0, y0)) => {
                    let dx = x - x0;
                    let dy = y - y0;
                    if dx * dx + dy * dy > MOVE_RADIUS_SQ {
                        moved = true;
                    }
                    if moved {
                        // 已经在拖选/操作，本次按压放弃判定。
                        armed = false;
                        continue;
                    }
                    if t0.elapsed() >= Duration::from_millis(HOLD_MS) {
                        armed = false;
                        eprintln!("[free_drag] 长按达标");
                        drag_window(&app, x, y);
                    }
                }
            }
        }
    });
}

/// 手动拖动：按住期间把窗口跟着光标位移走，松手结束。
fn drag_window(app: &AppHandle, x: i32, y: i32) {
    if !read_config(app).map(|c| c.hide_shell).unwrap_or(false) {
        return;
    }
    let Some(window) = window_under_cursor(app, x, y) else {
        eprintln!("[free_drag] 未命中本应用窗口，忽略");
        return;
    };
    eprintln!("[free_drag] 开始拖动窗口 {}", window.label());
    // 最大化状态下先还原，否则 set_outer_position 行为异常。
    if window.is_maximized().unwrap_or(false) {
        let _ = window.unmaximize();
        // 等还原动画落定后重取基点，避免跳变。
        thread::sleep(Duration::from_millis(60));
    }
    let Some((cx, cy)) = cursor() else { return };
    let Some(base) = window.outer_position().ok() else { return };
    let (start_x, start_y) = (cx, cy);
    while button_down() {
        thread::sleep(Duration::from_millis(DRAG_POLL_MS));
        let Some((cx, cy)) = cursor() else { continue };
        let _ = window.set_position(PhysicalPosition::new(
            base.x + cx - start_x,
            base.y + cy - start_y,
        ));
    }
    eprintln!("[free_drag] 拖动结束");
}

/// 光标命中的顶层窗口若属于本应用（主窗口或任意“新建窗口”），返回它。
fn window_under_cursor(app: &AppHandle, x: i32, y: i32) -> Option<WebviewWindow> {
    let hit = unsafe { WindowFromPoint(POINT { x, y }) };
    if hit.is_null() {
        return None;
    }
    let root = unsafe { GetAncestor(hit, GA_ROOT) } as usize;
    if root == 0 {
        return None;
    }
    app.webview_windows()
        .into_values()
        .find(|win| win.hwnd().map(|h| h.0 as usize).unwrap_or(0) == root)
}
