use std::{
    sync::atomic::{AtomicU64, Ordering},
    thread,
    time::Duration,
};
#[cfg(target_os = "macos")]
use tauri::TitleBarStyle;
use tauri::{
    webview::DownloadEvent, AppHandle, Emitter, Manager, State, WebviewUrl, WebviewWindowBuilder,
};
#[cfg(target_os = "macos")]
use window_vibrancy::{apply_vibrancy, NSVisualEffectMaterial, NSVisualEffectState};

use crate::{
    download::handle_download_request,
    state::{AppState, Phase},
    util::open_external_url,
};

// Links rendered inside the dsh iframe do not bubble to the launcher document.
// Install the bridge in every frame so ordinary http(s) links reach the system
// browser instead of navigating the embedded WebUI.
const EXTERNAL_LINK_BRIDGE: &str = r#"
(() => {
  if (window.__DSH_LAUNCHER_EXTERNAL_LINKS__) return;
  window.__DSH_LAUNCHER_EXTERNAL_LINKS__ = true;

  // These file-copy controls are rendered inside DSH's embedded WebUI. They
  // are not part of the desktop workflow, so remove them from every iframe.
  if (window !== window.top) {
    const removeFileCopyControls = () => {
      const id = 'dsh-launcher-hide-copy-file-controls';
      if (document.getElementById(id)) return;
      const root = document.head || document.documentElement;
      if (!root) return;
      const style = document.createElement('style');
      style.id = id;
      style.textContent = '[data-mobile-nav="copy-file"] { display: none !important; }';
      root.appendChild(style);
    };
    removeFileCopyControls();
    document.addEventListener('DOMContentLoaded', removeFileCopyControls, { once: true });
  }

  const isDeveloperToolsShortcut = (event) => event.key === 'F12'
    || (event.ctrlKey && event.shiftKey && !event.altKey && !event.metaKey
      && event.key.toLowerCase() === 'i');
  document.addEventListener('keydown', (event) => {
    if (!isDeveloperToolsShortcut(event)) return;
    event.preventDefault();
    event.stopImmediatePropagation();
    if (window === window.top) {
      window.__TAURI_INTERNALS__?.invoke('open_developer_tools')?.catch(() => {});
    } else {
      window.top.postMessage({ type: 'dsh-launcher-open-devtools' }, '*');
    }
  }, true);

  const isLoopback = (hostname) => hostname === 'localhost'
    || hostname === '127.0.0.1'
    || hostname === '[::1]';

  const openExternal = (value) => {
    let url;
    try { url = new URL(value, window.location.href); } catch { return false; }
    if (url.protocol !== 'http:' && url.protocol !== 'https:') return false;
    if (isLoopback(url.hostname)) return false;
    const message = { type: 'dsh-launcher-open-external', url: url.href };
    if (window === window.top) {
      window.__TAURI_INTERNALS__?.invoke('open_external', { url: url.href });
    } else {
      window.top.postMessage(message, '*');
    }
    return true;
  };

  if (window === window.top) {
    window.addEventListener('message', (event) => {
      if (event.data?.type === 'dsh-launcher-open-devtools') {
        window.__TAURI_INTERNALS__?.invoke('open_developer_tools')?.catch(() => {});
        return;
      }
      if (event.data?.type !== 'dsh-launcher-open-external'
        || typeof event.data.url !== 'string') return;
      openExternal(event.data.url);
    });
  }

  const handleClick = (event) => {
    if (event.defaultPrevented || event.button !== 0) return;
    const target = event.target;
    const anchor = target instanceof Element ? target.closest('a[href]') : null;
    if (!anchor || anchor.hasAttribute('download')) return;
    if (!openExternal(anchor.href)) return;
    event.preventDefault();
    event.stopImmediatePropagation();
  };

  document.addEventListener('click', handleClick, true);
  document.addEventListener('auxclick', (event) => {
    if (event.button !== 1) return;
    const target = event.target;
    const anchor = target instanceof Element ? target.closest('a[href]') : null;
    if (!anchor || anchor.hasAttribute('download') || !openExternal(anchor.href)) return;
    event.preventDefault();
    event.stopImmediatePropagation();
  }, true);
})();
"#;

