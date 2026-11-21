use crate::catalog::{Catalog, Rel, Table};
use crate::sqlgen::{Agg, AggFunc, DbValue, Filter, Order, OutKind, Select};
use ccp_proto::engine::{Args, FieldResult, Resolvable, RootResolver};
use ccp_proto::query::{ArgValue, Selection};
use ccp_proto::value::Value;
use std::collections::HashMap;

pub const DEFAULT_LIMIT: i64 = 100;
pub const MAX_LIMIT: i64 = 1000;
/// Upper bound on rows fetched for one nested relation level.
pub const REL_LIMIT: i64 = 10_000;
pub const MAX_DEPTH: usize = 4;

/// What a database driver provides. `select` receives a [`Select`] and must
/// build SQL with [`crate::build_sql`] so identifiers/values stay safe.
pub trait Backend {
    fn kind(&self) -> &'static str;
    fn version(&self) -> String;
    fn introspect(&self) -> Result<Catalog, String>;
    /// Rows come back aligned with the output columns of the built statement.
    fn select(&self, q: &Select) -> Result<Vec<Vec<Value>>, String>;
}

pub struct Executor {
    backend: Box<dyn Backend>,
    pub catalog: Catalog,
}

#[derive(Clone)]
enum RelVal {
    One(Option<RowData>),
    Many(Vec<RowData>),
}

#[derive(Clone)]
struct RowData {
    vals: Vec<(String, Value)>,
    rels: Vec<(String, RelVal)>,
}

struct Fetched {
    cols: Vec<String>,
    rows: Vec<(RowData, Vec<Value>)>,
}

struct RowNode(RowData);

impl Resolvable for RowNode {
    fn resolve_field(&self, field: &str, _args: &Args) -> FieldResult {
        if let Some((_, v)) = self.0.vals.iter().find(|(k, _)| k == field) {
            return FieldResult::Scalar(v.clone());
        }
        match self.0.rels.iter().find(|(k, _)| k == field) {
            Some((_, RelVal::One(Some(r)))) => FieldResult::Node(Box::new(RowNode(r.clone()))),
            Some((_, RelVal::One(None))) => FieldResult::Scalar(Value::Null),
            Some((_, RelVal::Many(rs))) => FieldResult::List(
                rs.iter()
                    .map(|r| Box::new(RowNode(r.clone())) as Box<dyn Resolvable>)
                    .collect(),
            ),
            None => FieldResult::Missing,
        }
    }
}

fn to_db(v: &Value) -> Option<DbValue> {
    match v {
        Value::Int(i) => Some(DbValue::Int(*i)),
        Value::Str(s) => Some(DbValue::Str(s.clone())),
        Value::Bool(b) => Some(DbValue::Int(*b as i64)),
        _ => None,
    }
}

/// Make driver output match the declared kind (e.g. decimals arrive as text).
fn normalize(kind: OutKind, v: Value) -> Value {
    match (kind, v) {
        (OutKind::Float, Value::Str(s)) => s
            .trim()
            .parse::<f64>()
            .map(Value::Float)
            .unwrap_or(Value::Str(s)),
        (OutKind::Float, Value::Int(i)) => Value::Float(i as f64),
        (OutKind::Int, Value::Str(s)) => s
            .trim()
            .parse::<i64>()
            .map(Value::Int)
            .unwrap_or(Value::Str(s)),
        (OutKind::Bool, Value::Int(i)) => Value::Bool(i != 0),
        (_, v) => v,
    }
}

struct FetchReq<'a> {
    table: &'a Table,
    filters: &'a [Filter],
    order: &'a [Order],
    limit: i64,
    offset: i64,
    sub: &'a [Selection],
    extra: &'a [String],
}

fn parse_order(s: &str) -> Vec<Order> {
    s.split(',')
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .map(|p| match p.strip_prefix('-') {
            Some(f) => Order {
                field: f.to_string(),
                desc: true,
            },
            None => Order {
                field: p.trim_start_matches('+').to_string(),
                desc: false,
            },
        })
        .collect()
}

