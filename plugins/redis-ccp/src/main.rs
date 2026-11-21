//! redis-ccp: read-only CCP server for Redis.
//!
//! Env: CCP_HOST (127.0.0.1), CCP_PORT (6379), CCP_PASSWORD, CCP_USER (ACL), CCP_DB (0)
//!
//! Safety: only a fixed set of read commands is ever sent; keys and patterns
//! travel as binary-safe bulk strings, so caller text can never become a command.
//! Scans and result sizes are bounded.

mod resp;

use ccp_mcp::{Meta, Service, scrub};
use ccp_proto::engine::{Args, FieldResult, RootResolver};
use ccp_proto::query::{ArgValue, Selection};
use ccp_proto::value::Value;
use resp::{Conn, Resp};
use std::cell::RefCell;
use std::collections::HashSet;

const DEFAULT_LIMIT: usize = 100;
const MAX_LIMIT: usize = 1000;
const MAX_SCAN_CALLS: usize = 2000;

struct Redis {
    conn: RefCell<Conn>,
    password: String,
    version: String,
}

fn str_arg(a: &Args, k: &str) -> Option<String> {
    match a.get(k) {
        Some(ArgValue::Str(s)) => Some(s.clone()),
        Some(ArgValue::Int(i)) => Some(i.to_string()),
        _ => None,
    }
}

fn int_arg(a: &Args, k: &str) -> Option<i64> {
    match a.get(k) {
        Some(ArgValue::Int(i)) => Some(*i),
        _ => None,
    }
}

fn limit_arg(a: &Args, default: usize) -> usize {
    int_arg(a, "limit").map_or(default, |l| l.clamp(0, MAX_LIMIT as i64) as usize)
}

fn s(r: &Resp) -> Value {
    r.text().map_or(Value::Null, Value::Str)
}

/// Project `(field, value)` pairs through a selection: all when empty, otherwise
/// only the requested fields (`null` when absent).
fn pick(fields: &[(String, Value)], sub: &[Selection]) -> Value {
    if sub.is_empty() {
        return Value::Object(fields.to_vec());
    }
    Value::Object(
        sub.iter()
            .map(|sel| {
                let v = fields
                    .iter()
                    .find(|(k, _)| *k == sel.name)
                    .map_or(Value::Null, |(_, v)| v.clone());
                (sel.name.clone(), v)
            })
            .collect(),
    )
}

fn pairs(items: &[Resp]) -> Vec<(String, Value)> {
    items
        .chunks(2)
        .filter(|c| c.len() == 2)
        .map(|c| (c[0].text().unwrap_or_default(), s(&c[1])))
        .collect()
}

impl Redis {
    fn err(&self, e: impl ToString) -> String {
        scrub(&e.to_string(), &self.password)
    }

    fn cmd(&self, args: &[&[u8]]) -> Result<Resp, String> {
        self.conn.borrow_mut().cmd(args).map_err(|e| self.err(e))
    }

    fn pipe(&self, cmds: &[Vec<&[u8]>]) -> Result<Vec<Resp>, String> {
        self.conn
            .borrow_mut()
            .pipeline(cmds)
            .map_err(|e| self.err(e))
    }

    /// SCAN until `limit` distinct keys are found (bounded).
    fn scan(&self, pattern: &str, ty: Option<&str>, limit: usize) -> Result<Vec<Vec<u8>>, String> {
        let mut keys: Vec<Vec<u8>> = Vec::new();
        let mut seen: HashSet<Vec<u8>> = HashSet::new();
        let mut cursor = b"0".to_vec();
        for _ in 0..MAX_SCAN_CALLS {
            if keys.len() >= limit {
                break;
            }
            let mut args: Vec<&[u8]> = vec![
                b"SCAN",
                &cursor,
                b"MATCH",
                pattern.as_bytes(),
                b"COUNT",
                b"1000",
            ];
            if let Some(t) = ty {
                args.push(b"TYPE");
                args.push(t.as_bytes());
            }
            let r = self.cmd(&args)?;
            let parts = r.items();
            let next = match parts.first() {
                Some(Resp::Bulk(Some(c))) => c.clone(),
                _ => return Err("unexpected SCAN reply".into()),
            };
            for k in parts.get(1).map_or(&[][..], |a| a.items()) {
                if let Resp::Bulk(Some(k)) = k
                    && keys.len() < limit
                    && seen.insert(k.clone())
                {
                    keys.push(k.clone());
                }
            }
            if next == b"0" {
                break;
            }
            cursor = next;
        }
        Ok(keys)
    }

