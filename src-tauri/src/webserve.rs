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
use tauri::{AppHandle, Emitter, Manager};

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
    write_notify_port_file(&app, port);
    thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let app = app.clone();
            let _ = thread::spawn(move || serve(&app, stream));
        }
    });
    Ok(port)
}

/// 把 webserve 端口写到固定位置，dsh-launcher-notify 插件靠它发现启动器。
/// 启动器退出后文件可能残留，插件 POST 失败会自行清缓存重试，无害。
fn write_notify_port_file(app: &AppHandle, port: u16) {
    let Some(dir) = app.path().app_config_dir().ok() else {
        return;
    };
    let _ = std::fs::create_dir_all(&dir);
    let _ = std::fs::write(
        dir.join("launcher-notify.json"),
        format!("{{\"port\":{port}}}"),
    );
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

    // dsh-launcher-notify 插件的转发端点：POST /launcher/notify。
    // 事件体原样转给前端（由通知插件弹系统通知），这里不做解析。
    if target == "/launcher/notify" {
        // 请求体可能跨 TCP 段：按 content-length 读满再转发，避免丢通知。
        let mut request = request.into_owned();
        let mut read_total = read;
        loop {
            let complete = match request.find("\r\n\r\n") {
                Some(header_end) => {
                    let content_length: usize = request
                        .lines()
                        .find(|line| line.to_ascii_lowercase().starts_with("content-length:"))
                        .and_then(|line| line.split(':').nth(1))
                        .and_then(|value| value.trim().parse().ok())
                        .unwrap_or(0);
                    request.len() >= header_end + 4 + content_length
                }
                None => false,
            };
            if complete || read_total >= buffer.len() {
                break;
            }
            let more = stream.read(&mut buffer[read_total..])?;
            if more == 0 {
                break;
            }
            read_total += more;
            request = String::from_utf8_lossy(&buffer[..read_total]).into_owned();
        }
        let body = request
            .split_once("\r\n\r\n")
            .map(|(_, body)| body.to_string())
            .unwrap_or_default();
        let _ = app.emit("launcher-notify", body);
        stream.write_all(
            b"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: 11\r\nconnection: close\r\n\r\n{\"ok\":true}",
        )?;
        return Ok(());
    }
    if target == "/launcher/ping" {
        stream.write_all(
            b"HTTP/1.1 200 OK\r\ncontent-type: text/plain\r\ncontent-length: 2\r\nconnection: close\r\n\r\nok",
        )?;
        return Ok(());
    }

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
