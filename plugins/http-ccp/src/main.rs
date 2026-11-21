//! http-ccp: read-only CCP server for REST/JSON APIs.
//!
//! One root, `get(path:"/users/1",<query params>){fields}`: the API answers with its
//! full JSON, the server keeps only the fields the query names, so a bloated
//! response costs the model a few tokens instead of thousands.
//!
//! Safety: GET only; `path` is joined to a fixed base URL (no scheme, host or
//! `..` allowed) and redirects are never followed, so the model cannot steer a
//! request, or your credentials, to another host.
//!
//! Env: CCP_URL      base URL, e.g. https://api.example.com/v1 (required)
//!      CCP_BEARER   token sent as `Authorization: Bearer ...`
//!      CCP_HEADERS  extra headers, `Name: value` separated by `;` or newlines
//!      CCP_TIMEOUT  seconds (default 15)     CCP_MAX_KB  response cap (default 4096)

use ccp_mcp::{Meta, Service, project, project_all, scrub};
use ccp_proto::engine::{Args, FieldResult, RootResolver};
use ccp_proto::query::{ArgValue, Selection};
use ccp_proto::value::Value;
use std::time::Duration;

struct Http {
    base: String,
    agent: ureq::Agent,
    headers: Vec<(String, String)>,
    max_bytes: u64,
    secrets: Vec<String>,
}

fn encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{:02X}", b)),
        }
    }
    out
}

/// `path` must stay a path: no scheme/authority, no traversal, no control chars.
fn check_path(path: &str) -> Result<(), String> {
    if !path.starts_with('/') || path.starts_with("//") || path.contains('\\') || path.contains('#')
    {
        return Err("path must start with a single `/` and be relative to the base URL".into());
    }
    if path.chars().any(|c| c.is_control() || c == ' ')
        || path.split('/').any(|s| s == ".." || s == "%2e%2e")
    {
        return Err("path contains forbidden characters or `..`".into());
    }
    Ok(())
}

impl Http {
    fn err(&self, e: impl ToString) -> String {
        self.secrets.iter().fold(e.to_string(), |m, s| scrub(&m, s))
    }

    fn get(&self, args: &Args, sub: &[Selection]) -> Result<Value, String> {
        let path = match args.get("path") {
            Some(ArgValue::Str(p)) => p.clone(),
            _ => return Err("get needs path:\"/...\"".into()),
        };
        check_path(&path)?;
        let mut query: Vec<String> = args
            .iter()
            .filter(|(k, _)| k.as_str() != "path")
            .map(|(k, v)| {
                let v = match v {
                    ArgValue::Int(i) => i.to_string(),
                    ArgValue::Str(s) => s.clone(),
                    ArgValue::Bool(b) => b.to_string(),
                };
                format!("{}={}", encode(k), encode(&v))
            })
            .collect();
        query.sort();
        let sep = if path.contains('?') { '&' } else { '?' };
        let url = format!(
            "{}{}{}{}",
            self.base,
            path,
            if query.is_empty() {
                String::new()
            } else {
                sep.to_string()
            },
            query.join("&")
        );

        let mut req = self.agent.get(&url).header("Accept", "application/json");
        for (k, v) in &self.headers {
            req = req.header(k.as_str(), v.as_str());
        }
        let mut resp = req.call().map_err(|e| self.err(e))?;
        let status = resp.status().as_u16();
        if (300..400).contains(&status) {
            let loc = resp
                .headers()
                .get("location")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("?");
            return Err(format!(
                "HTTP {} redirect to {} (redirects are not followed)",
                status, loc
            ));
        }
        let text = resp
            .body_mut()
            .with_config()
            .limit(self.max_bytes)
            .read_to_string()
            .map_err(|e| {
                format!(
                    "could not read response (limit {} KB): {}",
                    self.max_bytes / 1024,
                    self.err(e)
                )
            })?;
        if status >= 400 {
            let snippet: String = text.chars().take(300).collect();
            return Err(format!("HTTP {}: {}", status, snippet));
        }
        match serde_json::from_str::<serde_json::Value>(&text) {
            Ok(j) if sub.is_empty() => Ok(project_all(&j)),
            Ok(j) => Ok(project(&j, sub)),
            Err(_) => Ok(Value::Str(text)), // not JSON: return the text as is
        }
    }
}

impl RootResolver for Http {
    fn resolve_root(&self, name: &str, args: &Args, sub: &[Selection]) -> FieldResult {
        if name != "get" {
            return FieldResult::Missing;
        }
        match self.get(args, sub) {
            Ok(v) => FieldResult::Scalar(v),
            Err(e) => FieldResult::Error(e),
        }
    }
}

impl Service for Http {
    fn schema(&self) -> String {
        "root get(path:Str,...any query parameter):Json  // GET {base}{path}?params; braces pick JSON fields (a.b.c paths ok, arrays map)\ntype Json{...any field of the response}".into()
    }
    fn root(&self) -> &dyn RootResolver {
        self
    }
    fn info(&self) -> String {
        format!("HTTP {}", self.base)
    }
    fn hint(&self) -> String {
        format!("Base URL: {}.", self.base)
    }
}

fn parse_headers(raw: &str) -> Result<Vec<(String, String)>, String> {
    raw.split([';', '\n'])
        .map(str::trim)
        .filter(|h| !h.is_empty())
        .map(|h| {
            let (k, v) = h
                .split_once(':')
                .ok_or_else(|| format!("bad header `{}` (use Name: value)", h))?;
            Ok((k.trim().to_string(), v.trim().to_string()))
        })
        .collect()
}