    fn q_info(&self, args: &Args, sub: &[Selection]) -> Result<Value, String> {
        let section = str_arg(args, "section");
        let mut a: Vec<&[u8]> = vec![b"INFO"];
        if let Some(sec) = &section {
            a.push(sec.as_bytes());
        }
        let text = self.cmd(&a)?.text().unwrap_or_default();
        let fields: Vec<(String, Value)> = text
            .lines()
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .filter_map(|l| l.split_once(':'))
            .map(|(k, v)| {
                let val = v
                    .trim_end()
                    .parse::<i64>()
                    .map(Value::Int)
                    .unwrap_or_else(|_| Value::Str(v.trim_end().to_string()));
                (k.to_string(), val)
            })
            .collect();
        Ok(pick(&fields, sub))
    }

    fn q_keys(&self, args: &Args, sub: &[Selection]) -> Result<Value, String> {
        let pattern = str_arg(args, "match").unwrap_or_else(|| "*".into());
        let ty = str_arg(args, "type");
        let keys = self.scan(&pattern, ty.as_deref(), limit_arg(args, DEFAULT_LIMIT))?;
        let want = |n: &str| sub.iter().any(|x| x.name == n);
        let (types, ttls) = (
            if want("type") || want("size") {
                Some(
                    self.pipe(
                        &keys
                            .iter()
                            .map(|k| vec![&b"TYPE"[..], k])
                            .collect::<Vec<_>>(),
                    )?,
                )
            } else {
                None
            },
            if want("ttl") {
                Some(
                    self.pipe(
                        &keys
                            .iter()
                            .map(|k| vec![&b"TTL"[..], k])
                            .collect::<Vec<_>>(),
                    )?,
                )
            } else {
                None
            },
        );
        let sizes = if want("size") {
            let types = types.as_ref().unwrap();
            let cmds: Vec<Vec<&[u8]>> = keys
                .iter()
                .zip(types)
                .map(|(k, t)| {
                    let c: &[u8] = match t.text().as_deref() {
                        Some("string") => b"STRLEN",
                        Some("hash") => b"HLEN",
                        Some("list") => b"LLEN",
                        Some("set") => b"SCARD",
                        Some("zset") => b"ZCARD",
                        Some("stream") => b"XLEN",
                        _ => b"EXISTS",
                    };
                    vec![c, k]
                })
                .collect();
            Some(self.pipe(&cmds)?)
        } else {
            None
        };
        let rows = keys
            .iter()
            .enumerate()
            .map(|(i, k)| {
                let mut f = vec![(
                    "key".to_string(),
                    Value::Str(String::from_utf8_lossy(k).into_owned()),
                )];
                if let Some(t) = &types {
                    f.push(("type".into(), s(&t[i])));
                }
                if let Some(t) = &ttls {
                    f.push(("ttl".into(), t[i].int().map_or(Value::Null, Value::Int)));
                }
                if let Some(z) = &sizes {
                    f.push(("size".into(), z[i].int().map_or(Value::Null, Value::Int)));
                }
                pick(&f, sub)
            })
            .collect();
        Ok(Value::List(rows))
    }

    fn fields_cmd<'a>(&self, key: &'a [u8], sub: &'a [Selection]) -> Vec<&'a [u8]> {
        let mut c: Vec<&[u8]> = if sub.is_empty() {
            vec![b"HGETALL", key]
        } else {
            vec![b"HMGET", key]
        };
        if !sub.is_empty() {
            c.extend(sub.iter().map(|x| x.name.as_bytes()));
        }
        c
    }

