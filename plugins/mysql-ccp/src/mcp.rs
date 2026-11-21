//! Minimal MCP (JSON-RPC over stdio, newline-delimited) front end, so
//! `mysql-ccp` can be added to Claude directly. Two tools only — that is the
//! point of CCP: the model reads the schema once, then sends one compact query
//! instead of calling many narrow tools.

use ccp_proto::serve::answer_query;
use serde_json::{json, Value as J};
use std::io::{self, BufRead, Write};

use crate::{connect, sql, Mysql};

const INSTRUCTIONS: &str = "Read-only MySQL access over CCP, a compact GraphQL-style protocol. \
Call ccp_schema once, then ccp_query with one query line, e.g. users(limit:5,active:1){id name}. \
Root fields are tables; arguments filter by column equality plus limit/offset; \
the braces choose which columns to return.";

fn tools(schema_hint: &str) -> J {
    json!([
        {
            "name": "ccp_schema",
            "description": "Return the CCP schema (tables, columns, types) of the connected MySQL database. Call once.",
            "inputSchema": {"type": "object", "properties": {}}
        },
        {
            "name": "ccp_query",
            "description": format!("Run one read-only CCP query. Syntax: table(limit:N,offset:N,column:value){{col1 col2}}. Several root fields may be sent in one query. {}", schema_hint),
            "inputSchema": {
                "type": "object",
                "properties": {"query": {"type": "string", "description": "A CCP query line, e.g. users(id:1){name email}"}},
                "required": ["query"]
            }
        }
    ])
}

fn text_result(text: String, is_error: bool) -> J {
    json!({"content": [{"type": "text", "text": text}], "isError": is_error})
}

pub fn run() {
    let mut db: Option<Mysql> = None;
    let stdout = io::stdout();
    let mut out = stdout.lock();

    // Connect lazily so a wrong password shows up as a readable tool error
    // instead of the whole MCP server failing to start.
    let ensure = |db: &mut Option<Mysql>| -> Result<(), String> {
        if db.is_none() {
            *db = Some(connect()?);
        }
        Ok(())
    };

    for line in io::stdin().lock().lines() {
        let Ok(line) = line else { break };
        let Ok(req) = serde_json::from_str::<J>(line.trim()) else { continue };
        let Some(id) = req.get("id").cloned() else { continue }; // notification
        let method = req["method"].as_str().unwrap_or("");

        let result: Result<J, (i64, String)> = match method {
            "initialize" => Ok(json!({
                "protocolVersion": req["params"]["protocolVersion"].as_str().unwrap_or("2024-11-05"),
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "mysql-ccp", "version": env!("CARGO_PKG_VERSION")},
                "instructions": INSTRUCTIONS
            })),
            "ping" => Ok(json!({})),
            "tools/list" => {
                let hint = match ensure(&mut db) {
                    Ok(()) => format!("Tables: {}.", db.as_ref().unwrap().tables.iter().map(|t| t.name.as_str()).collect::<Vec<_>>().join(", ")),
                    Err(_) => "Call ccp_schema for the table list.".to_string(),
                };
                Ok(json!({"tools": tools(&hint)}))
            }
            "tools/call" => {
                let name = req["params"]["name"].as_str().unwrap_or("");
                Ok(match ensure(&mut db) {
                    Err(e) => text_result(format!("could not connect: {}", e), true),
                    Ok(()) => {
                        let m = db.as_ref().unwrap();
                        match name {
                            "ccp_schema" => text_result(sql::schema_text(&m.tables), false),
                            "ccp_query" => match req["params"]["arguments"]["query"].as_str() {
                                Some(q) => {
                                    let r = answer_query(m, q);
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
            Err((code, message)) => json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}}),
        };
        let _ = writeln!(out, "{}", resp);
        let _ = out.flush();
    }
}
