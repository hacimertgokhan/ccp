//! postgres-ccp: CCP server for PostgreSQL (current schema of the connection).
//! Env: CCP_HOST, CCP_PORT (5432), CCP_USER, CCP_PASSWORD, CCP_DBNAME.

use ccp_proto::value::Value;
use ccp_sql::catalog::{is_bool_type, is_float_type, is_int_type};
use ccp_sql::{
    Agg, AggFunc, Backend, Catalog, Column, DbValue, Dialect, ForeignKey, OutKind, Select, Table,
    build_sql, env_required, scrub,
};
use postgres::types::ToSql;
use postgres::{Client, NoTls};
use std::cell::RefCell;
use std::time::Duration;

/// Postgres is strictly typed, so values are cast explicitly on both sides:
/// ints travel as int8, everything else as text. Indexes stay usable for ints.
struct PgDialect;
impl Dialect for PgDialect {
    fn select_expr(&self, col: &Column) -> String {
        let q = self.quote(&col.name);
        if is_int_type(&col.data_type) {
            format!("{}::int8", q)
        } else if is_float_type(&col.data_type) {
            format!("{}::float8", q)
        } else if is_bool_type(&col.data_type) {
            q
        } else {
            format!("{}::text", q)
        }
    }
    fn agg_expr(&self, agg: &Agg, col: Option<&Column>, kind: OutKind) -> String {
        let q = col.map(|c| self.quote(&c.name)).unwrap_or_default();
        let cast = match kind {
            OutKind::Int => "int8",
            OutKind::Float => "float8",
            OutKind::Bool => "bool",
            OutKind::Str => "text",
        };
        match agg.func {
            AggFunc::Count => "COUNT(*)::int8".to_string(),
            AggFunc::CountDistinct => format!("COUNT(DISTINCT {})::int8", q),
            AggFunc::Sum => format!("SUM({})::{}", q, cast),
            AggFunc::Avg => format!("AVG({})::float8", q),
            AggFunc::Min => format!("MIN({})::{}", q, cast),
            AggFunc::Max => format!("MAX({})::{}", q, cast),
        }
    }
    fn filter_lhs(&self, col: &Column) -> String {
        let q = self.quote(&col.name);
        if is_int_type(&col.data_type) {
            q
        } else {
            format!("{}::text", q)
        }
    }
    fn placeholder(&self, index: usize, col: &Column) -> String {
        if is_int_type(&col.data_type) {
            format!("${}::int8", index)
        } else {
            format!("${}::text", index)
        }
    }
}

struct Pg {
    client: RefCell<Client>,
    password: String,
}

impl Pg {
    fn err(&self, e: postgres::Error) -> String {
        let msg = e
            .as_db_error()
            .map(|d| d.message().to_string())
            .unwrap_or_else(|| e.to_string());
        scrub(&msg, &self.password)
    }
}

fn norm_type(t: &str) -> String {
    match t {
        "smallint" => "smallint",
        "integer" => "integer",
        "bigint" => "bigint",
        "boolean" => "boolean",
        other => other,
    }
    .to_lowercase()
}

