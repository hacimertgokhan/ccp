//! Everything a CCP database server needs except the driver itself.
//!
//! A driver implements [`Backend`] (introspection + one `select` call) and
//! [`Dialect`] (identifier quoting / placeholders). This crate turns that into
//! a CCP schema, nested relation queries, an MCP server and a raw CCP server.

pub mod catalog;
pub mod exec;
mod serve;
pub mod sqlgen;

pub use catalog::{Catalog, Column, ForeignKey, Rel, Table};
pub use exec::{Backend, Executor};
pub use serve::main;
pub use sqlgen::{
    Agg, AggFunc, Built, DbValue, Dialect, Filter, Order, OutKind, Select, build_sql,
};

pub use ccp_mcp::{env_required, scrub};
