//! socketio-ccp: CCP server for a Socket.IO server (Engine.IO v4 over WebSocket).
//!
//! Connects as a read-only listener: it answers pings, collects events into a
//! ring buffer, and lets the model query them by event name, field and window.
//! It only emits what the operator configures in CCP_EMIT.
//!
//! Env: CCP_URL        http(s):// or ws(s):// base URL (required)
//!      CCP_PATH       Socket.IO path (default /socket.io/)
//!      CCP_NAMESPACE  namespace (default /)
//!      CCP_AUTH       JSON object sent as the connect `auth` payload, e.g. {"token":"..."}
//!      CCP_BEARER     token sent as `Authorization: Bearer ...`
//!      CCP_EMIT       JSON array of events sent after connecting, e.g. [["subscribe","room1"]]
//!      CCP_EVENTS     comma-separated event names to keep (default: all)
//!      CCP_BUFFER / CCP_BUFFER_MB   messages / megabytes kept (5000 / 32)

use ccp_mcp::{Service, scrub};
use ccp_stream::ws::{self, Socket, WsMessage};
use ccp_stream::{Buffer, StreamService, env_usize, spawn_reader};
use serde_json::Value as J;
use std::sync::Arc;
use std::time::Duration;

struct Config {
    url: String,
    ns: String,
    auth: Option<String>,
    bearer: Option<String>,
    emit: Vec<J>,
    events: Vec<String>,
}

fn send(sock: &mut Socket, text: String) -> Result<(), String> {
    sock.send(WsMessage::text(text)).map_err(|e| e.to_string())
}

fn next_text(sock: &mut Socket) -> Result<String, String> {
    loop {
        match sock.read() {
            Ok(WsMessage::Text(t)) => return Ok(t.as_str().to_string()),
            Ok(WsMessage::Close(_)) => return Err("closed by server".into()),
            Ok(_) => {}
            Err(e) if ws::is_idle(&e) => return Err("timed out waiting for the server".into()),
            Err(e) => return Err(e.to_string()),
        }
    }
}

/// `42/ns,12["event",...]` -> `("event", payload)`; `None` for other namespaces.
fn parse_event(body: &str, ns: &str) -> Option<(String, String)> {
    let rest = if let Some(r) = body.strip_prefix('/') {
        let (n, r) = r.split_once(',')?;
        if format!("/{}", n) != ns {
            return None;
        }
        r
    } else if ns == "/" {
        body
    } else {
        return None;
    };
    let rest = rest.trim_start_matches(|c: char| c.is_ascii_digit()); // optional ack id
    fast_event(rest).or_else(|| slow_event(rest))
}

/// One pass over `["event",payload]` without building a JSON tree: finds the event
/// name and the payload's byte range. Returns `None` for anything unusual
/// (several arguments, string payloads, escapes in the name), which the slow path handles.
fn fast_event(rest: &str) -> Option<(String, String)> {
    let b = rest.as_bytes();
    if b.len() < 4 || b[0] != b'[' || b[1] != b'"' || *b.last()? != b']' {
        return None;
    }
    let name_end = 2 + b[2..].iter().position(|&c| c == b'"' || c == b'\\')?;
    if b[name_end] == b'\\' {
        return None;
    }
    let event = &rest[2..name_end];
    match b.get(name_end + 1)? {
        b']' => return Some((event.to_string(), "null".into())),
        b',' => {}
        _ => return None,
    }
    let payload = &rest[name_end + 2..rest.len() - 1];
    if payload.starts_with('"') {
        return None; // string payloads are unquoted by the slow path
    }
    // a top-level comma would mean several arguments
    let (mut depth, mut in_str, mut esc) = (0i32, false, false);
    for &c in payload.as_bytes() {
        if in_str {
            if esc {
                esc = false;
            } else if c == b'\\' {
                esc = true;
            } else if c == b'"' {
                in_str = false;
            }
            continue;
        }
        match c {
            b'"' => in_str = true,
            b'{' | b'[' => depth += 1,
            b'}' | b']' => depth -= 1,
            b',' if depth == 0 => return None,
            _ => {}
        }
        if depth < 0 {
            return None;
        }
    }
    (depth == 0 && !in_str).then(|| (event.to_string(), payload.to_string()))
}

