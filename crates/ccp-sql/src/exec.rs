use crate::catalog::{Catalog, Rel, Table};
use crate::sqlgen::{DbValue, Filter, Select};
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
    /// Rows come back aligned with `q.columns` (after unknown ones are dropped).
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

    #[allow(clippy::too_many_arguments)]
    fn fetch(
        &self,
        table: &Table,
        filters: &[Filter],
        limit: i64,
        offset: i64,
        sub: &[Selection],
        extra: &[String],
        depth: usize,
    ) -> Result<Fetched, String> {
        if depth > MAX_DEPTH {
            return Err(format!(
                "relation nesting exceeds max depth ({})",
                MAX_DEPTH
            ));
        }
        let rels = self.catalog.relations(table);
        let mut out_cols: Vec<String> = Vec::new();
        for s in sub {
            if table.columns.iter().any(|c| c.name == s.name) && !out_cols.contains(&s.name) {
                out_cols.push(s.name.clone());
            }
        }
        let wants: Vec<(&Selection, &Rel)> = sub
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
        for e in extra {
            need(e);
        }
        for (_, r) in &wants {
            match r {
                Rel::BelongsTo { column, .. } => need(column),
                Rel::HasMany { parent_column, .. } => need(parent_column),
            }
        }
        let raw = self.backend.select(&Select {
            table,
            columns: &cols,
            filters,
            limit,
            offset,
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
                    child,
                    &[filter],
                    REL_LIMIT,
                    0,
                    &sel.sub,
                    std::slice::from_ref(child_col),
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
        match self.fetch(table, &filters, limit, offset, sub, &[], 0) {
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
