//! Minimal MCP (JSON-RPC over stdio, newline-delimited) front end so a CCP
//! database server can be added to Claude directly. Two tools only: the model
//! reads the schema once, then sends one compact query per question.

use crate::exec::Executor;
use ccp_proto::serve::answer_query;
use serde_json::{Value as J, json};
use std::io::{self, BufRead, Write};

fn instructions(kind: &str) -> String {
    format!(
        "Read-only {kind} access over CCP, the Core Context Protocol. Call ccp_schema once, \
then ccp_query with one query line, e.g. users(limit:5,active:1){{id name posts{{title}}}}. \
Root fields are tables; arguments filter by column equality plus limit/offset; braces choose \
the columns; foreign keys appear as nested fields, so related data comes back in one call."
    )
}

fn tools(kind: &str, hint: &str) -> J {
    json!([
        {
            "name": "ccp_schema",
            "description": format!("Return the CCP schema (tables, columns, types, relations) of the connected {kind} database. Call once."),
            "inputSchema": {"type": "object", "properties": {}}
        },
        {
            "name": "ccp_query",
            "description": format!("Run one read-only CCP query. Syntax: table(limit:N,offset:N,column:value){{col1 col2 relation{{col}}}}. Several root fields may be sent in one query. {}", hint),
            "inputSchema": {
                "type": "object",
                "properties": {"query": {"type": "string", "description": "A CCP query line, e.g. users(id:1){name posts{title}}"}},
                "required": ["query"]
            }
        }
    ])
}

fn text_result(text: String, is_error: bool) -> J {
    json!({"content": [{"type": "text", "text": text}], "isError": is_error})
}

pub fn run(name: &'static str, kind: &'static str, connect: &dyn Fn() -> Result<Executor, String>) {
    let mut db: Option<Executor> = None;
    let stdout = io::stdout();
    let mut out = stdout.lock();

    // Connect lazily so a wrong password becomes a readable tool error
    // instead of the whole MCP server failing to start.
    let ensure = |db: &mut Option<Executor>| -> Result<(), String> {
        if db.is_none() {
            *db = Some(connect()?);
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
                "serverInfo": {"name": name, "version": env!("CARGO_PKG_VERSION")},
                "instructions": instructions(kind)
            })),
            "ping" => Ok(json!({})),
            "tools/list" => {
                let hint = match ensure(&mut db) {
                    Ok(()) => format!(
                        "Tables: {}.",
                        db.as_ref()
                            .unwrap()
                            .catalog
                            .tables
                            .iter()
                            .map(|t| t.name.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                    Err(_) => "Call ccp_schema for the table list.".to_string(),
                };
                Ok(json!({"tools": tools(kind, &hint)}))
            }
            "tools/call" => {
                let tool = req["params"]["name"].as_str().unwrap_or("");
                Ok(match ensure(&mut db) {
                    Err(e) => text_result(format!("could not connect: {}", e), true),
                    Ok(()) => {
                        let ex = db.as_ref().unwrap();
                        match tool {
                            "ccp_schema" => text_result(ex.catalog.schema_text(), false),
                            "ccp_query" => match req["params"]["arguments"]["query"].as_str() {
                                Some(q) => {
                                    let r = answer_query(ex, q);
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
