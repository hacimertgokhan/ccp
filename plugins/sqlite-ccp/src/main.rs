//! sqlite-ccp: CCP server for a SQLite database file, opened read-only.
//! Env: CCP_PATH (path to the .db file; CCP_DBNAME is accepted as an alias).

use ccp_sql::Backend;
use rusqlite::{Connection, OpenFlags};
use sqlite_ccp::Sqlite;

fn connect() -> Result<Box<dyn Backend>, String> {
    let path =
        ccp_sql::env_required("CCP_PATH").or_else(|_| ccp_sql::env_required("CCP_DBNAME"))?;
    let conn = Connection::open_with_flags(
        &path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|e| format!("cannot open {}: {}", path, e))?;
    Ok(Box::new(Sqlite::from_connection(conn, "SQLite")))
}

fn main() {
    ccp_sql::main("sqlite-ccp", "SQLite", connect);
}
