//! Minimal MCP (JSON-RPC over stdio, newline-delimited). Two tools only: the
//! model reads the schema once, then sends one compact query per question.

use crate::{Meta, Service};
use ccp_proto::serve::answer_query;
use serde_json::{Value as J, json};
use std::io::{self, BufRead, Write};

fn instructions(m: &Meta) -> String {
    format!(
        "Read-only {kind} access over CCP, the Core Context Protocol. Call ccp_schema once, then \
ccp_query with one query line, e.g. {example} . {usage} Braces choose the fields to return; \
ask only for what you need.",
        kind = m.kind,
        example = m.example,
        usage = m.usage
    )
}

fn tools(m: &Meta, hint: &str) -> J {
    json!([
        {
            "name": "ccp_schema",
            "description": format!("Return the CCP schema of the connected {} source. Call once.", m.kind),
            "inputSchema": {"type": "object", "properties": {}}
        },
        {
            "name": "ccp_query",
            "description": format!("Run one read-only CCP query, e.g. {}. Several root fields may be sent in one query. {}", m.example, hint),
            "inputSchema": {
                "type": "object",
                "properties": {"query": {"type": "string", "description": "A CCP query line"}},
                "required": ["query"]
            }
        }
    ])
}

fn text_result(text: String, is_error: bool) -> J {
    json!({"content": [{"type": "text", "text": text}], "isError": is_error})
}

pub fn run(meta: Meta, connect: &dyn Fn() -> Result<Box<dyn Service>, String>) {
    let mut svc: Option<Box<dyn Service>> = None;
    let stdout = io::stdout();
    let mut out = stdout.lock();

    // Connect lazily so a wrong password becomes a readable tool error
    // instead of the whole MCP server failing to start.
    let ensure = |svc: &mut Option<Box<dyn Service>>| -> Result<(), String> {
        if svc.is_none() {
            *svc = Some(connect()?);
        }
        Ok(())
    };

    for line in io::stdin().lock().lines() {
        let Ok(line) = line else { break };
        let Ok(req) = serde_json::from_str::<J>(line.trim()) else {
            continue;
        };
        let Some(id) = req.get("id").cloned() else {
            continue;
        }; // notification
        let method = req["method"].as_str().unwrap_or("");

        let result: Result<J, (i64, String)> = match method {
            "initialize" => Ok(json!({
                "protocolVersion": req["params"]["protocolVersion"].as_str().unwrap_or("2024-11-05"),
                "capabilities": {"tools": {}},
                "serverInfo": {"name": meta.name, "version": env!("CARGO_PKG_VERSION")},
                "instructions": instructions(&meta)
            })),
            "ping" => Ok(json!({})),
            "tools/list" => {
                let hint = match ensure(&mut svc) {
                    Ok(()) => svc.as_ref().unwrap().hint(),
                    Err(_) => "Call ccp_schema for the vocabulary.".to_string(),
                };
                Ok(json!({"tools": tools(&meta, &hint)}))
            }
            "tools/call" => {
                let tool = req["params"]["name"].as_str().unwrap_or("");
                Ok(match ensure(&mut svc) {
                    Err(e) => text_result(format!("could not connect: {}", e), true),
                    Ok(()) => {
                        let s = svc.as_ref().unwrap();
                        match tool {
                            "ccp_schema" => text_result(s.schema(), false),
                            "ccp_query" => match req["params"]["arguments"]["query"].as_str() {
                                Some(q) => {
                                    let r = answer_query(s.root(), q);
                                    let err = r.starts_with("ERR ");
                                    text_result(r, err)
                                }
                                None => text_result("missing string argument `query`".into(), true),
                            },
                            other => text_result(format!("unknown tool {}", other), true),
                        }
                    }
                })
            }
            other => Err((-32601, format!("method not found: {}", other))),
        };

        let resp = match result {
            Ok(r) => json!({"jsonrpc": "2.0", "id": id, "result": r}),
            Err((code, message)) => {
                json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
            }
        };
        let _ = writeln!(out, "{}", resp);
        let _ = out.flush();
    }
}