    fn hash_value(&self, reply: &Resp, sub: &[Selection]) -> Value {
        if sub.is_empty() {
            Value::Object(pairs(reply.items()))
        } else {
            Value::Object(
                sub.iter()
                    .zip(reply.items())
                    .map(|(sel, v)| (sel.name.clone(), s(v)))
                    .collect(),
            )
        }
    }

    fn q_hash(&self, args: &Args, sub: &[Selection]) -> Result<Value, String> {
        let key = str_arg(args, "key").ok_or("hash needs key:\"...\"")?;
        let r = self.cmd(&self.fields_cmd(key.as_bytes(), sub))?;
        Ok(self.hash_value(&r, sub))
    }

    fn q_hashes(&self, args: &Args, sub: &[Selection]) -> Result<Value, String> {
        let pattern = str_arg(args, "match").unwrap_or_else(|| "*".into());
        let keys = self.scan(&pattern, Some("hash"), limit_arg(args, 20))?;
        // `_key` is a pseudo-field: fetch only the real hash fields.
        let real: Vec<Selection> = sub.iter().filter(|x| x.name != "_key").cloned().collect();
        let want_key = sub.is_empty() || real.len() != sub.len();
        let fetch_all = sub.is_empty();
        let cmds: Vec<Vec<&[u8]>> = keys
            .iter()
            .map(|k| self.fields_cmd(k, if fetch_all { &[] } else { &real }))
            .collect();
        let replies = self.pipe(&cmds)?;
        let rows = keys
            .iter()
            .zip(&replies)
            .map(|(k, r)| {
                let Value::Object(mut f) = self.hash_value(r, if fetch_all { &[] } else { &real })
                else {
                    unreachable!()
                };
                if want_key {
                    f.insert(
                        0,
                        (
                            "_key".into(),
                            Value::Str(String::from_utf8_lossy(k).into_owned()),
                        ),
                    );
                }
                Value::Object(f)
            })
            .collect();
        Ok(Value::List(rows))
    }

    fn q_get(&self, args: &Args) -> Result<Value, String> {
        let key = str_arg(args, "key").ok_or("get needs key:\"...\"")?;
        Ok(s(&self.cmd(&[b"GET", key.as_bytes()])?))
    }

    fn q_mget(&self, args: &Args, sub: &[Selection]) -> Result<Value, String> {
        let keys = str_arg(args, "keys").ok_or("mget needs keys:\"a,b,c\"")?;
        let keys: Vec<&str> = keys
            .split(',')
            .map(str::trim)
            .filter(|k| !k.is_empty())
            .take(MAX_LIMIT)
            .collect();
        let mut c: Vec<&[u8]> = vec![b"MGET"];
        c.extend(keys.iter().map(|k| k.as_bytes()));
        let r = self.cmd(&c)?;
        Ok(Value::List(
            keys.iter()
                .zip(r.items())
                .map(|(k, v)| {
                    pick(
                        &[
                            ("key".into(), Value::Str((*k).into())),
                            ("value".into(), s(v)),
                        ],
                        sub,
                    )
                })
                .collect(),
        ))
    }

    /// Absolute `[start, stop]` for a range command, bounded to MAX_LIMIT items.
    fn range(&self, len_cmd: &[u8], key: &str, args: &Args) -> Result<(i64, i64), String> {
        let len = self.cmd(&[len_cmd, key.as_bytes()])?.int().unwrap_or(0);
        let norm = |i: i64| if i < 0 { (len + i).max(0) } else { i };
        let start = norm(int_arg(args, "start").unwrap_or(0));
        let stop = norm(int_arg(args, "stop").unwrap_or(DEFAULT_LIMIT as i64 - 1));
        Ok((start, stop.min(start + MAX_LIMIT as i64 - 1).min(len - 1)))
    }

    fn q_list(&self, args: &Args) -> Result<Value, String> {
        let key = str_arg(args, "key").ok_or("list needs key:\"...\"")?;
        let (a, b) = self.range(b"LLEN", &key, args)?;
        if b < a {
            return Ok(Value::List(vec![]));
        }
        let r = self.cmd(&[
            b"LRANGE",
            key.as_bytes(),
            a.to_string().as_bytes(),
            b.to_string().as_bytes(),
        ])?;
        Ok(Value::List(r.items().iter().map(s).collect()))
    }

