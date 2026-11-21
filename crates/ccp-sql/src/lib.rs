//! Everything a CCP database server needs except the driver itself.
//!
//! A driver implements [`Backend`] (introspection + one `select` call) and
//! [`Dialect`] (identifier quoting / placeholders). This crate turns that into
//! a CCP schema, nested relation queries, an MCP server and a raw CCP server.

pub mod catalog;
pub mod exec;
pub mod mcp;
pub mod run;
pub mod sqlgen;

pub use catalog::{Catalog, Column, ForeignKey, Rel, Table};
pub use exec::{Backend, Executor};
pub use run::main;
pub use sqlgen::{DbValue, Dialect, Filter, Select, build_sql};

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