/// `count`, `sum_total`, `avg_total`, `min_x`, `max_x`, `distinct_x`.
fn parse_agg(name: &str, table: &Table) -> Option<Agg> {
    if name == "count" {
        return Some(Agg {
            alias: name.into(),
            func: AggFunc::Count,
            column: None,
        });
    }
    for (prefix, func) in [
        ("sum_", AggFunc::Sum),
        ("avg_", AggFunc::Avg),
        ("min_", AggFunc::Min),
        ("max_", AggFunc::Max),
        ("distinct_", AggFunc::CountDistinct),
    ] {
        if let Some(col) = name.strip_prefix(prefix)
            && table.columns.iter().any(|c| c.name == col)
        {
            return Some(Agg {
                alias: name.into(),
                func,
                column: Some(col.into()),
            });
        }
    }
    None
}

impl Executor {
    pub fn new(backend: Box<dyn Backend>) -> Result<Executor, String> {
        let catalog = backend.introspect()?.sanitize();
        Ok(Executor { backend, catalog })
    }

    pub fn kind(&self) -> &'static str {
        self.backend.kind()
    }

    pub fn info(&self) -> String {
        format!(
            "{} {} ({} tables)",
            self.backend.kind(),
            self.backend.version(),
            self.catalog.tables.len()
        )
    }

    fn run(&self, q: &Select) -> Result<Vec<Vec<Value>>, String> {
        let kinds = crate::sqlgen::build_sql(&NullDialect, q)?.kinds;
        let rows = self.backend.select(q)?;
        Ok(rows
            .into_iter()
            .map(|r| {
                r.into_iter()
                    .enumerate()
                    .map(|(i, v)| kinds.get(i).map_or(v.clone(), |k| normalize(*k, v)))
                    .collect()
            })
            .collect())
    }

    /// Group mode: `table(group:"a,b",order:"-sum_x"){a b count sum_x}`.
    fn aggregate(&self, req: &FetchReq, group: &str) -> Result<Vec<RowData>, String> {
        let (table, filters, order, limit, offset, sub) = (
            req.table,
            req.filters,
            req.order,
            req.limit,
            req.offset,
            req.sub,
        );
        let group_cols: Vec<String> = group
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(String::from)
            .collect();
        for g in &group_cols {
            if !table.columns.iter().any(|c| &c.name == g) {
                return Err(format!("unknown group column `{}`", g));
            }
        }
        let mut aggs: Vec<Agg> = Vec::new();
        let mut out_names: Vec<String> = Vec::new();
        for s in sub {
            if group_cols.contains(&s.name) {
                out_names.push(s.name.clone());
            } else if let Some(a) = parse_agg(&s.name, table) {
                if !aggs.iter().any(|x| x.alias == a.alias) {
                    aggs.push(a);
                }
                out_names.push(s.name.clone());
            }
        }
        if aggs.is_empty() {
            aggs.push(Agg {
                alias: "count".into(),
                func: AggFunc::Count,
                column: None,
            });
            if !out_names.contains(&"count".to_string()) {
                out_names.push("count".into());
            }
        }
        let rows = self.run(&Select {
            table,
            columns: &group_cols,
            filters,
            order,
            aggs: Some(&aggs),
            limit,
            offset,
        })?;
        let names: Vec<&String> = group_cols
            .iter()
            .chain(aggs.iter().map(|a| &a.alias))
            .collect();
        Ok(rows
            .into_iter()
            .map(|r| {
                let vals = out_names
                    .iter()
                    .filter_map(|n| {
                        names
                            .iter()
                            .position(|x| *x == n)
                            .map(|i| (n.clone(), r[i].clone()))
                    })
                    .collect();
                RowData {
                    vals,
                    rels: Vec::new(),
                }
            })
            .collect())
    }

    fn fetch(&self, req: &FetchReq, depth: usize) -> Result<Fetched, String> {
        if depth > MAX_DEPTH {
            return Err(format!(
                "relation nesting exceeds max depth ({})",
                MAX_DEPTH
            ));
        }
        let table = req.table;
        let rels = self.catalog.relations(table);
        let mut out_cols: Vec<String> = Vec::new();
        for s in req.sub {
            if table.columns.iter().any(|c| c.name == s.name) && !out_cols.contains(&s.name) {
                out_cols.push(s.name.clone());
            }
        }
        let wants: Vec<(&Selection, &Rel)> = req
            .sub
            .iter()
            .filter_map(|s| rels.iter().find(|r| r.name() == s.name).map(|r| (s, r)))
            .collect();
        if out_cols.is_empty() && wants.is_empty() {
            out_cols = table.columns.iter().map(|c| c.name.clone()).collect();
        }
        let mut cols = out_cols.clone();
        let mut need = |c: &str| {
            if !cols.iter().any(|x| x == c) {
                cols.push(c.to_string());
            }
        };
        for e in req.extra {
            need(e);
        }
        for (_, r) in &wants {
            match r {
                Rel::BelongsTo { column, .. } => need(column),
                Rel::HasMany { parent_column, .. } => need(parent_column),
            }
        }
        let raw = self.run(&Select {
            table,
            columns: &cols,
            filters: req.filters,
            order: req.order,
            aggs: None,
            limit: req.limit,
            offset: req.offset,
        })?;
        let pos = |c: &str| cols.iter().position(|x| x == c).unwrap();
        let mut rows: Vec<(RowData, Vec<Value>)> = raw
            .into_iter()
            .map(|r| {
                let vals = out_cols
                    .iter()
                    .map(|c| (c.clone(), r[pos(c)].clone()))
                    .collect();
                (
                    RowData {
                        vals,
                        rels: Vec::new(),
                    },
                    r,
                )
            })
            .collect();

        for (sel, rel) in wants {
            let (parent_col, child_table, child_col, many) = match rel {
                Rel::BelongsTo {
                    column,
                    target,
                    target_column,
                    ..
                } => (column, target, target_column, false),
                Rel::HasMany {
                    child,
                    child_column,
                    parent_column,
                    ..
                } => (parent_column, child, child_column, true),
            };
            let Some(child) = self.catalog.table(child_table) else {
                continue;
            };
            let pi = pos(parent_col);
            let mut keys: Vec<DbValue> = Vec::new();
            for (_, raw) in &rows {
                if let Some(k) = to_db(&raw[pi])
                    && !keys.contains(&k)
                {
                    keys.push(k);
                }
            }
            let filter = Filter {
                column: child_col.clone(),
                values: keys,
            };
            let sub_fetch = if filter.values.is_empty() {
                Fetched {
                    cols: vec![],
                    rows: vec![],
                }
            } else {
                self.fetch(
                    &FetchReq {
                        table: child,
                        filters: std::slice::from_ref(&filter),
                        order: &[],
                        limit: REL_LIMIT,
                        offset: 0,
                        sub: &sel.sub,
                        extra: std::slice::from_ref(child_col),
                    },
                    depth + 1,
                )?
            };
            let ci = sub_fetch.cols.iter().position(|c| c == child_col);
            let mut groups: HashMap<String, Vec<RowData>> = HashMap::new();
            if let Some(ci) = ci {
                for (rd, raw) in sub_fetch.rows {
                    groups
                        .entry(raw[ci].to_compact_string())
                        .or_default()
                        .push(rd);
                }
            }
            for (rd, raw) in rows.iter_mut() {
                let mut hit = groups
                    .get(&raw[pi].to_compact_string())
                    .cloned()
                    .unwrap_or_default();
                let val = if many {
                    RelVal::Many(hit)
                } else {
                    RelVal::One(if hit.is_empty() {
                        None
                    } else {
                        Some(hit.swap_remove(0))
                    })
                };
                rd.rels.push((sel.name.clone(), val));
            }
        }
        Ok(Fetched { cols, rows })
    }
}

