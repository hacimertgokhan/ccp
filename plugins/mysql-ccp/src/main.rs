//! mysql-ccp: a CCP server for MySQL / MariaDB, built on `ccp-proto`.
//!
//! Add it straight to Claude (MCP over stdio); see README. Connection settings come from the
//! environment: CCP_HOST, CCP_PORT (default 3306), CCP_USER, CCP_PASSWORD,
//! CCP_DBNAME. It introspects the database once, advertises a CCP schema, then
//! answers queries such as `users(limit:10,active:1){id name}`.
//!
//! Safety model
//!  * read-only: the session is `READ ONLY` and only SELECTs are generated;
//!  * table/column names come from `information_schema`, never from the
//!    caller, and are backtick-quoted; unknown names are ignored;
//!  * every caller-supplied value is a bound parameter, never interpolated;
//!  * result size is capped (see MAX_LIMIT).

mod mcp;
mod sql;

use ccp_proto::engine::{Args, FieldResult, Resolvable, RootResolver};
use ccp_proto::query::Selection;
use ccp_proto::serve::serve_stdio;
use ccp_proto::value::Value;
use mysql::prelude::Queryable;
use mysql::{Conn, OptsBuilder};
use sql::{Column, Table};
use std::cell::RefCell;
use std::time::Duration;

struct Mysql {
    conn: RefCell<Conn>,
    tables: Vec<Table>,
    version: String,
}

struct RowNode(Vec<(String, Value)>);

impl Resolvable for RowNode {
    fn resolve_field(&self, field: &str, _args: &Args) -> FieldResult {
        match self.0.iter().find(|(k, _)| k == field) {
            Some((_, v)) => FieldResult::Scalar(v.clone()),
            None => FieldResult::Missing,
        }
    }
}

struct TableNode(Table);

impl Resolvable for TableNode {
    fn resolve_field(&self, field: &str, _args: &Args) -> FieldResult {
        match field {
            "name" => FieldResult::Scalar(Value::Str(self.0.name.clone())),
            "columns" => FieldResult::List(
                self.0
                    .columns
                    .iter()
                    .map(|c| Box::new(ColumnNode(c.clone())) as Box<dyn Resolvable>)
                    .collect(),
            ),
            _ => FieldResult::Missing,
        }
    }
}

struct ColumnNode(Column);

impl Resolvable for ColumnNode {
    fn resolve_field(&self, field: &str, _args: &Args) -> FieldResult {
        match field {
            "name" => FieldResult::Scalar(Value::Str(self.0.name.clone())),
            "type" => FieldResult::Scalar(Value::Str(self.0.data_type.clone())),
            _ => FieldResult::Missing,
        }
    }
}

impl RootResolver for Mysql {
    fn resolve_root(&self, name: &str, args: &Args, sub: &[Selection]) -> FieldResult {
        if name == "_tables" {
            return FieldResult::List(
                self.tables
                    .iter()
                    .map(|t| Box::new(TableNode(t.clone())) as Box<dyn Resolvable>)
                    .collect(),
            );
        }
        let Some(table) = self.tables.iter().find(|t| t.name == name) else {
            return FieldResult::Missing;
        };
        let (query, params, cols) = sql::build_select(table, args, sub);
        let rows: Result<Vec<mysql::Row>, _> = self.conn.borrow_mut().exec(query, params);
        match rows {
            Ok(rows) => FieldResult::List(
                rows.into_iter()
                    .map(|row| {
                        let values = row.unwrap();
                        let fields = cols.iter().cloned().zip(values.iter().map(sql::to_value)).collect();
                        Box::new(RowNode(fields)) as Box<dyn Resolvable>
                    })
                    .collect(),
            ),
            Err(e) => FieldResult::Error(e.to_string()),
        }
    }
}

fn env(name: &str) -> Result<String, String> {
    std::env::var(name).map_err(|_| format!("{} is not set", name))
}

fn connect() -> Result<Mysql, String> {
    let host = env("CCP_HOST")?;
    let user = env("CCP_USER")?;
    let dbname = env("CCP_DBNAME")?;
    let password = std::env::var("CCP_PASSWORD").unwrap_or_default();
    let port: u16 = match std::env::var("CCP_PORT") {
        Ok(p) => p.parse().map_err(|_| "CCP_PORT is not a valid port".to_string())?,
        Err(_) => 3306,
    };
    // NOTE: plain TCP (no TLS yet). Fine for localhost or a tunnelled link.
    let opts = OptsBuilder::new()
        .ip_or_hostname(Some(host))
        .tcp_port(port)
        .user(Some(user))
        .pass(Some(password.clone()))
        .db_name(Some(dbname))
        .tcp_connect_timeout(Some(Duration::from_secs(5)));
    let scrub = |e: mysql::Error| {
        let m = e.to_string();
        if password.is_empty() { m } else { m.replace(&password, "***") }
    };
    let mut conn = Conn::new(opts).map_err(&scrub)?;
    conn.query_drop("SET SESSION TRANSACTION READ ONLY").map_err(&scrub)?;
    let version: String = conn.query_first("SELECT VERSION()").map_err(&scrub)?.unwrap_or_default();
    let tables = sql::introspect(&mut conn).map_err(&scrub)?;
    Ok(Mysql { conn: RefCell::new(conn), tables, version })
}

fn main() {
    // Default: MCP over stdio (what Claude launches). `--ccp`: raw CCP line
    // protocol (SCHEMA / PING / query lines) for non-MCP clients.
    if !std::env::args().any(|a| a == "--ccp") {
        return mcp::run();
    }
    let db = match connect() {
        Ok(db) => db,
        Err(e) => {
            eprintln!("mysql-ccp: {}", e);
            std::process::exit(1);
        }
    };
    let schema = sql::schema_text(&db.tables);
    let info = format!("MySQL {} ({} tables)", db.version, db.tables.len());
    serve_stdio(&schema, &db, &|| Ok(info.clone()));
}
