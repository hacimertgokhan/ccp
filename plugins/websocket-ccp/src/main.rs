//! websocket-ccp: CCP server for a WebSocket feed.
//!
//! A background thread keeps the connection open (reconnecting with backoff) and
//! fills a ring buffer; the model asks for the fields, windows and summaries it
//! needs. It never sends anything the operator did not configure.
//!
//! Env: CCP_URL        ws:// or wss:// endpoint (required)
//!      CCP_BEARER     token sent as `Authorization: Bearer ...`
//!      CCP_SUBSCRIBE  text message(s) sent after connecting, one per line (operator-defined)
//!      CCP_BUFFER     messages kept (default 5000)     CCP_BUFFER_MB  bytes kept (default 32)

use ccp_mcp::{Service, scrub};
use ccp_stream::ws::{self, WsMessage};
use ccp_stream::{Buffer, StreamService, env_usize, spawn_reader};
use std::sync::Arc;
use std::time::Duration;

fn session(
    buf: &Buffer,
    url: &str,
    bearer: &Option<String>,
    subscribe: &[String],
) -> Result<(), String> {
    let mut sock = ws::open(url, bearer.as_deref())?;
    for m in subscribe {
        sock.send(WsMessage::text(m.clone()))
            .map_err(|e| e.to_string())?;
    }
    buf.set_connected(true, None);
    loop {
        match sock.read() {
            Ok(WsMessage::Text(t)) => buf.push(None, t.as_str().to_string()),
            Ok(WsMessage::Binary(b)) => buf.push(None, String::from_utf8_lossy(&b).into_owned()),
            Ok(WsMessage::Close(_)) => return Err("closed by server".into()),
            Ok(_) => {}
            Err(e) if ws::is_idle(&e) => sock
                .send(WsMessage::Ping(Vec::new().into()))
                .map_err(|e| e.to_string())?,
            Err(e) => return Err(e.to_string()),
        }
        if buf.stopped() {
            return Ok(());
        }
    }
}

fn connect() -> Result<Box<dyn Service>, String> {
    let url = ccp_mcp::env_required("CCP_URL")?;
    if !(url.starts_with("ws://") || url.starts_with("wss://")) {
        return Err("CCP_URL must start with ws:// or wss://".into());
    }
    let bearer = std::env::var("CCP_BEARER").ok().filter(|b| !b.is_empty());
    let subscribe: Vec<String> = std::env::var("CCP_SUBSCRIBE")
        .map(|v| {
            v.lines()
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .map(String::from)
                .collect()
        })
        .unwrap_or_default();
    let buf = Buffer::new(
        env_usize("CCP_BUFFER", 5000),
        env_usize("CCP_BUFFER_MB", 32) * 1024 * 1024,
    );
    let secret = bearer.clone().unwrap_or_default();
    let (u, b, s) = (url.clone(), bearer.clone(), subscribe.clone());
    spawn_reader(buf.clone(), move |buf| {
        session(buf, &u, &b, &s).map_err(|e| scrub(&e, &secret))
    });
    if let Err(e) = buf.wait_first(Duration::from_secs(12)) {
        buf.shutdown();
        return Err(e);
    }
    let shown = ws::redact(&url);
    Ok(Box::new(StreamService::new(
        Arc::clone(&buf),
        format!("WebSocket {}", shown),
        format!("Listening to {}.", shown),
    )))
}

fn main() {
    ccp_mcp::main(ccp_stream::meta("websocket-ccp", "WebSocket"), connect);
}

#[cfg(test)]
mod tests {
    use super::*;
    use ccp_proto::serve::answer_query;
    use std::net::TcpListener;
    use tungstenite::accept;

    #[test]
    fn listens_subscribes_and_answers() {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        std::thread::spawn(move || {
            let (s, _) = l.accept().unwrap();
            let mut ws = accept(s).unwrap();
            // the operator-configured subscription must arrive first
            let first = ws.read().unwrap();
            assert_eq!(
                first.into_text().unwrap().as_str(),
                r#"{"op":"sub","ch":"trades"}"#
            );
            for (sym, px) in [("BTC", 100.5), ("ETH", 10.25), ("BTC", 101.5)] {
                ws.send(WsMessage::text(format!(
                    r#"{{"sym":"{}","px":{},"meta":{{"venue":"x"}}}}"#,
                    sym, px
                )))
                .unwrap();
            }
            ws.send(WsMessage::text("plain text frame")).unwrap();
            std::thread::sleep(Duration::from_secs(5));
        });
        // SAFETY of test env: set before connect(), single-threaded at this point.
        unsafe {
            std::env::set_var(
                "CCP_URL",
                format!("ws://127.0.0.1:{}/feed?token=secret", port),
            );
            std::env::set_var("CCP_SUBSCRIBE", r#"{"op":"sub","ch":"trades"}"#);
        }
        let svc = connect().unwrap();
        assert_eq!(
            svc.info(),
            format!("WebSocket ws://127.0.0.1:{}/feed", port)
        );
        std::thread::sleep(Duration::from_millis(300));
        let q = |s: &str| answer_query(svc.root(), s);
        assert_eq!(
            q("messages(limit:3){sym px meta.venue}"),
            r#"{messages:[{sym:"ETH",px:10.25,meta.venue:"x"},{sym:"BTC",px:101.5,meta.venue:"x"},{sym:null,px:null,meta.venue:null}]}"#
        );
        assert_eq!(
            q("messages(limit:1){text}"),
            r#"{messages:[{text:"plain text frame"}]}"#
        );
        assert_eq!(
            q("summary(field:\"sym\"){count values{value count}}"),
            r#"{summary:{count:4,values:[{value:"BTC",count:2},{value:"ETH",count:1}]}}"#
        );
        assert_eq!(
            q("summary(field:\"px\"){min max}"),
            "{summary:{min:10.25,max:101.5}}"
        );
        assert_eq!(q("status{connected}"), "{status:{connected:true}}");
    }
}