/// Only used to compute output kinds (`build_sql` needs a dialect); never executed.
struct NullDialect;
impl crate::sqlgen::Dialect for NullDialect {}

struct TableMeta(Table);
struct ColumnMeta(String, String);

impl Resolvable for TableMeta {
    fn resolve_field(&self, field: &str, _args: &Args) -> FieldResult {
        match field {
            "name" => FieldResult::Scalar(Value::Str(self.0.name.clone())),
            "columns" => FieldResult::List(
                self.0
                    .columns
                    .iter()
                    .map(|c| {
                        Box::new(ColumnMeta(c.name.clone(), c.data_type.clone()))
                            as Box<dyn Resolvable>
                    })
                    .collect(),
            ),
            _ => FieldResult::Missing,
        }
    }
}

impl Resolvable for ColumnMeta {
    fn resolve_field(&self, field: &str, _args: &Args) -> FieldResult {
        match field {
            "name" => FieldResult::Scalar(Value::Str(self.0.clone())),
            "type" => {
                FieldResult::Scalar(Value::Str(crate::catalog::ccp_type(&self.1).to_string()))
            }
            "native" => FieldResult::Scalar(Value::Str(self.1.clone())),
            _ => FieldResult::Missing,
        }
    }
}

impl RootResolver for Executor {
    fn resolve_root(&self, name: &str, args: &Args, sub: &[Selection]) -> FieldResult {
        if name == "_tables" {
            return FieldResult::List(
                self.catalog
                    .tables
                    .iter()
                    .map(|t| Box::new(TableMeta(t.clone())) as Box<dyn Resolvable>)
                    .collect(),
            );
        }
        let Some(table) = self.catalog.table(name) else {
            return FieldResult::Missing;
        };

        let mut filters: Vec<Filter> = Vec::new();
        let mut keys: Vec<&String> = args
            .keys()
            .filter(|k| table.columns.iter().any(|c| &c.name == *k))
            .collect();
        keys.sort();
        for k in keys {
            let v = match &args[k] {
                ArgValue::Int(i) => DbValue::Int(*i),
                ArgValue::Str(s) => DbValue::Str(s.clone()),
                ArgValue::Bool(b) => DbValue::Int(*b as i64),
            };
            filters.push(Filter {
                column: k.clone(),
                values: vec![v],
            });
        }
        let int_arg = |n: &str, d: i64| match args.get(n) {
            Some(ArgValue::Int(i)) => *i,
            _ => d,
        };
        let limit = int_arg("limit", DEFAULT_LIMIT).clamp(0, MAX_LIMIT);
        let offset = int_arg("offset", 0).max(0);
        let order = match args.get("order") {
            Some(ArgValue::Str(s)) => parse_order(s),
            _ => Vec::new(),
        };

        if let Some(ArgValue::Str(group)) = args.get("group") {
            return match self.aggregate(
                &FetchReq {
                    table,
                    filters: &filters,
                    order: &order,
                    limit,
                    offset,
                    sub,
                    extra: &[],
                },
                group,
            ) {
                Ok(rows) => FieldResult::List(
                    rows.into_iter()
                        .map(|rd| Box::new(RowNode(rd)) as Box<dyn Resolvable>)
                        .collect(),
                ),
                Err(e) => FieldResult::Error(e),
            };
        }
        let req = FetchReq {
            table,
            filters: &filters,
            order: &order,
            limit,
            offset,
            sub,
            extra: &[],
        };
        match self.fetch(&req, 0) {
            Ok(f) => FieldResult::List(
                f.rows
                    .into_iter()
                    .map(|(rd, _)| Box::new(RowNode(rd)) as Box<dyn Resolvable>)
                    .collect(),
            ),
            Err(e) => FieldResult::Error(e),
        }
    }
}
