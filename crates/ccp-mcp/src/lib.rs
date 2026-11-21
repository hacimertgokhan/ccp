//! Everything a CCP server needs except its data source.
//!
//! Implement [`Service`] (a schema plus a `RootResolver`), hand it to [`main`],
//! and you get an MCP server for Claude *and* the raw CCP line protocol.

mod json;
mod mcp;

pub use json::{lookup, project, project_all, project_field};

use ccp_proto::engine::RootResolver;
use ccp_proto::serve::serve_stdio;

/// A connected data source, ready to answer queries.
pub trait Service {
    /// The schema advertisement (read once by the model).
    fn schema(&self) -> String;
    fn root(&self) -> &dyn RootResolver;
    /// One-line description used for `PING` / `--ccp`.
    fn info(&self) -> String;
    /// Extra text appended to the `ccp_query` tool description (e.g. a table list).
    fn hint(&self) -> String {
        String::new()
    }
}

/// Static description of a server binary.
#[derive(Clone, Copy)]
pub struct Meta {
    /// Binary / MCP server name, e.g. `redis-ccp`.
    pub name: &'static str,
    /// Human name of the source, e.g. `Redis`.
    pub kind: &'static str,
    /// Paragraph shown to the model on `initialize`: what the roots mean.
    pub usage: &'static str,
    /// One representative query, shown in the tool description.
    pub example: &'static str,
}

pub fn env_required(name: &str) -> Result<String, String> {
    std::env::var(name).map_err(|_| format!("{} is not set", name))
}

/// Defensive: never let a credential reach a log or the model via an error message.
pub fn scrub(message: &str, secret: &str) -> String {
    if secret.is_empty() {
        message.to_string()
    } else {
        message.replace(secret, "***")
    }
}

/// Entry point shared by every server binary.
///
/// * no flags: MCP over stdio (what Claude launches)
/// * `--ccp`: raw CCP line protocol (`SCHEMA`, `PING`, query lines)
pub fn main(meta: Meta, connect: impl Fn() -> Result<Box<dyn Service>, String>) {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--version" || a == "-V") {
        println!("{} {}", meta.name, env!("CARGO_PKG_VERSION"));
        return;
    }
    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!(
            "{name} {ver}\nRead-only {kind} server for CCP (Core Context Protocol).\n\nUSAGE: {name} [--ccp]\n\n  (default)  MCP over stdio, for Claude and other MCP clients\n  --ccp      raw CCP line protocol over stdio\n\nConfiguration: see https://github.com/hacimertgokhan/ccp#servers",
            name = meta.name,
            ver = env!("CARGO_PKG_VERSION"),
            kind = meta.kind
        );
        return;
    }
    if !args.iter().any(|a| a == "--ccp") {
        return mcp::run(meta, &connect);
    }
    match connect() {
        Ok(svc) => {
            let info = svc.info();
            serve_stdio(&svc.schema(), svc.root(), &|| Ok(info.clone()));
        }
        Err(e) => {
            eprintln!("{}: {}", meta.name, e);
            std::process::exit(1);
        }
    }
}
