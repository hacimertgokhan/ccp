use ccp_proto::engine::Args;
use ccp_proto::query::{ArgValue, Selection};
use ccp_proto::value::Value;
use mysql::prelude::Queryable;

pub const DEFAULT_LIMIT: i64 = 100;
pub const MAX_LIMIT: i64 = 1000;

#[derive(Clone, Debug)]
pub struct Column {
    pub name: String,
    pub data_type: String,
}

#[derive(Clone, Debug)]
pub struct Table {
    pub name: String,
    pub columns: Vec<Column>,
}

pub fn introspect(conn: &mut mysql::Conn) -> Result<Vec<Table>, mysql::Error> {
    let rows: Vec<(String, String, String)> = conn.query(
        "SELECT TABLE_NAME, COLUMN_NAME, DATA_TYPE FROM information_schema.COLUMNS \
         WHERE TABLE_SCHEMA = DATABASE() ORDER BY TABLE_NAME, ORDINAL_POSITION",
    )?;
    let mut tables: Vec<Table> = Vec::new();
    for (t, c, ty) in rows {
        // CCP identifiers are [A-Za-z0-9_]; anything else can't be addressed
        // by the query language, so it is simply not exposed.
        if !is_ident(&t) || !is_ident(&c) {
            continue;
        }
        let col = Column { name: c, data_type: ty };
        match tables.last_mut() {
            Some(last) if last.name == t => last.columns.push(col),
            _ => tables.push(Table { name: t, columns: vec![col] }),
        }
    }
    Ok(tables)
}

fn is_ident(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn ccp_type(data_type: &str) -> &'static str {
    match data_type {
        "tinyint" | "smallint" | "mediumint" | "int" | "integer" | "bigint" | "year" => "Int",
        _ => "Str",
    }
}

fn type_name(table: &str) -> String {
    let mut c = table.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}

/// The CCP schema advertisement, generated from the live database.
pub fn schema_text(tables: &[Table]) -> String {
    let mut out = String::from("root _tables:[_Table]\n");
    for t in tables {
        let args: Vec<String> = ["limit:Int".to_string(), "offset:Int".to_string()]
            .into_iter()
            .chain(t.columns.iter().map(|c| format!("{}:{}", c.name, ccp_type(&c.data_type))))
            .collect();
        out.push_str(&format!("root {}({}):[{}]\n", t.name, args.join(","), type_name(&t.name)));
    }
    out.push_str("type _Table{name:Str,columns:[_Column]}\ntype _Column{name:Str,type:Str}");
    for t in tables {
        let fields: Vec<String> =
            t.columns.iter().map(|c| format!("{}:{}", c.name, ccp_type(&c.data_type))).collect();
        out.push_str(&format!("\ntype {}{{{}}}", type_name(&t.name), fields.join(",")));
    }
    out
}

fn quote(ident: &str) -> String {
    format!("`{}`", ident.replace('`', "``"))
}

/// Build `SELECT <requested cols> FROM <table> [WHERE col = ? ...] LIMIT n OFFSET m`.
/// Returns the SQL, its bound parameters and the selected column names.
/// Identifiers only ever come from the introspected `table`; caller values
/// are parameters; limit/offset are clamped integers.
pub fn build_select(table: &Table, args: &Args, sub: &[Selection]) -> (String, Vec<mysql::Value>, Vec<String>) {
    let mut cols: Vec<String> = Vec::new();
    for s in sub {
        if table.columns.iter().any(|c| c.name == s.name) && !cols.contains(&s.name) {
            cols.push(s.name.clone());
        }
    }
    if cols.is_empty() {
        cols = table.columns.iter().map(|c| c.name.clone()).collect();
    }

    // Deterministic order for the WHERE clause (HashMap order is random).
    let mut filters: Vec<(&String, &ArgValue)> = args
        .iter()
        .filter(|(k, _)| table.columns.iter().any(|c| &c.name == *k))
        .collect();
    filters.sort_by(|a, b| a.0.cmp(b.0));

    let mut params = Vec::new();
    let mut wheres = Vec::new();
    for (k, v) in filters {
        wheres.push(format!("{} = ?", quote(k)));
        params.push(match v {
            ArgValue::Int(i) => mysql::Value::Int(*i),
            ArgValue::Str(s) => mysql::Value::Bytes(s.clone().into_bytes()),
            ArgValue::Bool(b) => mysql::Value::Int(*b as i64),
        });
    }

    let int_arg = |name: &str, default: i64| match args.get(name) {
        Some(ArgValue::Int(i)) => *i,
        _ => default,
    };
    let limit = int_arg("limit", DEFAULT_LIMIT).clamp(0, MAX_LIMIT);
    let offset = int_arg("offset", 0).max(0);

    let mut sql = format!(
        "SELECT {} FROM {}",
        cols.iter().map(|c| quote(c)).collect::<Vec<_>>().join(", "),
        quote(&table.name)
    );
    if !wheres.is_empty() {
        sql.push_str(" WHERE ");
        sql.push_str(&wheres.join(" AND "));
    }
    sql.push_str(&format!(" LIMIT {} OFFSET {}", limit, offset));
    (sql, params, cols)
}

pub fn to_value(v: &mysql::Value) -> Value {
    match v {
        mysql::Value::NULL => Value::Null,
        mysql::Value::Int(i) => Value::Int(*i),
        mysql::Value::UInt(u) => i64::try_from(*u).map(Value::Int).unwrap_or_else(|_| Value::Str(u.to_string())),
        mysql::Value::Bytes(b) => Value::Str(String::from_utf8_lossy(b).into_owned()),
        other => Value::Str(other.as_sql(true).trim_matches('\'').to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn users() -> Table {
        Table {
            name: "users".into(),
            columns: vec![
                Column { name: "id".into(), data_type: "int".into() },
                Column { name: "name".into(), data_type: "varchar".into() },
            ],
        }
    }

    fn sel(name: &str) -> Selection {
        Selection { name: name.into(), args: HashMap::new(), sub: vec![] }
    }

    #[test]
    fn selects_only_requested_known_columns() {
        let (sql, params, cols) = build_select(&users(), &HashMap::new(), &[sel("name"), sel("evil`; DROP"), sel("name")]);
        assert_eq!(sql, "SELECT `name` FROM `users` LIMIT 100 OFFSET 0");
        assert!(params.is_empty());
        assert_eq!(cols, vec!["name"]);
    }

    #[test]
    fn values_are_bound_never_interpolated() {
        let mut args: Args = HashMap::new();
        args.insert("name".into(), ArgValue::Str("x' OR '1'='1".into()));
        args.insert("unknown".into(), ArgValue::Int(1));
        let (sql, params, _) = build_select(&users(), &args, &[]);
        assert_eq!(sql, "SELECT `id`, `name` FROM `users` WHERE `name` = ? LIMIT 100 OFFSET 0");
        assert_eq!(params.len(), 1);
        assert!(!sql.contains("OR"));
    }

    #[test]
    fn limit_is_clamped() {
        let mut args: Args = HashMap::new();
        args.insert("limit".into(), ArgValue::Int(10_000_000));
        args.insert("offset".into(), ArgValue::Int(-5));
        let (sql, _, _) = build_select(&users(), &args, &[]);
        assert!(sql.ends_with("LIMIT 1000 OFFSET 0"));
    }

    #[test]
    fn schema_is_generated() {
        let s = schema_text(&[users()]);
        assert!(s.contains("root users(limit:Int,offset:Int,id:Int,name:Str):[Users]"));
        assert!(s.contains("type Users{id:Int,name:Str}"));
    }
}
