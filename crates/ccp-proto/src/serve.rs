use crate::engine::{execute, RootResolver};
use crate::query::parse_query;
use std::io::{self, BufRead, Write};

/// CCP wire protocol over stdio: one line in, one line out. No JSON-RPC
/// envelope, no per-call schema repetition.
///
/// * `SCHEMA` -> the schema advertisement (sent once by the client)
/// * `PING`   -> `OK <info>` or `ERR <reason>` (health/connectivity probe)
/// * anything else is a CCP query; the reply is the compact result or `ERR <reason>`
pub fn serve_stdio(
    schema: &str,
    root: &dyn RootResolver,
    ping: &dyn Fn() -> Result<String, String>,
) {
    let stdin = io::stdin();
    let mut stdout = io::stdout();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let response = match trimmed {
            "SCHEMA" => schema.replace('\n', " | "),
            "PING" => match ping() {
                Ok(info) => format!("OK {}", info),
                Err(e) => format!("ERR {}", e),
            },
            q => answer_query(root, q),
        };
        let _ = writeln!(stdout, "{}", response);
        let _ = stdout.flush();
    }
}

/// Parse and execute one CCP query line; the reply is the compact result or `ERR <reason>`.
pub fn answer_query(root: &dyn RootResolver, query: &str) -> String {
    match parse_query(query) {
        Ok(selections) => execute(root, &selections).to_compact_string(),
        Err(e) => format!("ERR {}", e),
    }
}