fn slow_event(rest: &str) -> Option<(String, String)> {
    let arr: Vec<J> = serde_json::from_str(rest).ok()?;
    let event = arr.first()?.as_str()?.to_string();
    let payload = match arr.len() {
        1 => "null".to_string(),
        2 => match &arr[1] {
            J::String(s) => s.clone(),
            other => other.to_string(),
        },
        _ => J::Array(arr[1..].to_vec()).to_string(),
    };
    Some((event, payload))
}

fn session(buf: &Buffer, cfg: &Config) -> Result<(), String> {
    let mut sock = ws::open(&cfg.url, cfg.bearer.as_deref())?;

    // Engine.IO open packet, then connect to the namespace.
    let open = next_text(&mut sock)?;
    if !open.starts_with('0') {
        return Err(format!(
            "not an Engine.IO v4 server (first packet: {})",
            open.chars().take(20).collect::<String>()
        ));
    }
    let prefix = if cfg.ns == "/" {
        String::new()
    } else {
        format!("{},", cfg.ns)
    };
    send(
        &mut sock,
        format!("40{}{}", prefix, cfg.auth.clone().unwrap_or_default()),
    )?;
    loop {
        let p = next_text(&mut sock)?;
        match p.as_bytes().first() {
            Some(b'2') => send(&mut sock, "3".into())?,
            Some(b'4') if p.as_bytes().get(1) == Some(&b'0') => break,
            Some(b'4') if p.as_bytes().get(1) == Some(&b'4') => {
                return Err(format!("connect refused: {}", &p[2..]));
            }
            _ => {}
        }
    }
    for ev in &cfg.emit {
        send(&mut sock, format!("42{}{}", prefix, ev))?;
    }
    buf.set_connected(true, None);

    loop {
        let t = match sock.read() {
            Ok(WsMessage::Text(t)) => t.as_str().to_string(),
            Ok(WsMessage::Close(_)) => return Err("closed by server".into()),
            Ok(_) => continue,
            Err(e) if ws::is_idle(&e) => {
                // quiet link: the server pings every ~25 s, so this means it is dead
                return Err("no data or pings from server".into());
            }
            Err(e) => return Err(e.to_string()),
        };
        match t.as_bytes().first() {
            Some(b'2') => send(&mut sock, "3".into())?,
            Some(b'1') => return Err("closed by server".into()),
            Some(b'4') => match t.as_bytes().get(1) {
                Some(b'2') => {
                    if let Some((event, payload)) = parse_event(&t[2..], &cfg.ns)
                        && (cfg.events.is_empty() || cfg.events.contains(&event))
                    {
                        buf.push(Some(event), payload);
                    }
                }
                Some(b'1') => return Err("namespace disconnected".into()),
                _ => {}
            },
            _ => {}
        }
        if buf.stopped() {
            return Ok(());
        }
    }
}

fn build_url(base: &str, path: &str) -> Result<String, String> {
    let ws = if let Some(r) = base.strip_prefix("https://") {
        format!("wss://{}", r)
    } else if let Some(r) = base.strip_prefix("http://") {
        format!("ws://{}", r)
    } else if base.starts_with("ws://") || base.starts_with("wss://") {
        base.to_string()
    } else {
        return Err("CCP_URL must start with http(s):// or ws(s)://".into());
    };
    let mut p = path.to_string();
    if !p.starts_with('/') {
        p.insert(0, '/');
    }
    if !p.ends_with('/') {
        p.push('/');
    }
    Ok(format!(
        "{}{}?EIO=4&transport=websocket",
        ws.trim_end_matches('/'),
        p
    ))
}