    fn q_members(&self, args: &Args) -> Result<Value, String> {
        let key = str_arg(args, "key").ok_or("members needs key:\"...\"")?;
        let limit = limit_arg(args, DEFAULT_LIMIT);
        let mut out: Vec<Value> = Vec::new();
        let mut cursor = b"0".to_vec();
        for _ in 0..MAX_SCAN_CALLS {
            if out.len() >= limit {
                break;
            }
            let r = self.cmd(&[b"SSCAN", key.as_bytes(), &cursor, b"COUNT", b"1000"])?;
            let parts = r.items();
            let next = match parts.first() {
                Some(Resp::Bulk(Some(c))) => c.clone(),
                _ => return Err("unexpected SSCAN reply".into()),
            };
            for m in parts.get(1).map_or(&[][..], |a| a.items()) {
                if out.len() < limit {
                    out.push(s(m));
                }
            }
            if next == b"0" {
                break;
            }
            cursor = next;
        }
        Ok(Value::List(out))
    }

    fn q_zrange(&self, args: &Args, sub: &[Selection]) -> Result<Value, String> {
        let key = str_arg(args, "key").ok_or("zrange needs key:\"...\"")?;
        let (a, b) = self.range(b"ZCARD", &key, args)?;
        if b < a {
            return Ok(Value::List(vec![]));
        }
        let rev = matches!(args.get("reverse"), Some(ArgValue::Bool(true)));
        let (a, b) = (a.to_string(), b.to_string());
        let mut c: Vec<&[u8]> = vec![b"ZRANGE", key.as_bytes(), a.as_bytes(), b.as_bytes()];
        if rev {
            c.push(b"REV");
        }
        c.push(b"WITHSCORES");
        let r = self.cmd(&c)?;
        Ok(Value::List(
            r.items()
                .chunks(2)
                .filter(|p| p.len() == 2)
                .map(|p| {
                    let score = p[1]
                        .text()
                        .and_then(|t| t.parse::<f64>().ok())
                        .map_or(Value::Null, Value::Float);
                    pick(&[("member".into(), s(&p[0])), ("score".into(), score)], sub)
                })
                .collect(),
        ))
    }

    fn q_stream(&self, args: &Args, sub: &[Selection]) -> Result<Value, String> {
        let key = str_arg(args, "key").ok_or("stream needs key:\"...\"")?;
        let count = limit_arg(args, 10).to_string();
        let latest = !matches!(args.get("oldest"), Some(ArgValue::Bool(true)));
        let c: Vec<&[u8]> = if latest {
            vec![
                b"XREVRANGE",
                key.as_bytes(),
                b"+",
                b"-",
                b"COUNT",
                count.as_bytes(),
            ]
        } else {
            vec![
                b"XRANGE",
                key.as_bytes(),
                b"-",
                b"+",
                b"COUNT",
                count.as_bytes(),
            ]
        };
        let r = self.cmd(&c)?;
        Ok(Value::List(
            r.items()
                .iter()
                .map(|e| {
                    let e = e.items();
                    let mut f = vec![("id".to_string(), e.first().map_or(Value::Null, s))];
                    if let Some(kv) = e.get(1) {
                        f.extend(pairs(kv.items()));
                    }
                    pick(&f, sub)
                })
                .collect(),
        ))
    }
}

impl RootResolver for Redis {
    fn resolve_root(&self, name: &str, args: &Args, sub: &[Selection]) -> FieldResult {
        let r = match name {
            "info" => self.q_info(args, sub),
            "dbsize" => self
                .cmd(&[b"DBSIZE"])
                .map(|r| r.int().map_or(Value::Null, Value::Int)),
            "keys" => self.q_keys(args, sub),
            "get" => self.q_get(args),
            "mget" => self.q_mget(args, sub),
            "hash" => self.q_hash(args, sub),
            "hashes" => self.q_hashes(args, sub),
            "list" => self.q_list(args),
            "members" => self.q_members(args),
            "zrange" => self.q_zrange(args, sub),
            "stream" => self.q_stream(args, sub),
            _ => return FieldResult::Missing,
        };
        match r {
            Ok(v) => FieldResult::Scalar(v),
            Err(e) => FieldResult::Error(e),
        }
    }
}

