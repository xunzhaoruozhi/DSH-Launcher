//! 生产构建下用本机 HTTP 提供 Launcher 自己的页面。
//!
//! 为什么不继续用 Tauri 的内置协议：dsh 自 0.1.2 起给 Web 界面加了浏览器
//! 认证，令牌换来的 Cookie 只有在「和宿主页面同站」时才落得下来 —— 实测
//! WebView2 里跨站 iframe 的 Set-Cookie 会被第三方 Cookie 策略丢掉（Edge
//! 同样），随后每个请求都是 401。把页面放到 `http://localhost:<port>` 后，
//! 内嵌的 `http://localhost:<dsh 端口>` 与它同站，Cookie 正常生效。
//!
//! 端口是随机的本机回环端口，只服务内置的前端资源。

use std::{
    io::{Read, Write},
    net::{Ipv4Addr, TcpListener, TcpStream},
    thread,
    time::Duration,
};
use tauri::AppHandle;

/// 前端页面的监听端口；窗口用它拼 origin，认证与同站判定都要靠它。
#[derive(Clone, Copy)]
pub struct FrontendPort(pub u16);

/// 在回环上开一个只读的静态服务器，返回它实际监听的端口。
pub fn start(app: AppHandle) -> Result<u16, String> {
    let listener =
        TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).map_err(|error| error.to_string())?;
    let port = listener
        .local_addr()
        .map_err(|error| error.to_string())?
        .port();
    thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let app = app.clone();
            let _ = thread::spawn(move || serve(&app, stream));
        }
    });
    Ok(port)
}

fn serve(app: &AppHandle, mut stream: TcpStream) -> std::io::Result<()> {
    // 单个连接只处理一次请求（connection: close），读超时防止半开连接把线程挂住。
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let mut buffer = [0u8; 4096];
    let read = stream.read(&mut buffer)?;
    let request = String::from_utf8_lossy(&buffer[..read]);
    let target = request
        .split_whitespace()
        .nth(1)
        .unwrap_or("/")
        .split('?')
        .next()
        .unwrap_or("/");
    let path = if target == "/" {
        "index.html"
    } else {
        target.trim_start_matches('/')
    };
    let resolver = app.asset_resolver();
    match resolver.get(path.to_string()) {
        Some(asset) => {
            let bytes = asset.bytes();
            let head = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: {}\r\ncontent-length: {}\r\ncache-control: no-store\r\nconnection: close\r\n\r\n",
                asset.mime_type,
                bytes.len()
            );
            stream.write_all(head.as_bytes())?;
            stream.write_all(bytes.as_ref())?;
        }
        None => {
            stream.write_all(
                b"HTTP/1.1 404 Not Found\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
            )?;
        }
    }
    Ok(())
}