/// 窗口要加载的页面地址。
///
/// 生产构建走本机 HTTP（`http://localhost:<port>`）而不是 Tauri 内置协议：
/// 内嵌的 dsh 页面与宿主同站，认证 Cookie 才不会被第三方 Cookie 策略丢掉。
/// 开发构建沿用 dev server，它本身就在 localhost 上。
fn launcher_url(app: &AppHandle) -> Result<WebviewUrl, String> {
    if tauri::is_dev() {
        return Ok(WebviewUrl::App("index.html".into()));
    }
    match app.try_state::<crate::webserve::FrontendPort>() {
        Some(port) => format!("http://localhost:{}/index.html", port.0)
            .parse()
            .map(WebviewUrl::External)
            .map_err(|error| format!("前端地址无效：{error}")),
        None => Ok(WebviewUrl::App("index.html".into())),
    }
}

fn next_window_label() -> String {
    static WINDOW_COUNTER: AtomicU64 = AtomicU64::new(0);
    format!(
        "control-{}",
        WINDOW_COUNTER.fetch_add(1, Ordering::Relaxed) + 1
    )
}

fn reveal_window_fallback(window: tauri::WebviewWindow) {
    // 窗口以隐藏状态创建，由前端首帧就绪后 show()，避免启动白屏。这里兜底：
    // 前端脚本万一没跑起来，也要在几秒内把窗口亮出来，不能凭空消失。
    thread::spawn(move || {
        thread::sleep(Duration::from_secs(3));
        if !window.is_visible().unwrap_or(true) {
            let _ = window.show();
        }
    });
}

pub fn spawn_launcher_window_named(
    app: &AppHandle,
    label: String,
    position: Option<(f64, f64)>,
    size: Option<(f64, f64)>,
) -> Result<(), String> {
    // Use the app URL so Tauri resolves the dev server in development and
    // the bundled frontend in production. External URLs do not reliably get
    // the launcher initialization scripts and window APIs.
    let (width, height) = size.unwrap_or((1100.0, 760.0));
    // dsh WebUI 在 1023px 以下切换到移动布局；保持窗口宽度高于断点，
    // 让内嵌页面始终使用桌面布局。
    let width = width.max(1024.0);
    let config = crate::config::read_config(app).unwrap_or_default();
    let developer_mode = config.developer_mode;
    let mut builder = WebviewWindowBuilder::new(app, &label, launcher_url(app)?)
        .title("DSH Launcher")
        .inner_size(width, height)
        .min_inner_size(1024.0, 560.0)
        .devtools(developer_mode)
        .resizable(true)
        .visible(false);
    // macOS：启用原生装饰 + 叠加标题栏，露出标准红黄绿按钮（左侧）。
    // transparent(true) 配合 macos-private-api，让标题栏下方的原生振动模糊
    // （NSVisualEffectView）能够透出；前端把标题栏设为半透明以呈现毛玻璃。
    // Windows/Linux：保持无边框自定义标题栏（与原始行为完全一致）。
    #[cfg(target_os = "macos")]
    {
        builder = builder
            .decorations(true)
            .title_bar_style(TitleBarStyle::Overlay)
            .transparent(true)
            // Overlay 模式下系统会在红绿灯右侧绘制原生标题文字；清空标题
            // 只保留红绿灯，避免“DSH Launcher”文字与前端居中的标签区在
            // 小窗口下交叠。窗口标题由 Dock/App 菜单提供，无需在此显示。
            .title("");
    }
    #[cfg(not(target_os = "macos"))]
    {
        builder = builder.decorations(false);
    }
    #[cfg(target_os = "windows")]
    {
        // 把 dsh 的源隔离进独立渲染进程（站点隔离）。否则它和壳子页面同站
        // （localhost 不同端口），共用一个渲染进程：dsh 页面流式输出烧 CPU
        // 时，壳子的输入响应与绘制被一起挤死，表现为整个窗口点不动。
        // WebView2 默认不开站点隔离，--isolate-origins 单独传不生效，
        // 必须同时带 --site-per-process 才会把 iframe 拆成独立渲染进程。
        let dsh_origins = format!(
            "http://localhost:{},http://127.0.0.1:{}",
            config.port, config.port
        );
        builder = builder.additional_browser_args(
            format!(
                "--disable-features=msWebOOUI,msPdfOOUI,msSmartScreenProtection \
                 --enable-features=SitePerProcess --isolate-origins={dsh_origins}"
            )
            .as_str(),
        );
    }
    builder = match position {
        Some((x, y)) => builder.position(x, y),
        None => builder.center(),
    };
    let download_app = app.clone();
    let window = builder
        // Install the click bridge in the dsh iframe as well as the launcher page.
        .initialization_script_for_all_frames(EXTERNAL_LINK_BRIDGE)
        // dsh WebUI 在 iframe 内，外层 document 的 click 事件收不到它的链接。
        // 对 target=_blank/window.open，直接交给系统浏览器，避免在壳子里生成
        // 一个没有认证上下文的新窗口。
        .on_new_window(move |url, _features| {
            if url.scheme() == "http" || url.scheme() == "https" {
                let _ = open_external_url(url.as_str());
            }
            tauri::webview::NewWindowResponse::Deny
        })
        .on_download(move |_webview, event| match event {
            DownloadEvent::Requested { url, destination } => {
                handle_download_request(&download_app, &url, destination)
            }
            DownloadEvent::Finished { url, path, success } => {
                if success {
                    if let Some(path) = path {
                        eprintln!("下载完成 {url}：{}", path.display());
                    }
                } else {
                    eprintln!("下载失败 {url}");
                }
                true
            }
            _ => true,
        })
        .build()
        .map_err(|error| format!("无法创建 Launcher 窗口：{error}"))?;
    // macOS：安装原生振动模糊（HeaderView 材质），标题栏呈系统毛玻璃质感。
    // 失败不影响功能，仅降级为纯色标题栏。
    #[cfg(target_os = "macos")]
    {
        let _ = apply_vibrancy(
            &window,
            NSVisualEffectMaterial::HeaderView,
            Some(NSVisualEffectState::Active),
            None,
        );
    }
    reveal_window_fallback(window);
    Ok(())
}