fn connect() -> Result<Box<dyn Service>, String> {
    let base = ccp_mcp::env_required("CCP_URL")?;
    let path = std::env::var("CCP_PATH").unwrap_or_else(|_| "/socket.io/".into());
    let ns = std::env::var("CCP_NAMESPACE").unwrap_or_else(|_| "/".into());
    let ns = if ns.starts_with('/') {
        ns
    } else {
        format!("/{}", ns)
    };
    let auth = match std::env::var("CCP_AUTH") {
        Ok(a) if !a.trim().is_empty() => {
            serde_json::from_str::<J>(&a)
                .map_err(|e| format!("CCP_AUTH is not valid JSON: {}", e))?;
            Some(a)
        }
        _ => None,
    };
    let emit: Vec<J> = match std::env::var("CCP_EMIT") {
        Ok(e) if !e.trim().is_empty() => serde_json::from_str(&e)
            .map_err(|e| format!("CCP_EMIT must be a JSON array of arrays: {}", e))?,
        _ => Vec::new(),
    };
    let events: Vec<String> = std::env::var("CCP_EVENTS")
        .map(|v| {
            v.split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(String::from)
                .collect()
        })
        .unwrap_or_default();
    let bearer = std::env::var("CCP_BEARER").ok().filter(|b| !b.is_empty());
    let url = build_url(&base, &path)?;
    let shown = ws::redact(&url);

    let secrets: Vec<String> = [bearer.clone(), auth.clone()]
        .into_iter()
        .flatten()
        .collect();
    let cfg = Config {
        url,
        ns,
        auth,
        bearer,
        emit,
        events,
    };
    let buf = Buffer::new(
        env_usize("CCP_BUFFER", 5000),
        env_usize("CCP_BUFFER_MB", 32) * 1024 * 1024,
    );
    spawn_reader(buf.clone(), move |b| {
        session(b, &cfg).map_err(|e| secrets.iter().fold(e, |m, s| scrub(&m, s)))
    });
    if let Err(e) = buf.wait_first(Duration::from_secs(12)) {
        buf.shutdown();
        return Err(e);
    }
    Ok(Box::new(StreamService::new(
        Arc::clone(&buf),
        format!("Socket.IO {}", shown),
        format!("Listening to Socket.IO at {}.", shown),
    )))
}

fn main() {
    ccp_mcp::main(ccp_stream::meta("socketio-ccp", "Socket.IO"), connect);
}

#[cfg(test)]
mod tests {
    use super::*;
    use ccp_proto::serve::answer_query;
    use std::net::TcpListener;
    use tungstenite::accept;

