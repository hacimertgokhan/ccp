//! Event streams (WebSocket, Socket.IO, ...) for models.
//!
//! A background thread keeps the connection open and fills a ring buffer. The
//! model never sees the firehose: it asks for the last N messages, waits for new
//! ones, or asks for a summary, and only the fields it names come back.

#[cfg(feature = "ws")]
pub mod ws;

use ccp_mcp::{Meta, Service};
use ccp_proto::engine::{Args, FieldResult, RootResolver};
use ccp_proto::query::{ArgValue, Selection};
use ccp_proto::value::Value;
use serde_json::Value as J;
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub const MAX_LIMIT: usize = 1000;
pub const MAX_WAIT_SECS: u64 = 30;

pub struct Message {
    pub seq: u64,
    pub ts_ms: u64,
    pub event: Option<String>,
    pub raw: String,
    json: OnceLock<Option<J>>,
}

impl Message {
    /// Parsed lazily: a busy stream costs only a copy until someone asks for fields.
    pub fn json(&self) -> Option<&J> {
        self.json
            .get_or_init(|| serde_json::from_str(&self.raw).ok())
            .as_ref()
    }
}

#[derive(Default)]
struct Inner {
    msgs: VecDeque<Arc<Message>>,
    next_seq: u64,
    bytes_buffered: usize,
    total: u64,
    total_bytes: u64,
    connected: bool,
    reconnects: u64,
    error: Option<String>,
    stop: bool,
}

pub struct Buffer {
    inner: Mutex<Inner>,
    cv: Condvar,
    cap_msgs: usize,
    cap_bytes: usize,
    started: Instant,
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

impl Buffer {
    /// `cap_msgs` messages / `cap_bytes` bytes, whichever is hit first (oldest dropped).
    pub fn new(cap_msgs: usize, cap_bytes: usize) -> Arc<Buffer> {
        Arc::new(Buffer {
            inner: Mutex::new(Inner::default()),
            cv: Condvar::new(),
            cap_msgs,
            cap_bytes,
            started: Instant::now(),
        })
    }

    pub fn push(&self, event: Option<String>, raw: String) {
        let mut g = self.inner.lock().unwrap();
        g.next_seq += 1;
        g.total += 1;
        g.total_bytes += raw.len() as u64;
        g.bytes_buffered += raw.len();
        let m = Arc::new(Message {
            seq: g.next_seq,
            ts_ms: now_ms(),
            event,
            raw,
            json: OnceLock::new(),
        });
        g.msgs.push_back(m);
        while g.msgs.len() > self.cap_msgs
            || (g.bytes_buffered > self.cap_bytes && g.msgs.len() > 1)
        {
            if let Some(old) = g.msgs.pop_front() {
                g.bytes_buffered -= old.raw.len();
            }
        }
        drop(g);
        self.cv.notify_all();
    }

    pub fn set_connected(&self, connected: bool, error: Option<String>) {
        let mut g = self.inner.lock().unwrap();
        if connected && !g.connected && g.total > 0 || (connected && g.error.is_some()) {
            g.reconnects += 1;
        }
        g.connected = connected;
        g.error = error;
        drop(g);
        self.cv.notify_all();
    }

    pub fn stopped(&self) -> bool {
        self.inner.lock().unwrap().stop
    }

    pub fn shutdown(&self) {
        self.inner.lock().unwrap().stop = true;
        self.cv.notify_all();
    }

    /// Wait for the first connection attempt: `Ok` once connected, `Err` with the reason otherwise.
    pub fn wait_first(&self, timeout: Duration) -> Result<(), String> {
        let deadline = Instant::now() + timeout;
        let mut g = self.inner.lock().unwrap();
        loop {
            if g.connected {
                return Ok(());
            }
            if let Some(e) = &g.error {
                return Err(e.clone());
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err("timed out connecting".into());
            }
            g = self.cv.wait_timeout(g, left).unwrap().0;
        }
    }
}

/// Run `session` (one connection, start to finish) in a background thread,
/// reconnecting with backoff. `session` must call `set_connected(true, None)`
/// once the handshake is done and return the reason when the connection ends.
pub fn spawn_reader<F>(buf: Arc<Buffer>, mut session: F)
where
    F: FnMut(&Buffer) -> Result<(), String> + Send + 'static,
{
    std::thread::spawn(move || {
        let mut backoff = 1u64;
        while !buf.stopped() {
            let started = Instant::now();
            let why = session(&buf)
                .err()
                .unwrap_or_else(|| "connection closed".into());
            buf.set_connected(false, Some(why));
            if buf.stopped() {
                break;
            }
            if started.elapsed() > Duration::from_secs(30) {
                backoff = 1;
            }
            std::thread::sleep(Duration::from_secs(backoff));
            backoff = (backoff * 2).min(30);
        }
    });
}

struct Filter {
    event: Option<String>,
    contains: Option<String>,
}

impl Filter {
    fn from(args: &Args) -> Filter {
        let s = |k: &str| match args.get(k) {
            Some(ArgValue::Str(v)) if !v.is_empty() => Some(v.clone()),
            _ => None,
        };
        Filter {
            event: s("event"),
            contains: s("contains"),
        }
    }
    fn ok(&self, m: &Message) -> bool {
        self.event
            .as_ref()
            .is_none_or(|e| m.event.as_deref() == Some(e.as_str()))
            && self
                .contains
                .as_ref()
                .is_none_or(|c| m.raw.contains(c.as_str()))
    }
}

fn int(args: &Args, k: &str) -> Option<i64> {
    match args.get(k) {
        Some(ArgValue::Int(i)) => Some(*i),
        _ => None,
    }
}

pub struct StreamService {
    buf: Arc<Buffer>,
    info: String,
    hint: String,
}

impl StreamService {
    pub fn new(buf: Arc<Buffer>, info: String, hint: String) -> StreamService {
        StreamService { buf, info, hint }
    }