impl Backend for Pg {
    fn kind(&self) -> &'static str {
        "PostgreSQL"
    }

    fn version(&self) -> String {
        self.client
            .borrow_mut()
            .query_one("SHOW server_version", &[])
            .map(|r| r.get::<_, String>(0))
            .unwrap_or_default()
    }

    fn introspect(&self) -> Result<Catalog, String> {
        let mut c = self.client.borrow_mut();
        let cols = c
            .query(
                "SELECT table_name::text, column_name::text, data_type::text FROM information_schema.columns \
                 WHERE table_schema = current_schema() ORDER BY table_name, ordinal_position",
                &[],
            )
            .map_err(|e| self.err(e))?;
        let pks = c
            .query(
                "SELECT kcu.table_name::text, kcu.column_name::text FROM information_schema.table_constraints tc \
                 JOIN information_schema.key_column_usage kcu \
                   ON tc.constraint_name = kcu.constraint_name AND tc.table_schema = kcu.table_schema \
                 WHERE tc.constraint_type = 'PRIMARY KEY' AND tc.table_schema = current_schema() \
                 ORDER BY kcu.table_name, kcu.ordinal_position",
                &[],
            )
            .map_err(|e| self.err(e))?;
        let fks = c
            .query(
                "SELECT cl.relname::text, att.attname::text, rcl.relname::text, ratt.attname::text \
                 FROM pg_constraint con \
                 JOIN pg_class cl ON cl.oid = con.conrelid \
                 JOIN pg_namespace ns ON ns.oid = cl.relnamespace \
                 JOIN pg_class rcl ON rcl.oid = con.confrelid \
                 JOIN pg_attribute att ON att.attrelid = con.conrelid AND att.attnum = con.conkey[1] \
                 JOIN pg_attribute ratt ON ratt.attrelid = con.confrelid AND ratt.attnum = con.confkey[1] \
                 WHERE con.contype = 'f' AND ns.nspname = current_schema() AND array_length(con.conkey, 1) = 1",
                &[],
            )
            .map_err(|e| self.err(e))?;

        let mut tables: Vec<Table> = Vec::new();
        for r in cols {
            let (t, name, ty): (String, String, String) = (r.get(0), r.get(1), r.get(2));
            let col = Column {
                name,
                data_type: norm_type(&ty),
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
        for r in pks {
            let (t, col): (String, String) = (r.get(0), r.get(1));
            if let Some(tb) = tables.iter_mut().find(|x| x.name == t) {
                tb.pk.push(col);
            }
        }
        for r in fks {
            let (t, col, rt, rc): (String, String, String, String) =
                (r.get(0), r.get(1), r.get(2), r.get(3));
            if let Some(tb) = tables.iter_mut().find(|x| x.name == t) {
                tb.fks.push(ForeignKey {
                    column: col,
                    ref_table: rt,
                    ref_column: rc,
                });
            }
        }
        Ok(Catalog { tables })
    }

    fn select(&self, q: &Select) -> Result<Vec<Vec<Value>>, String> {
        let built = build_sql(&PgDialect, q)?;
        let boxed: Vec<Box<dyn ToSql + Sync>> = built
            .params
            .into_iter()
            .map(|p| match p {
                DbValue::Int(i) => Box::new(i) as Box<dyn ToSql + Sync>,
                DbValue::Str(s) => Box::new(s) as Box<dyn ToSql + Sync>,
            })
            .collect();
        let refs: Vec<&(dyn ToSql + Sync)> = boxed.iter().map(|b| b.as_ref()).collect();
        let rows = self
            .client
            .borrow_mut()
            .query(built.sql.as_str(), &refs)
            .map_err(|e| self.err(e))?;
        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            let mut vals = Vec::with_capacity(built.kinds.len());
            for (i, kind) in built.kinds.iter().enumerate() {
                let v = match kind {
                    OutKind::Int => row.get::<_, Option<i64>>(i).map(Value::Int),
                    OutKind::Float => row.get::<_, Option<f64>>(i).map(Value::Float),
                    OutKind::Bool => row.get::<_, Option<bool>>(i).map(Value::Bool),
                    OutKind::Str => row.get::<_, Option<String>>(i).map(Value::Str),
                };
                vals.push(v.unwrap_or(Value::Null));
            }
            out.push(vals);
        }
        Ok(out)
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
        Err(_) => 5432,
    };
    // NOTE: plain TCP (no TLS yet). Fine for localhost or a tunnelled link.
    let mut cfg = postgres::Config::new();
    cfg.host(&host)
        .port(port)
        .user(&user)
        .password(&password)
        .dbname(&dbname)
        .connect_timeout(Duration::from_secs(5));
    let mut client = cfg.connect(NoTls).map_err(|e| {
        let msg = e
            .as_db_error()
            .map(|d| d.message().to_string())
            .unwrap_or_else(|| e.to_string());
        scrub(&msg, &password)
    })?;
    client
        .batch_execute("SET default_transaction_read_only = on")
        .map_err(|e| scrub(&e.to_string(), &password))?;
    Ok(Box::new(Pg {
        client: RefCell::new(client),
        password,
    }))
}

fn main() {
    ccp_sql::main("postgres-ccp", "PostgreSQL", connect);
}
