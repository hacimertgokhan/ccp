use crate::catalog::{Column, Table, is_bool_type, is_decimal_type, is_float_type, is_int_type};

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

#[derive(Clone, Debug, PartialEq)]
pub struct Order {
    /// A column, or (in group mode) a group column / aggregate alias.
    pub field: String,
    pub desc: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum AggFunc {
    Count,
    CountDistinct,
    Sum,
    Avg,
    Min,
    Max,
}

#[derive(Clone, Debug)]
pub struct Agg {
    /// Output name, also the SQL alias (e.g. `sum_total`).
    pub alias: String,
    pub func: AggFunc,
    pub column: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum OutKind {
    Int,
    Float,
    Bool,
    Str,
}

pub struct Select<'a> {
    pub table: &'a Table,
    /// Plain mode: the columns to return. Group mode: the grouping columns.
    pub columns: &'a [String],
    pub filters: &'a [Filter],
    pub order: &'a [Order],
    /// `Some` switches to group mode: output = `columns` then `aggs`.
    pub aggs: Option<&'a [Agg]>,
    pub limit: i64,
    pub offset: i64,
}

pub struct Built {
    pub sql: String,
    pub params: Vec<DbValue>,
    /// One entry per output column, so drivers can decode rows exactly.
    pub kinds: Vec<OutKind>,
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
    /// `kind` is the output kind; drivers with strict typing cast to it.
    fn agg_expr(&self, agg: &Agg, col: Option<&Column>, _kind: OutKind) -> String {
        let q = col.map(|c| self.quote(&c.name)).unwrap_or_default();
        match agg.func {
            AggFunc::Count => "COUNT(*)".to_string(),
            AggFunc::CountDistinct => format!("COUNT(DISTINCT {})", q),
            AggFunc::Sum => format!("SUM({})", q),
            AggFunc::Avg => format!("AVG({})", q),
            AggFunc::Min => format!("MIN({})", q),
            AggFunc::Max => format!("MAX({})", q),
        }
    }
}

pub fn col_kind(c: &Column) -> OutKind {
    let t = c.data_type.as_str();
    if is_int_type(t) {
        OutKind::Int
    } else if is_float_type(t) {
        OutKind::Float
    } else if is_bool_type(t) {
        OutKind::Bool
    } else {
        OutKind::Str
    }
}

fn is_numeric(c: &Column) -> bool {
    let t = c.data_type.as_str();
    is_int_type(t) || is_float_type(t) || is_decimal_type(t)
}

pub fn agg_kind(agg: &Agg, col: Option<&Column>) -> Result<OutKind, String> {
    match agg.func {
        AggFunc::Count | AggFunc::CountDistinct => Ok(OutKind::Int),
        AggFunc::Avg | AggFunc::Sum => {
            let c = col.ok_or("aggregate needs a column")?;
            if !is_numeric(c) {
                return Err(format!(
                    "{} needs a numeric column, `{}` is {}",
                    agg.alias, c.name, c.data_type
                ));
            }
            Ok(if agg.func == AggFunc::Sum && is_int_type(&c.data_type) {
                OutKind::Int
            } else {
                OutKind::Float
            })
        }
        AggFunc::Min | AggFunc::Max => {
            let c = col.ok_or("aggregate needs a column")?;
            Ok(match col_kind(c) {
                OutKind::Bool => OutKind::Str,
                k if is_decimal_type(&c.data_type) && k == OutKind::Str => OutKind::Float,
                k => k,
            })
        }
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

/// Build `SELECT <cols|aggs> FROM <table> [WHERE ..] [GROUP BY ..] [ORDER BY ..] LIMIT n OFFSET m`.
///
/// Identifiers come only from the introspected `Table` (or from aggregate
/// aliases built from it); values are bound parameters; limit/offset are integers.
pub fn build_sql(d: &dyn Dialect, q: &Select) -> Result<Built, String> {
    let col = |name: &str| q.table.columns.iter().find(|c| c.name == name);
    let cols: Vec<&Column> = q.columns.iter().filter_map(|n| col(n)).collect();
    if cols.is_empty() && q.aggs.is_none_or(|a| a.is_empty()) {
        return Err("no selectable columns".into());
    }
    let mut kinds: Vec<OutKind> = cols.iter().map(|c| col_kind(c)).collect();
    let mut exprs: Vec<String> = cols.iter().map(|c| d.select_expr(c)).collect();
    let mut aliases: Vec<&str> = Vec::new();
    if let Some(aggs) = q.aggs {
        for a in aggs {
            let c = match &a.column {
                Some(n) => Some(col(n).ok_or_else(|| format!("unknown column `{}`", n))?),
                None => None,
            };
            let k = agg_kind(a, c)?;
            exprs.push(format!("{} AS {}", d.agg_expr(a, c, k), d.quote(&a.alias)));
            kinds.push(k);
            aliases.push(&a.alias);
        }
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
        exprs.join(", "),
        d.quote(&q.table.name)
    );
    if !wheres.is_empty() {
        sql.push_str(&format!(" WHERE {}", wheres.join(" AND ")));
    }
    if q.aggs.is_some() && !cols.is_empty() {
        sql.push_str(&format!(
            " GROUP BY {}",
            cols.iter()
                .map(|c| d.quote(&c.name))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }

    let mut orders: Vec<String> = Vec::new();
    for o in q.order {
        let known = if q.aggs.is_some() {
            cols.iter().any(|c| c.name == o.field) || aliases.contains(&o.field.as_str())
        } else {
            col(&o.field).is_some()
        };
        if !known {
            return Err(format!("unknown order field `{}`", o.field));
        }
        orders.push(format!(
            "{}{}",
            d.quote(&o.field),
            if o.desc { " DESC" } else { "" }
        ));
    }
    if orders.is_empty() && q.aggs.is_none() {
        orders = q.table.pk.iter().map(|p| d.quote(p)).collect();
    } else if orders.is_empty() && !cols.is_empty() {
        orders = cols.iter().map(|c| d.quote(&c.name)).collect();
    }
    if !orders.is_empty() {
        sql.push_str(&format!(" ORDER BY {}", orders.join(", ")));
    }
    sql.push_str(&format!(
        " LIMIT {} OFFSET {}",
        q.limit.max(0),
        q.offset.max(0)
    ));
    Ok(Built { sql, params, kinds })
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Ansi;
    impl Dialect for Ansi {}

    fn orders() -> Table {
        Table {
            name: "orders".into(),
            columns: vec![
                Column {
                    name: "id".into(),
                    data_type: "int".into(),
                },
                Column {
                    name: "status".into(),
                    data_type: "varchar".into(),
                },
                Column {
                    name: "total".into(),
                    data_type: "int".into(),
                },
            ],
            pk: vec!["id".into()],
            fks: vec![],
        }
    }

    fn plain<'a>(t: &'a Table, cols: &'a [String], f: &'a [Filter], o: &'a [Order]) -> Select<'a> {
        Select {
            table: t,
            columns: cols,
            filters: f,
            order: o,
            aggs: None,
            limit: 5,
            offset: 0,
        }
    }

    #[test]
    fn unknown_columns_are_dropped_and_values_bound() {
        let t = orders();
        let cols = vec!["status".to_string(), "evil\"; DROP".to_string()];
        let f = vec![Filter {
            column: "status".into(),
            values: vec![DbValue::Str("x' OR '1'='1".into())],
        }];
        let b = build_sql(&Ansi, &plain(&t, &cols, &f, &[])).unwrap();
        assert_eq!(
            b.sql,
            "SELECT \"status\" FROM \"orders\" WHERE \"status\" IN (?) ORDER BY \"id\" LIMIT 5 OFFSET 0"
        );
        assert_eq!(b.params, vec![DbValue::Str("x' OR '1'='1".into())]);
        assert_eq!(b.kinds, vec![OutKind::Str]);
    }

    #[test]
    fn unparsable_int_never_matches() {
        let t = orders();
        let cols = vec!["id".to_string()];
        let f = vec![Filter {
            column: "id".into(),
            values: vec![DbValue::Str("abc".into())],
        }];
        let b = build_sql(&Ansi, &plain(&t, &cols, &f, &[])).unwrap();
        assert!(b.sql.contains("WHERE 1=0") && b.params.is_empty());
    }

    #[test]
    fn order_is_validated() {
        let t = orders();
        let cols = vec!["id".to_string()];
        let ok = [Order {
            field: "total".into(),
            desc: true,
        }];
        assert!(
            build_sql(&Ansi, &plain(&t, &cols, &[], &ok))
                .unwrap()
                .sql
                .contains("ORDER BY \"total\" DESC")
        );
        let bad = [Order {
            field: "x\"; DROP".into(),
            desc: false,
        }];
        assert!(build_sql(&Ansi, &plain(&t, &cols, &[], &bad)).is_err());
    }

    #[test]
    fn group_by_with_aggregates() {
        let t = orders();
        let cols = vec!["status".to_string()];
        let aggs = vec![
            Agg {
                alias: "count".into(),
                func: AggFunc::Count,
                column: None,
            },
            Agg {
                alias: "sum_total".into(),
                func: AggFunc::Sum,
                column: Some("total".into()),
            },
        ];
        let ord = [Order {
            field: "sum_total".into(),
            desc: true,
        }];
        let q = Select {
            table: &t,
            columns: &cols,
            filters: &[],
            order: &ord,
            aggs: Some(&aggs),
            limit: 10,
            offset: 0,
        };
        let b = build_sql(&Ansi, &q).unwrap();
        assert_eq!(
            b.sql,
            "SELECT \"status\", COUNT(*) AS \"count\", SUM(\"total\") AS \"sum_total\" FROM \"orders\" GROUP BY \"status\" ORDER BY \"sum_total\" DESC LIMIT 10 OFFSET 0"
        );
        assert_eq!(b.kinds, vec![OutKind::Str, OutKind::Int, OutKind::Int]);
    }

    #[test]
    fn sum_of_text_is_rejected() {
        let t = orders();
        let aggs = vec![Agg {
            alias: "sum_status".into(),
            func: AggFunc::Sum,
            column: Some("status".into()),
        }];
        let q = Select {
            table: &t,
            columns: &[],
            filters: &[],
            order: &[],
            aggs: Some(&aggs),
            limit: 1,
            offset: 0,
        };
        assert!(build_sql(&Ansi, &q).is_err());
    }
}