    /// Matching messages. `wait == 0`: the latest ones already buffered.
    /// `wait > 0`: block until `limit` NEW matching messages arrive or time runs out.
    fn collect(&self, args: &Args, limit: usize, default_wait: u64) -> (Vec<Arc<Message>>, f64) {
        let f = Filter::from(args);
        let wait =
            int(args, "wait").map_or(default_wait, |w| w.clamp(0, MAX_WAIT_SECS as i64) as u64);
        let since = int(args, "since").map(|s| s.max(0) as u64);
        let mut g = self.buf.inner.lock().unwrap();
        if wait == 0 {
            let it = g
                .msgs
                .iter()
                .filter(|m| since.is_none_or(|s| m.seq > s) && f.ok(m));
            let out: Vec<Arc<Message>> = if since.is_some() {
                it.take(limit).cloned().collect()
            } else {
                let mut v: Vec<_> = g
                    .msgs
                    .iter()
                    .rev()
                    .filter(|m| f.ok(m))
                    .take(limit)
                    .cloned()
                    .collect();
                v.reverse();
                v
            };
            let span = match (out.first(), out.last()) {
                (Some(a), Some(b)) => (b.ts_ms - a.ts_ms) as f64 / 1000.0,
                _ => 0.0,
            };
            return (out, span);
        }
        let start_seq = since.unwrap_or(g.next_seq);
        let t0 = Instant::now();
        let deadline = t0 + Duration::from_secs(wait);
        loop {
            let hits = g
                .msgs
                .iter()
                .filter(|m| m.seq > start_seq && f.ok(m))
                .count();
            let left = deadline.saturating_duration_since(Instant::now());
            if hits >= limit || left.is_zero() || g.stop {
                let out: Vec<_> = g
                    .msgs
                    .iter()
                    .filter(|m| m.seq > start_seq && f.ok(m))
                    .take(limit)
                    .cloned()
                    .collect();
                return (out, t0.elapsed().as_secs_f64());
            }
            g = self.buf.cv.wait_timeout(g, left).unwrap().0;
        }
    }

    fn q_messages(&self, args: &Args, sub: &[Selection]) -> Value {
        let limit = int(args, "limit").map_or(20, |l| l.clamp(0, MAX_LIMIT as i64) as usize);
        let (msgs, _) = self.collect(args, limit, 0);
        Value::List(msgs.iter().map(|m| message_value(m, sub)).collect())
    }

