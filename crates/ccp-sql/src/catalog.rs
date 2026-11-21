use std::collections::HashSet;

#[derive(Clone, Debug)]
pub struct Column {
    pub name: String,
    /// Normalised lowercase type name (e.g. `int`, `bigint`, `varchar`, `boolean`).
    pub data_type: String,
}

#[derive(Clone, Debug)]
pub struct ForeignKey {
    pub column: String,
    pub ref_table: String,
    pub ref_column: String,
}

#[derive(Clone, Debug, Default)]
pub struct Table {
    pub name: String,
    pub columns: Vec<Column>,
    pub pk: Vec<String>,
    pub fks: Vec<ForeignKey>,
}

/// A navigable relation derived from a foreign key.
#[derive(Clone, Debug)]
pub enum Rel {
    /// This table's `column` points at `target.target_column` (e.g. `posts.author`).
    BelongsTo {
        name: String,
        column: String,
        target: String,
        target_column: String,
    },
    /// `child.child_column` points at this table's `parent_column` (e.g. `users.posts`).
    HasMany {
        name: String,
        child: String,
        child_column: String,
        parent_column: String,
    },
}

impl Rel {
    pub fn name(&self) -> &str {
        match self {
            Rel::BelongsTo { name, .. } | Rel::HasMany { name, .. } => name,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct Catalog {
    pub tables: Vec<Table>,
}

pub fn is_ident(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

pub fn is_int_type(t: &str) -> bool {
    matches!(
        t,
        "tinyint"
            | "smallint"
            | "mediumint"
            | "int"
            | "integer"
            | "bigint"
            | "year"
            | "int2"
            | "int4"
            | "int8"
    )
}

pub fn is_bool_type(t: &str) -> bool {
    matches!(t, "boolean" | "bool")
}

pub fn is_float_type(t: &str) -> bool {
    matches!(
        t,
        "float" | "double" | "real" | "float4" | "float8" | "double precision"
    )
}

/// Exact decimals stay strings in plain selects (no precision loss) but can be aggregated.
pub fn is_decimal_type(t: &str) -> bool {
    matches!(t, "decimal" | "numeric" | "dec" | "money")
}

pub fn ccp_type(t: &str) -> &'static str {
    if is_int_type(t) {
        "Int"
    } else if is_float_type(t) {
        "Float"
    } else if is_bool_type(t) {
        "Bool"
    } else {
        "Str"
    }
}

pub fn type_name(table: &str) -> String {
    let mut c = table.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}

impl Catalog {
    /// CCP identifiers are `[A-Za-z0-9_]`; anything else cannot be addressed by
    /// the query language, so it is not exposed. Dangling foreign keys are dropped.
    pub fn sanitize(mut self) -> Catalog {
        self.tables.retain(|t| is_ident(&t.name));
        for t in &mut self.tables {
            t.columns.retain(|c| is_ident(&c.name));
        }
        let snapshot = self.tables.clone();
        for t in &mut self.tables {
            let cols: Vec<String> = t.columns.iter().map(|c| c.name.clone()).collect();
            t.pk.retain(|p| cols.contains(p));
            t.fks.retain(|fk| {
                cols.contains(&fk.column)
                    && snapshot.iter().any(|o| {
                        o.name == fk.ref_table && o.columns.iter().any(|c| c.name == fk.ref_column)
                    })
            });
        }
        self
    }

    pub fn table(&self, name: &str) -> Option<&Table> {
        self.tables.iter().find(|t| t.name == name)
    }

    pub fn relations(&self, table: &Table) -> Vec<Rel> {
        let mut used: HashSet<String> = table.columns.iter().map(|c| c.name.clone()).collect();
        let mut rels = Vec::new();
        for fk in &table.fks {
            let name = fk
                .column
                .strip_suffix("_id")
                .filter(|s| !s.is_empty())
                .unwrap_or(&fk.ref_table)
                .to_string();
            if used.insert(name.clone()) {
                rels.push(Rel::BelongsTo {
                    name,
                    column: fk.column.clone(),
                    target: fk.ref_table.clone(),
                    target_column: fk.ref_column.clone(),
                });
            }
        }
        for other in &self.tables {
            for fk in other.fks.iter().filter(|fk| fk.ref_table == table.name) {
                if used.insert(other.name.clone()) {
                    rels.push(Rel::HasMany {
                        name: other.name.clone(),
                        child: other.name.clone(),
                        child_column: fk.column.clone(),
                        parent_column: fk.ref_column.clone(),
                    });
                }
            }
        }
        rels
    }

    /// The CCP schema advertisement, generated from the live database.
    pub fn schema_text(&self) -> String {
        let mut out = String::from("root _tables:[_Table]\n");
        for t in &self.tables {
            let args: Vec<String> = t
                .columns
                .iter()
                .map(|c| format!("{}:{}", c.name, ccp_type(&c.data_type)))
                .collect();
            out.push_str(&format!(
                "root {}({}):[{}]\n",
                t.name,
                args.join(","),
                type_name(&t.name)
            ));
        }
        // Said once, not per table: every table root takes these.
        out.push_str(
            "all table roots also take limit:Int,offset:Int,order:\"-f,g\",group:\"cols\"; with group the braces take count,sum_C,avg_C,min_C,max_C,distinct_C\ntype _Table{name:Str,columns:[_Column]}\ntype _Column{name:Str,type:Str,native:Str}",
        );
        for t in &self.tables {
            let mut fields: Vec<String> = t
                .columns
                .iter()
                .map(|c| format!("{}:{}", c.name, ccp_type(&c.data_type)))
                .collect();
            for r in self.relations(t) {
                fields.push(match r {
                    Rel::BelongsTo { name, target, .. } => {
                        format!("{}:{}", name, type_name(&target))
                    }
                    Rel::HasMany { name, child, .. } => format!("{}:[{}]", name, type_name(&child)),
                });
            }
            out.push_str(&format!(
                "\ntype {}{{{}}}",
                type_name(&t.name),
                fields.join(",")
            ));
        }
        out
    }
}