fn connect() -> Result<Box<dyn Service>, String> {
    let base = ccp_mcp::env_required("CCP_URL")?;
    if !(base.starts_with("https://") || base.starts_with("http://")) {
        return Err("CCP_URL must start with http:// or https://".into());
    }
    let mut headers = std::env::var("CCP_HEADERS")
        .map(|h| parse_headers(&h))
        .unwrap_or_else(|_| Ok(Vec::new()))?;
    let bearer = std::env::var("CCP_BEARER").ok().filter(|b| !b.is_empty());
    if let Some(b) = &bearer {
        headers.push(("Authorization".into(), format!("Bearer {}", b)));
    }
    let secrets: Vec<String> = headers
        .iter()
        .map(|(_, v)| v.clone())
        .filter(|v| v.len() > 3)
        .collect();
    let timeout = std::env::var("CCP_TIMEOUT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(15u64);
    let max_kb = std::env::var("CCP_MAX_KB")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(4096u64);
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(timeout)))
        .max_redirects(0)
        .http_status_as_error(false)
        .build()
        .into();
    // base without query/fragment and without a trailing slash
    let base = base
        .split(['?', '#'])
        .next()
        .unwrap_or(&base)
        .trim_end_matches('/')
        .to_string();
    Ok(Box::new(Http {
        base,
        agent,
        headers,
        max_bytes: max_kb * 1024,
        secrets,
    }))
}

fn main() {
    ccp_mcp::main(
        Meta {
            name: "http-ccp",
            kind: "HTTP/JSON API",
            usage: "One root: get(path:\"/users/1\", page:2){name address.city tags{id}} calls GET on the configured API \
and returns only the fields you name; extra arguments become query parameters. Arrays map element-wise.",
            example: "get(path:\"/users\",limit:5){id name email}",
        },
        connect,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use ccp_proto::serve::answer_query;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    /// A tiny HTTP server: echoes the request line + auth header into the JSON it returns.
    fn serve() -> u16 {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for s in l.incoming() {
                let mut s = s.unwrap();
                let mut buf = [0u8; 4096];
                let n = s.read(&mut buf).unwrap();
                let req = String::from_utf8_lossy(&buf[..n]).to_string();
                let line = req.lines().next().unwrap_or("").to_string();
                let auth = req
                    .lines()
                    .find(|l| l.to_lowercase().starts_with("authorization:"))
                    .unwrap_or("")
                    .to_string();
                let (status, body, extra) = if line.contains("/redirect") {
                    (
                        "302 Found",
                        String::new(),
                        "Location: http://evil.example/x\r\n",
                    )
                } else if line.contains("/missing") {
                    ("404 Not Found", r#"{"error":"nope"}"#.to_string(), "")
                } else if line.contains("/text") {
                    ("200 OK", "plain words".to_string(), "")
                } else if line.contains("/list") {
                    ("200 OK", r#"[{"id":1,"name":"a","big":"xxxxxxxx"},{"id":2,"name":"b","big":"yyyyyyyy"}]"#.to_string(), "")
                } else {
                    (
                        "200 OK",
                        format!(
                            r#"{{"line":"{}","auth":"{}","user":{{"name":"Mert","address":{{"city":"Izmir"}}}}}}"#,
                            line.replace('"', "'"),
                            auth.replace('"', "'")
                        ),
                        "",
                    )
                };
                let _ = write!(
                    s,
                    "HTTP/1.1 {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n{}\r\n{}",
                    status,
                    body.len(),
                    extra,
                    body
                );
            }
        });
        port
    }

    #[test]
    fn path_rules() {
        assert!(check_path("/users/1?x=1").is_ok());
        for bad in [
            "users",
            "//evil.com/x",
            "/a/../b",
            "/a b",
            "http://x",
            "/a\\b",
            "/a#b",
        ] {
            assert!(check_path(bad).is_err(), "{}", bad);
        }
    }

    #[test]
    fn projects_fields_sends_query_and_auth_and_refuses_redirects() {
        let port = serve();
        unsafe {
            std::env::set_var("CCP_URL", format!("http://127.0.0.1:{}/api/", port));
            std::env::set_var("CCP_BEARER", "tok123");
        }
        let svc = connect().unwrap();
        let q = |s: &str| answer_query(svc.root(), s);
        assert_eq!(
            q(r#"get(path:"/x",page:2,q:"a b"){user.address.city auth}"#),
            r#"{get:{user.address.city:"Izmir",auth:"authorization: Bearer tok123"}}"#
        );
        assert!(q(r#"get(path:"/x",page:2,q:"a b"){line}"#).contains("GET /api/x?page=2&q=a%20b"));
        assert_eq!(
            q(r#"get(path:"/list"){id name}"#),
            r#"{get:[{id:1,name:"a"},{id:2,name:"b"}]}"#
        );
        assert_eq!(q(r#"get(path:"/text")"#), r#"{get:"plain words"}"#);
        assert!(q(r#"get(path:"/missing"){x}"#).contains("HTTP 404"));
        assert!(q(r#"get(path:"/redirect"){x}"#).contains("redirects are not followed"));
        assert!(q(r#"get(path:"//evil.com"){x}"#).contains("path must start"));
        assert!(q(r#"get(path:"/x"){user}"#).contains("Izmir"));
    }
}