    fn q_summary(&self, args: &Args, sub: &[Selection]) -> Value {
        let (msgs, secs) = self.collect(args, MAX_LIMIT * 10, 0);
        let bytes: usize = msgs.iter().map(|m| m.raw.len()).sum();
        let mut fields: Vec<(String, Value)> = vec![
            ("count".into(), Value::Int(msgs.len() as i64)),
            ("bytes".into(), Value::Int(bytes as i64)),
            (
                "per_second".into(),
                if secs > 0.0 {
                    Value::Float((msgs.len() as f64 / secs * 100.0).round() / 100.0)
                } else {
                    Value::Null
                },
            ),
            (
                "first_seq".into(),
                msgs.first()
                    .map_or(Value::Null, |m| Value::Int(m.seq as i64)),
            ),
            (
                "last_seq".into(),
                msgs.last()
                    .map_or(Value::Null, |m| Value::Int(m.seq as i64)),
            ),
        ];
        if let Some(ArgValue::Str(path)) = args.get("field") {
            let top = int(args, "top").map_or(10, |t| t.clamp(1, 100) as usize);
            let mut counts: HashMap<String, i64> = HashMap::new();
            let (mut mn, mut mx, mut sum, mut n) = (f64::MAX, f64::MIN, 0.0, 0i64);
            let mut last: Option<String> = None;
            for m in &msgs {
                let Some(v) = m.json().and_then(|j| ccp_mcp::lookup(j, path)) else {
                    continue;
                };
                let text = match v {
                    J::String(s) => s.clone(),
                    other => other.to_string(),
                };
                if let Some(x) = v.as_f64() {
                    mn = mn.min(x);
                    mx = mx.max(x);
                    sum += x;
                    n += 1;
                }
                last = Some(text.clone());
                *counts.entry(text).or_default() += 1;
            }
            let mut vals: Vec<(String, i64)> = counts.into_iter().collect();
            vals.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
            fields.push((
                "values".into(),
                Value::List(
                    vals.into_iter()
                        .take(top)
                        .map(|(v, c)| {
                            Value::Object(vec![
                                ("value".into(), Value::Str(v)),
                                ("count".into(), Value::Int(c)),
                            ])
                        })
                        .collect(),
                ),
            ));
            if n > 0 {
                fields.push(("min".into(), Value::Float(mn)));
                fields.push(("max".into(), Value::Float(mx)));
                fields.push(("avg".into(), Value::Float(sum / n as f64)));
            }
            fields.push(("last".into(), last.map_or(Value::Null, Value::Str)));
        }
        pick(&fields, sub)
    }