fn spawn_launcher_window(
    app: &AppHandle,
    position: Option<(f64, f64)>,
    size: Option<(f64, f64)>,
) -> Result<(), String> {
    spawn_launcher_window_named(app, next_window_label(), position, size)
}

// 必须是 async：同步命令在主线程执行，而在主线程上同步创建 WebView 会在
// Windows 上死锁（wry#583）——新窗口停在白屏，且事件循环被卡住，所有窗口的
// 拖拽/最小化/关闭全部失效。
#[tauri::command]
pub async fn new_launcher_window(app: AppHandle) -> Result<(), String> {
    spawn_launcher_window(&app, None, None)
}

#[tauri::command]
pub fn open_developer_tools(window: tauri::WebviewWindow) -> Result<(), String> {
    let config = crate::config::read_config(&window.app_handle())?;
    if config.developer_mode {
        window.open_devtools();
    }
    Ok(())
}

#[cfg(target_os = "windows")]
pub fn apply_devtools_setting(app: &AppHandle, enabled: bool) -> Result<(), String> {
    for (label, window) in app.webview_windows() {
        if !label.starts_with("control") {
            continue;
        }
        set_window_devtools_enabled(&window, enabled)
            .map_err(|error| format!("无法更新窗口 {label} 的开发者工具权限：{error}"))?;
    }
    Ok(())
}

#[cfg(not(target_os = "windows"))]
pub fn apply_devtools_setting(_app: &AppHandle, _enabled: bool) -> Result<(), String> {
    // 其他平台在创建 WebView 时读取 developer_mode；新窗口会立即使用新设置。
    Ok(())
}

#[cfg(target_os = "windows")]
fn set_window_devtools_enabled(window: &tauri::WebviewWindow, enabled: bool) -> Result<(), String> {
    let (sender, receiver) = std::sync::mpsc::channel();
    window
        .with_webview(move |webview| {
            let result = unsafe {
                webview
                    .controller()
                    .CoreWebView2()
                    .map_err(|error| error.to_string())
                    .and_then(|webview| webview.Settings().map_err(|error| error.to_string()))
                    .and_then(|settings| {
                        settings
                            .SetAreDevToolsEnabled(enabled)
                            .map_err(|error| error.to_string())
                    })
            };
            let _ = sender.send(result);
        })
        .map_err(|error| error.to_string())?;
    receiver
        .recv_timeout(Duration::from_secs(5))
        .map_err(|error| format!("等待 WebView2 更新超时：{error}"))?
}

#[tauri::command]
pub fn open_workspace(app: AppHandle, state: State<'_, AppState>) -> Result<(), String> {
    {
        let runtime = state.runtime.lock().map_err(|_| "启动器状态锁已损坏")?;
        if runtime.phase != Phase::Ready {
            return Err("dsh Web 服务尚未就绪".into());
        }
    }
    if let Some(control) = app.get_webview_window("control") {
        control.show().map_err(|error| error.to_string())?;
        control.set_focus().map_err(|error| error.to_string())?;
    }
    let _ = app.emit("show-workspace", ());
    Ok(())
}

pub fn show_control(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("control") {
        let _ = window.show();
        let _ = window.set_focus();
    }
}