    #[test]
    fn parses_event_packets() {
        assert_eq!(
            parse_event(r#"["trade",{"px":1}]"#, "/"),
            Some(("trade".into(), r#"{"px":1}"#.into()))
        );
        assert_eq!(
            parse_event(r#"12["ack",1]"#, "/"),
            Some(("ack".into(), "1".into()))
        );
        assert_eq!(
            parse_event(r#"["hello"]"#, "/"),
            Some(("hello".into(), "null".into()))
        );
        assert_eq!(
            parse_event(r#"["multi",1,"b"]"#, "/"),
            Some(("multi".into(), r#"[1,"b"]"#.into()))
        );
        assert_eq!(
            parse_event(r#"/admin,["x",1]"#, "/admin"),
            Some(("x".into(), "1".into()))
        );
        assert_eq!(parse_event(r#"/admin,["x",1]"#, "/"), None);
        assert_eq!(parse_event(r#"["x",1]"#, "/admin"), None);
    }

    #[test]
    fn fast_and_slow_paths_agree() {
        for body in [
            r#"["trade",{"px":1,"s":"a,b]","n":[1,2]}]"#,
            r#"["t",[1,2,3]]"#,
            r#"["t",42]"#,
            r#"["t",null]"#,
            r#"["t","text"]"#,
            r#"["t",1,2]"#,
            r#"["t",{"a":1},{"b":2}]"#,
            r#"["we\"ird",1]"#,
            r#"["only"]"#,
        ] {
            // same event and same JSON content (the fast path also keeps key order)
            let norm = |r: Option<(String, String)>| {
                r.map(|(e, p)| (e, serde_json::from_str::<J>(&p).unwrap_or(J::String(p))))
            };
            let slow = norm(slow_event(body));
            let both = norm(fast_event(body).or_else(|| slow_event(body)));
            assert_eq!(slow, both, "{}", body);
        }
        assert!(
            fast_event(r#"["t",{"a":1}]"#).is_some(),
            "fast path must handle the common case"
        );
        assert!(fast_event(r#"["t",1,2]"#).is_none());
    }

    #[test]
    fn builds_urls() {
        assert_eq!(
            build_url("https://h.example/", "socket.io").unwrap(),
            "wss://h.example/socket.io/?EIO=4&transport=websocket"
        );
        assert_eq!(
            build_url("ws://h:3000", "/s/").unwrap(),
            "ws://h:3000/s/?EIO=4&transport=websocket"
        );
        assert!(build_url("ftp://x", "/").is_err());
    }

    #[test]
    fn full_engineio_session_against_a_protocol_mock() {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        std::thread::spawn(move || {
            let (s, _) = l.accept().unwrap();
            let mut ws = accept(s).unwrap();
            ws.send(WsMessage::text(r#"0{"sid":"abc","upgrades":[],"pingInterval":25000,"pingTimeout":20000,"maxPayload":1000000}"#)).unwrap();
            let c = ws.read().unwrap().into_text().unwrap();
            assert_eq!(c.as_str(), r#"40{"token":"t"}"#);
            ws.send(WsMessage::text(r#"40{"sid":"xyz"}"#)).unwrap();
            // operator-configured emit arrives right after the connect ack
            assert_eq!(
                ws.read().unwrap().into_text().unwrap().as_str(),
                r#"42["subscribe","btc"]"#
            );
            ws.send(WsMessage::text("2")).unwrap();
            assert_eq!(ws.read().unwrap().into_text().unwrap().as_str(), "3");
            ws.send(WsMessage::text(r#"42["trade",{"px":100.5,"sym":"BTC"}]"#))
                .unwrap();
            ws.send(WsMessage::text(r#"42["trade",{"px":101.5,"sym":"BTC"}]"#))
                .unwrap();
            ws.send(WsMessage::text(r#"42["status","ok"]"#)).unwrap();
            ws.send(WsMessage::text(r#"42/other,["trade",{"px":1}]"#))
                .unwrap();
            std::thread::sleep(Duration::from_secs(5));
        });
        unsafe {
            std::env::set_var("CCP_URL", format!("http://127.0.0.1:{}", port));
            std::env::set_var("CCP_AUTH", r#"{"token":"t"}"#);
            std::env::set_var("CCP_EMIT", r#"[["subscribe","btc"]]"#);
        }
        let svc = connect().unwrap();
        std::thread::sleep(Duration::from_millis(400));
        let q = |s: &str| answer_query(svc.root(), s);
        assert_eq!(
            q(r#"messages(event:"trade"){px sym}"#),
            r#"{messages:[{px:100.5,sym:"BTC"},{px:101.5,sym:"BTC"}]}"#
        );
        assert_eq!(
            q("messages(event:\"status\"){text}"),
            r#"{messages:[{text:"ok"}]}"#
        );
        assert_eq!(
            q(r#"summary(event:"trade",field:"px"){count min max avg}"#),
            "{summary:{count:2,min:100.5,max:101.5,avg:101}}"
        );
        // the foreign-namespace packet never reached the buffer
        assert_eq!(q("status{total}"), "{status:{total:3}}");
    }
}