    fn q_status(&self, sub: &[Selection]) -> Value {
        let g = self.buf.inner.lock().unwrap();
        let fields = vec![
            ("connected".to_string(), Value::Bool(g.connected)),
            ("reconnects".into(), Value::Int(g.reconnects as i64)),
            ("buffered".into(), Value::Int(g.msgs.len() as i64)),
            ("total".into(), Value::Int(g.total as i64)),
            ("bytes".into(), Value::Int(g.total_bytes as i64)),
            (
                "uptime_s".into(),
                Value::Int(self.buf.started.elapsed().as_secs() as i64),
            ),
            (
                "error".into(),
                g.error.clone().map_or(Value::Null, Value::Str),
            ),
        ];
        pick(&fields, sub)
    }
}

fn pick(fields: &[(String, Value)], sub: &[Selection]) -> Value {
    if sub.is_empty() {
        return Value::Object(fields.to_vec());
    }
    Value::Object(
        sub.iter()
            .map(|s| {
                (
                    s.name.clone(),
                    fields
                        .iter()
                        .find(|(k, _)| *k == s.name)
                        .map_or(Value::Null, |(_, v)| v.clone()),
                )
            })
            .collect(),
    )
}

fn message_value(m: &Message, sub: &[Selection]) -> Value {
    let meta = |name: &str| -> Option<Value> {
        Some(match name {
            "_seq" => Value::Int(m.seq as i64),
            "_ts" => Value::Int(m.ts_ms as i64),
            "_event" => m.event.clone().map_or(Value::Null, Value::Str),
            "_raw" => Value::Str(m.raw.clone()),
            "_size" => Value::Int(m.raw.len() as i64),
            _ => return None,
        })
    };
    if sub.is_empty() {
        return Value::Object(vec![
            ("_seq".into(), Value::Int(m.seq as i64)),
            (
                "_event".into(),
                m.event.clone().map_or(Value::Null, Value::Str),
            ),
            ("_raw".into(), Value::Str(m.raw.clone())),
        ]);
    }
    Value::Object(
        sub.iter()
            .map(|s| {
                let v = meta(&s.name).unwrap_or_else(|| match m.json() {
                    Some(j) => ccp_mcp::project_field(j, s),
                    // not JSON: the whole payload is reachable as `text`
                    None if s.name == "text" => Value::Str(m.raw.clone()),
                    None => Value::Null,
                });
                (s.name.clone(), v)
            })
            .collect(),
    )
}

impl RootResolver for StreamService {
    fn resolve_root(&self, name: &str, args: &Args, sub: &[Selection]) -> FieldResult {
        FieldResult::Scalar(match name {
            "messages" => self.q_messages(args, sub),
            "summary" => self.q_summary(args, sub),
            "status" => self.q_status(sub),
            _ => return FieldResult::Missing,
        })
    }
}

pub const SCHEMA: &str = "\
root messages(limit:Int,wait:Int,since:Int,event:Str,contains:Str):[Message]\n\
root summary(wait:Int,field:Str,top:Int,event:Str,contains:Str):Summary\n\
root status:Status\n\
type Message{_seq:Int,_ts:Int,_event:Str,_raw:Str,_size:Int,text:Str,...any payload field; a.b.c paths ok}\n\
type Summary{count:Int,bytes:Int,per_second:Float,first_seq:Int,last_seq:Int,values:[Value],min:Float,max:Float,avg:Float,last:Str}\n\
type Value{value:Str,count:Int}\n\
type Status{connected:Bool,reconnects:Int,buffered:Int,total:Int,bytes:Int,uptime_s:Int,error:Str}";

impl Service for StreamService {
    fn schema(&self) -> String {
        SCHEMA.to_string()
    }
    fn root(&self) -> &dyn RootResolver {
        self
    }
    fn info(&self) -> String {
        self.info.clone()
    }
    fn hint(&self) -> String {
        self.hint.clone()
    }
}

/// `Meta` shared by the stream servers (name/kind differ per server).
pub fn meta(name: &'static str, kind: &'static str) -> Meta {
    Meta {
        name,
        kind,
        usage: "The server listens in the background; you never receive the raw stream. messages(limit:20){fields} \
returns the latest messages, messages(wait:10,limit:5) waits for new ones, since:<_seq> polls incrementally, \
summary(wait:10,field:\"price\"){count per_second values{value count} min max avg} reports instead of listing, status{connected} checks the link.",
        example: "messages(limit:10,event:\"trade\"){_seq price symbol}",
    }
}

/// Read a size/capacity env var with a default.
pub fn env_usize(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ccp_proto::serve::answer_query;

    fn svc() -> (Arc<Buffer>, StreamService) {
        let b = Buffer::new(5, 1 << 20);
        (
            b.clone(),
            StreamService::new(b, "test".into(), String::new()),
        )
    }

    #[test]
    fn latest_messages_with_field_selection_and_ring_cap() {
        let (b, s) = svc();
        for i in 1..=7 {
            b.push(
                Some("tick".into()),
                format!(r#"{{"price":{},"sym":"A","meta":{{"n":{}}}}}"#, i * 10, i),
            );
        }
        // ring keeps the last 5 (seq 3..7)
        assert_eq!(
            answer_query(&s, "messages(limit:2){_seq price meta.n}"),
            "{messages:[{_seq:6,price:60,meta.n:6},{_seq:7,price:70,meta.n:7}]}"
        );
        assert_eq!(
            answer_query(&s, "messages(since:5){_seq}"),
            "{messages:[{_seq:6},{_seq:7}]}"
        );
        assert_eq!(
            answer_query(&s, "messages(limit:1,contains:\"\\\"price\\\":40\"){price}"),
            "{messages:[{price:40}]}"
        );
        assert_eq!(
            answer_query(&s, "messages(event:\"nope\"){_seq}"),
            "{messages:[]}"
        );
    }

    #[test]
    fn non_json_text_and_defaults() {
        let (b, s) = svc();
        b.push(None, "hello world".into());
        assert_eq!(
            answer_query(&s, "messages{text price}"),
            "{messages:[{text:\"hello world\",price:null}]}"
        );
        assert_eq!(
            answer_query(&s, "messages"),
            "{messages:[{_seq:1,_event:null,_raw:\"hello world\"}]}"
        );
    }

    #[test]
    fn summary_counts_and_numeric_stats() {
        let (b, s) = svc();
        for (t, p) in [("buy", 10), ("sell", 20), ("buy", 30)] {
            b.push(None, format!(r#"{{"side":"{}","price":{}}}"#, t, p));
        }
        assert_eq!(
            answer_query(&s, "summary(field:\"side\"){count values{value count}}"),
            r#"{summary:{count:3,values:[{value:"buy",count:2},{value:"sell",count:1}]}}"#
        );
        assert_eq!(
            answer_query(&s, "summary(field:\"price\"){min max avg last}"),
            r#"{summary:{min:10,max:30,avg:20,last:"30"}}"#
        );
    }

    #[test]
    fn wait_returns_new_messages_only() {
        let (b, s) = svc();
        b.push(None, r#"{"old":1}"#.into());
        let b2 = b.clone();
        let t = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(150));
            b2.push(None, r#"{"new":1}"#.into());
        });
        let t0 = Instant::now();
        assert_eq!(
            answer_query(&s, "messages(wait:5,limit:1){new}"),
            "{messages:[{new:1}]}"
        );
        assert!(t0.elapsed() < Duration::from_secs(3));
        t.join().unwrap();
        // timeout path returns what it has
        assert_eq!(
            answer_query(&s, "messages(wait:1,limit:3){new}"),
            "{messages:[]}"
        );
    }

    #[test]
    fn status_reports_link_state() {
        let (b, s) = svc();
        b.set_connected(true, None);
        assert_eq!(
            answer_query(&s, "status{connected total}"),
            "{status:{connected:true,total:0}}"
        );
        b.set_connected(false, Some("boom".into()));
        assert_eq!(
            answer_query(&s, "status{connected error}"),
            "{status:{connected:false,error:\"boom\"}}"
        );
    }
}
