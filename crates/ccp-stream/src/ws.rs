//! WebSocket client plumbing shared by the stream servers: connect with a
//! timeout, optional bearer token, TLS for `wss://`, bounded message size.

use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;
use tungstenite::client::IntoClientRequest;
use tungstenite::http::HeaderValue;
use tungstenite::protocol::WebSocketConfig;
use tungstenite::stream::MaybeTlsStream;

pub use tungstenite::Message as WsMessage;
pub type Socket = tungstenite::WebSocket<MaybeTlsStream<TcpStream>>;

/// Idle reads time out after this long; the reader then pings to check the link.
pub const IDLE_SECS: u64 = 60;
const MAX_MESSAGE: usize = 4 * 1024 * 1024;

pub fn open(url: &str, bearer: Option<&str>) -> Result<Socket, String> {
    let mut req = url
        .into_client_request()
        .map_err(|e| format!("bad url: {}", e))?;
    if let Some(b) = bearer.filter(|b| !b.is_empty()) {
        let v = HeaderValue::from_str(&format!("Bearer {}", b))
            .map_err(|_| "bearer token has invalid characters".to_string())?;
        req.headers_mut().insert("Authorization", v);
    }
    let uri = req.uri().clone();
    let host = uri.host().ok_or("url has no host")?.to_string();
    let tls = matches!(uri.scheme_str(), Some("wss"));
    let port = uri.port_u16().unwrap_or(if tls { 443 } else { 80 });
    let addr = (host.as_str(), port)
        .to_socket_addrs()
        .map_err(|e| format!("cannot resolve {}: {}", host, e))?
        .next()
        .ok_or_else(|| format!("cannot resolve {}", host))?;
    let stream = TcpStream::connect_timeout(&addr, Duration::from_secs(10))
        .map_err(|e| format!("cannot connect to {}:{}: {}", host, port, e))?;
    stream.set_nodelay(true).ok();
    stream
        .set_read_timeout(Some(Duration::from_secs(IDLE_SECS)))
        .ok();
    stream.set_write_timeout(Some(Duration::from_secs(10))).ok();
    let cfg = WebSocketConfig::default()
        .max_message_size(Some(MAX_MESSAGE))
        .max_frame_size(Some(MAX_MESSAGE));
    let (ws, _) = tungstenite::client_tls_with_config(req, stream, Some(cfg), None)
        .map_err(|e| format!("websocket handshake failed: {}", e))?;
    Ok(ws)
}

/// Strip the query string (it often carries tokens) before showing a URL.
pub fn redact(url: &str) -> String {
    url.split('?').next().unwrap_or(url).to_string()
}

/// True for the read-timeout "nothing arrived" case.
pub fn is_idle(e: &tungstenite::Error) -> bool {
    matches!(e, tungstenite::Error::Io(io) if matches!(io.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut))
}
