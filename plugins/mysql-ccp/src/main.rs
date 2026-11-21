//! mysql-ccp: CCP server for MySQL / MariaDB.
//! Env: CCP_HOST, CCP_PORT (3306), CCP_USER, CCP_PASSWORD, CCP_DBNAME.

use ccp_proto::value::Value;
use ccp_sql::{
    Backend, Catalog, Column, DbValue, Dialect, ForeignKey, Select, Table, build_sql, env_required,
    scrub,
};
use mysql::prelude::Queryable;
use mysql::{Conn, OptsBuilder};
use std::cell::RefCell;
use std::time::Duration;

struct MysqlDialect;
impl Dialect for MysqlDialect {
    fn quote(&self, ident: &str) -> String {
        format!("`{}`", ident.replace('`', "``"))
    }
}

struct Mysql {
    conn: RefCell<Conn>,
    password: String,
}

impl Mysql {
    fn err(&self, e: mysql::Error) -> String {
        scrub(&e.to_string(), &self.password)
    }
}

fn to_value(v: &mysql::Value) -> Value {
    match v {
        mysql::Value::NULL => Value::Null,
        mysql::Value::Int(i) => Value::Int(*i),
        mysql::Value::UInt(u) => i64::try_from(*u)
            .map(Value::Int)
            .unwrap_or_else(|_| Value::Str(u.to_string())),
        mysql::Value::Float(f) => Value::Float(*f as f64),
        mysql::Value::Double(f) => Value::Float(*f),
        mysql::Value::Bytes(b) => Value::Str(String::from_utf8_lossy(b).into_owned()),
        other => Value::Str(other.as_sql(true).trim_matches('\'').to_string()),
    }
}

impl Backend for Mysql {
    fn kind(&self) -> &'static str {
        "MySQL"
    }

    fn version(&self) -> String {
        self.conn
            .borrow_mut()
            .query_first("SELECT VERSION()")
            .ok()
            .flatten()
            .unwrap_or_default()
    }

    fn introspect(&self) -> Result<Catalog, String> {
        let mut conn = self.conn.borrow_mut();
        let cols: Vec<(String, String, String)> = conn
            .query(
                "SELECT TABLE_NAME, COLUMN_NAME, DATA_TYPE FROM information_schema.COLUMNS \
                 WHERE TABLE_SCHEMA = DATABASE() ORDER BY TABLE_NAME, ORDINAL_POSITION",
            )
            .map_err(|e| self.err(e))?;
        let pks: Vec<(String, String)> = conn
            .query(
                "SELECT TABLE_NAME, COLUMN_NAME FROM information_schema.KEY_COLUMN_USAGE \
                 WHERE TABLE_SCHEMA = DATABASE() AND CONSTRAINT_NAME = 'PRIMARY' ORDER BY TABLE_NAME, ORDINAL_POSITION",
            )
            .map_err(|e| self.err(e))?;
        let fks: Vec<(String, String, String, String, String)> = conn
            .query(
                "SELECT CONSTRAINT_NAME, TABLE_NAME, COLUMN_NAME, REFERENCED_TABLE_NAME, REFERENCED_COLUMN_NAME \
                 FROM information_schema.KEY_COLUMN_USAGE \
                 WHERE TABLE_SCHEMA = DATABASE() AND REFERENCED_TABLE_NAME IS NOT NULL",
            )
            .map_err(|e| self.err(e))?;

        let mut tables: Vec<Table> = Vec::new();
        for (t, c, ty) in cols {
            let col = Column {
                name: c,
                data_type: ty.to_lowercase(),
            };
            match tables.last_mut() {
                Some(last) if last.name == t => last.columns.push(col),
                _ => tables.push(Table {
                    name: t,
                    columns: vec![col],
                    ..Default::default()
                }),
            }
        }
        for (t, c) in pks {
            if let Some(tb) = tables.iter_mut().find(|x| x.name == t) {
                tb.pk.push(c);
            }
        }
        // Composite foreign keys (several rows per constraint) are not exposed.
        for (name, t, c, rt, rc) in &fks {
            let parts = fks.iter().filter(|f| &f.0 == name && &f.1 == t).count();
            if parts == 1
                && let Some(tb) = tables.iter_mut().find(|x| &x.name == t)
            {
                tb.fks.push(ForeignKey {
                    column: c.clone(),
                    ref_table: rt.clone(),
                    ref_column: rc.clone(),
                });
            }
        }
        Ok(Catalog { tables })
    }

    fn select(&self, q: &Select) -> Result<Vec<Vec<Value>>, String> {
        let built = build_sql(&MysqlDialect, q)?;
        let params: Vec<mysql::Value> = built
            .params
            .into_iter()
            .map(|p| match p {
                DbValue::Int(i) => mysql::Value::Int(i),
                DbValue::Str(s) => mysql::Value::Bytes(s.into_bytes()),
            })
            .collect();
        let rows: Vec<mysql::Row> = self
            .conn
            .borrow_mut()
            .exec(built.sql, params)
            .map_err(|e| self.err(e))?;
        Ok(rows
            .into_iter()
            .map(|r| r.unwrap().iter().map(to_value).collect())
            .collect())
    }
}

fn connect() -> Result<Box<dyn Backend>, String> {
    let host = env_required("CCP_HOST")?;
    let user = env_required("CCP_USER")?;
    let dbname = env_required("CCP_DBNAME")?;
    let password = std::env::var("CCP_PASSWORD").unwrap_or_default();
    let port: u16 = match std::env::var("CCP_PORT") {
        Ok(p) => p
            .parse()
            .map_err(|_| "CCP_PORT is not a valid port".to_string())?,
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
    let mut conn = Conn::new(opts).map_err(|e| scrub(&e.to_string(), &password))?;
    conn.query_drop("SET SESSION TRANSACTION READ ONLY")
        .map_err(|e| scrub(&e.to_string(), &password))?;
    Ok(Box::new(Mysql {
        conn: RefCell::new(conn),
        password,
    }))
}

fn main() {
    ccp_sql::main("mysql-ccp", "MySQL", connect);
}
