use crate::catalog::{Column, Table, is_bool_type, is_int_type};

/// A value bound to a placeholder. Caller-supplied data only ever travels as one of these.
#[derive(Clone, Debug, PartialEq)]
pub enum DbValue {
    Int(i64),
    Str(String),
}

/// `column IN (values...)`; plain equality is a one-element list.
#[derive(Clone, Debug)]
pub struct Filter {
    pub column: String,
    pub values: Vec<DbValue>,
}

pub struct Select<'a> {
    pub table: &'a Table,
    pub columns: &'a [String],
    pub filters: &'a [Filter],
    pub limit: i64,
    pub offset: i64,
}

/// Per-database SQL flavour. Defaults are ANSI (double-quoted identifiers, `?`).
pub trait Dialect {
    fn quote(&self, ident: &str) -> String {
        format!("\"{}\"", ident.replace('"', "\"\""))
    }
    fn select_expr(&self, col: &Column) -> String {
        self.quote(&col.name)
    }
    fn filter_lhs(&self, col: &Column) -> String {
        self.quote(&col.name)
    }
    fn placeholder(&self, _index: usize, _col: &Column) -> String {
        "?".to_string()
    }
}

/// Make a caller value fit the column it is compared with. `None` = cannot match.
fn coerce(col: &Column, v: &DbValue) -> Option<DbValue> {
    let t = col.data_type.as_str();
    match v {
        DbValue::Int(i) if is_int_type(t) => Some(DbValue::Int(*i)),
        DbValue::Int(i) if is_bool_type(t) => Some(DbValue::Str((*i != 0).to_string())),
        DbValue::Int(i) => Some(DbValue::Str(i.to_string())),
        DbValue::Str(s) if is_int_type(t) => s.trim().parse().ok().map(DbValue::Int),
        DbValue::Str(s) => Some(DbValue::Str(s.clone())),
    }
}

/// Build `SELECT <cols> FROM <table> [WHERE col IN (?..) AND ..] [ORDER BY pk] LIMIT n OFFSET m`.
///
/// Identifiers come only from the introspected `Table` (unknown columns are
/// ignored); values are bound parameters; limit/offset are integers.
pub fn build_sql(d: &dyn Dialect, q: &Select) -> Result<(String, Vec<DbValue>), String> {
    let col = |name: &str| q.table.columns.iter().find(|c| c.name == name);
    let cols: Vec<&Column> = q.columns.iter().filter_map(|n| col(n)).collect();
    if cols.is_empty() {
        return Err("no selectable columns".into());
    }
    let mut params: Vec<DbValue> = Vec::new();
    let mut wheres: Vec<String> = Vec::new();
    for f in q.filters {
        let Some(c) = col(&f.column) else { continue };
        let vals: Vec<DbValue> = f.values.iter().filter_map(|v| coerce(c, v)).collect();
        if vals.is_empty() {
            wheres.push("1=0".into());
            continue;
        }
        let mut phs = Vec::new();
        for v in vals {
            params.push(v);
            phs.push(d.placeholder(params.len(), c));
        }
        wheres.push(format!("{} IN ({})", d.filter_lhs(c), phs.join(", ")));
    }
    let mut sql = format!(
        "SELECT {} FROM {}",
        cols.iter()
            .map(|c| d.select_expr(c))
            .collect::<Vec<_>>()
            .join(", "),
        d.quote(&q.table.name)
    );
    if !wheres.is_empty() {
        sql.push_str(&format!(" WHERE {}", wheres.join(" AND ")));
    }
    let pk: Vec<String> = q.table.pk.iter().map(|p| d.quote(p)).collect();
    if !pk.is_empty() {
        sql.push_str(&format!(" ORDER BY {}", pk.join(", ")));
    }
    sql.push_str(&format!(
        " LIMIT {} OFFSET {}",
        q.limit.max(0),
        q.offset.max(0)
    ));
    Ok((sql, params))
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Ansi;
    impl Dialect for Ansi {}

    fn users() -> Table {
        Table {
            name: "users".into(),
            columns: vec![
                Column {
                    name: "id".into(),
                    data_type: "int".into(),
                },
                Column {
                    name: "name".into(),
                    data_type: "varchar".into(),
                },
            ],
            pk: vec!["id".into()],
            fks: vec![],
        }
    }

    #[test]
    fn unknown_columns_are_dropped_and_values_bound() {
        let t = users();
        let cols = vec!["name".to_string(), "evil\"; DROP".to_string()];
        let filters = vec![Filter {
            column: "name".into(),
            values: vec![DbValue::Str("x' OR '1'='1".into())],
        }];
        let (sql, params) = build_sql(
            &Ansi,
            &Select {
                table: &t,
                columns: &cols,
                filters: &filters,
                limit: 5,
                offset: 0,
            },
        )
        .unwrap();
        assert_eq!(
            sql,
            "SELECT \"name\" FROM \"users\" WHERE \"name\" IN (?) ORDER BY \"id\" LIMIT 5 OFFSET 0"
        );
        assert_eq!(params, vec![DbValue::Str("x' OR '1'='1".into())]);
    }

    #[test]
    fn int_columns_coerce_and_unparsable_never_matches() {
        let t = users();
        let cols = vec!["id".to_string()];
        let filters = vec![Filter {
            column: "id".into(),
            values: vec![DbValue::Str("abc".into())],
        }];
        let (sql, params) = build_sql(
            &Ansi,
            &Select {
                table: &t,
                columns: &cols,
                filters: &filters,
                limit: 1,
                offset: 0,
            },
        )
        .unwrap();
        assert!(sql.contains("WHERE 1=0"));
        assert!(params.is_empty());
    }
}