const SCHEMA: &str = "\
root info(section:Str):Info\n\
root dbsize:Int\n\
root keys(match:Str,type:Str,limit:Int):[Key]\n\
root get(key:Str):Str\n\
root mget(keys:Str):[Entry]\n\
root hash(key:Str):Hash\n\
root hashes(match:Str,limit:Int):[Hash]\n\
root list(key:Str,start:Int,stop:Int):[Str]\n\
root members(key:Str,limit:Int):[Str]\n\
root zrange(key:Str,start:Int,stop:Int,reverse:Bool):[ZItem]\n\
root stream(key:Str,limit:Int,oldest:Bool):[Event]\n\
type Info{redis_version:Str,used_memory:Int,connected_clients:Int,...any INFO field}\n\
type Key{key:Str,type:Str,ttl:Int,size:Int}\n\
type Entry{key:Str,value:Str}\n\
type Hash{_key:Str,...any hash field}\n\
type ZItem{member:Str,score:Float}\n\
type Event{id:Str,...any stream field}";

impl Service for Redis {
    fn schema(&self) -> String {
        SCHEMA.to_string()
    }
    fn root(&self) -> &dyn RootResolver {
        self
    }
    fn info(&self) -> String {
        let n = self
            .cmd(&[b"DBSIZE"])
            .ok()
            .and_then(|r| r.int())
            .unwrap_or(0);
        format!("Redis {} ({} keys)", self.version, n)
    }
    fn hint(&self) -> String {
        "Roots: info, dbsize, keys, get, mget, hash, hashes, list, members, zrange, stream.".into()
    }
}

fn connect() -> Result<Box<dyn Service>, String> {
    let host = std::env::var("CCP_HOST").unwrap_or_else(|_| "127.0.0.1".into());
    let port: u16 = match std::env::var("CCP_PORT") {
        Ok(p) => p
            .parse()
            .map_err(|_| "CCP_PORT is not a valid port".to_string())?,
        Err(_) => 6379,
    };
    let password = std::env::var("CCP_PASSWORD").unwrap_or_default();
    let mut conn = Conn::connect(&host, port)
        .map_err(|e| format!("cannot connect to {}:{}: {}", host, port, e))?;
    let fail = |e: String| scrub(&e, &password);
    if !password.is_empty() {
        match std::env::var("CCP_USER") {
            Ok(u) => conn.cmd(&[b"AUTH", u.as_bytes(), password.as_bytes()]),
            Err(_) => conn.cmd(&[b"AUTH", password.as_bytes()]),
        }
        .map_err(fail)?;
    }
    if let Ok(db) = std::env::var("CCP_DB") {
        conn.cmd(&[b"SELECT", db.as_bytes()])
            .map_err(|e| scrub(&e, &password))?;
    }
    let server = conn
        .cmd(&[b"INFO", b"server"])
        .map_err(|e| scrub(&e, &password))?
        .text()
        .unwrap_or_default();
    let version = server
        .lines()
        .find_map(|l| l.strip_prefix("redis_version:"))
        .unwrap_or("?")
        .trim()
        .to_string();
    Ok(Box::new(Redis {
        conn: RefCell::new(conn),
        password,
        version,
    }))
}

fn main() {
    ccp_mcp::main(
        Meta {
            name: "redis-ccp",
            kind: "Redis",
            usage: "Roots: keys(match:\"user:*\"){key type ttl}, get(key), hash(key){field..}, \
hashes(match:\"user:*\"){_key name} reads many hashes like table rows, list, members, zrange, stream, info(section:\"memory\"){used_memory}.",
            example: "hashes(match:\"user:*\",limit:5){_key name email}",
        },
        connect,
    );
}
