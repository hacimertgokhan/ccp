//! sqlite-ccp: CCP server for a SQLite database file, opened read-only.
//! Env: CCP_PATH (path to the .db file; CCP_DBNAME is accepted as an alias).

use ccp_proto::value::Value;
use ccp_sql::{Backend, Catalog, Column, DbValue, Dialect, ForeignKey, Select, Table, build_sql};
use rusqlite::types::ValueRef;
use rusqlite::{Connection, OpenFlags, params_from_iter};

struct SqliteDialect;
impl Dialect for SqliteDialect {}

struct Sqlite {
    conn: Connection,
}

/// SQLite declares free-form types; map them with its own affinity rule
/// (anything containing "INT" is an integer) onto the names CCP understands.
fn norm_type(declared: &str) -> String {
    let t = declared.to_lowercase();
    if t.contains("int") {
        "integer".into()
    } else if t == "boolean" || t == "bool" {
        "boolean".into()
    } else {
        t
    }
}

impl Backend for Sqlite {
    fn kind(&self) -> &'static str {
        "SQLite"
    }

    fn version(&self) -> String {
        self.conn
            .query_row("SELECT sqlite_version()", [], |r| r.get(0))
            .unwrap_or_default()
    }

    fn introspect(&self) -> Result<Catalog, String> {
        let e = |e: rusqlite::Error| e.to_string();
        let mut names = self
            .conn
            .prepare("SELECT name FROM sqlite_master WHERE type IN ('table','view') AND name NOT LIKE 'sqlite_%' ORDER BY name")
            .map_err(e)?;
        let names: Vec<String> = names
            .query_map([], |r| r.get(0))
            .map_err(e)?
            .collect::<Result<_, _>>()
            .map_err(e)?;
        let mut tables = Vec::new();
        for name in names {
            let mut t = Table {
                name: name.clone(),
                ..Default::default()
            };
            let mut st = self
                .conn
                .prepare("SELECT name, type, pk FROM pragma_table_info(?1)")
                .map_err(e)?;
            let mut pk: Vec<(i64, String)> = Vec::new();
            for r in st
                .query_map([&name], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, i64>(2)?,
                    ))
                })
                .map_err(e)?
            {
                let (c, ty, p) = r.map_err(e)?;
                if p > 0 {
                    pk.push((p, c.clone()));
                }
                t.columns.push(Column {
                    name: c,
                    data_type: norm_type(&ty),
                });
            }
            pk.sort();
            t.pk = pk.into_iter().map(|(_, c)| c).collect();

            let mut st = self
                .conn
                .prepare("SELECT id, \"from\", \"table\", \"to\" FROM pragma_foreign_key_list(?1)")
                .map_err(e)?;
            let fk_rows: Vec<(i64, String, String, Option<String>)> = st
                .query_map([&name], |r| {
                    Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
                })
                .map_err(e)?
                .collect::<Result<_, _>>()
                .map_err(e)?;
            for (id, from, table, to) in &fk_rows {
                if fk_rows.iter().filter(|f| f.0 == *id).count() > 1 {
                    continue; // composite FK: not exposed
                }
                // `to` is NULL when the FK references the parent's primary key.
                let ref_col = match to {
                    Some(c) => c.clone(),
                    None => {
                        let mut s = self
                            .conn
                            .prepare("SELECT name FROM pragma_table_info(?1) WHERE pk = 1")
                            .map_err(e)?;
                        match s.query_row([table], |r| r.get::<_, String>(0)) {
                            Ok(c) => c,
                            Err(_) => continue,
                        }
                    }
                };
                t.fks.push(ForeignKey {
                    column: from.clone(),
                    ref_table: table.clone(),
                    ref_column: ref_col,
                });
            }
            tables.push(t);
        }
        Ok(Catalog { tables })
    }

    fn select(&self, q: &Select) -> Result<Vec<Vec<Value>>, String> {
        let (sql, params) = build_sql(&SqliteDialect, q)?;
        let mut st = self.conn.prepare(&sql).map_err(|e| e.to_string())?;
        let n = st.column_count();
        let params = params.into_iter().map(|p| match p {
            DbValue::Int(i) => rusqlite::types::Value::Integer(i),
            DbValue::Str(s) => rusqlite::types::Value::Text(s),
        });
        let rows = st
            .query_map(params_from_iter(params), |r| {
                (0..n)
                    .map(|i| {
                        Ok(match r.get_ref(i)? {
                            ValueRef::Null => Value::Null,
                            ValueRef::Integer(i) => Value::Int(i),
                            ValueRef::Real(f) => Value::Str(f.to_string()),
                            ValueRef::Text(t) => {
                                Value::Str(String::from_utf8_lossy(t).into_owned())
                            }
                            ValueRef::Blob(b) => Value::Str(format!("<blob {} bytes>", b.len())),
                        })
                    })
                    .collect::<Result<Vec<_>, _>>()
            })
            .map_err(|e| e.to_string())?;
        rows.collect::<Result<_, _>>().map_err(|e| e.to_string())
    }
}

fn connect() -> Result<Box<dyn Backend>, String> {
    let path =
        ccp_sql::env_required("CCP_PATH").or_else(|_| ccp_sql::env_required("CCP_DBNAME"))?;
    let conn = Connection::open_with_flags(
        &path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|e| format!("cannot open {}: {}", path, e))?;
    Ok(Box::new(Sqlite { conn }))
}

fn main() {
    ccp_sql::main("sqlite-ccp", "SQLite", connect);
}

#[cfg(test)]
mod tests {
    use super::*;
    use ccp_proto::serve::answer_query;
    use ccp_sql::Executor;

    fn seeded() -> Executor {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE users(id INTEGER PRIMARY KEY, name TEXT, active BOOLEAN);
             CREATE TABLE posts(id INTEGER PRIMARY KEY, author_id INTEGER, title TEXT,
                                FOREIGN KEY(author_id) REFERENCES users(id));
             CREATE TABLE comments(id INTEGER PRIMARY KEY, post_id INTEGER, body TEXT,
                                   FOREIGN KEY(post_id) REFERENCES posts(id));
             INSERT INTO users VALUES (1,'Mert',1),(2,'Ayse',0),(3,'NoPosts',1);
             INSERT INTO posts VALUES (10,1,'CCP'),(11,1,'Context'),(12,2,'Schema');
             INSERT INTO comments VALUES (100,10,'nice'),(101,10,'ok'),(102,12,'yes');",
        )
        .unwrap();
        Executor::new(Box::new(Sqlite { conn })).unwrap()
    }

    #[test]
    fn flat_query_and_filters() {
        let ex = seeded();
        assert_eq!(
            answer_query(&ex, "users{id name}"),
            r#"{users:[{id:1,name:"Mert"},{id:2,name:"Ayse"},{id:3,name:"NoPosts"}]}"#
        );
        assert_eq!(
            answer_query(&ex, "users(id:2){name}"),
            r#"{users:[{name:"Ayse"}]}"#
        );
        assert_eq!(
            answer_query(&ex, "users(name:\"x' OR '1'='1\"){id}"),
            "{users:[]}"
        );
        assert_eq!(
            answer_query(&ex, "users(limit:1,offset:1){id}"),
            "{users:[{id:2}]}"
        );
    }

    #[test]
    fn nested_relations_in_both_directions() {
        let ex = seeded();
        assert_eq!(
            answer_query(&ex, "users(id:1){name posts{title comments{body}}}"),
            r#"{users:[{name:"Mert",posts:[{title:"CCP",comments:[{body:"nice"},{body:"ok"}]},{title:"Context",comments:[]}]}]}"#
        );
        assert_eq!(
            answer_query(&ex, "posts(id:12){title author{name}}"),
            r#"{posts:[{title:"Schema",author:{name:"Ayse"}}]}"#
        );
        // a user without posts yields an empty list, not an error
        assert_eq!(
            answer_query(&ex, "users(id:3){posts{title}}"),
            "{users:[{posts:[]}]}"
        );
    }

    #[test]
    fn schema_lists_relations() {
        let s = seeded().catalog.schema_text();
        assert!(
            s.contains("type Users{id:Int,name:Str,active:Bool,posts:[Posts]}"),
            "{}",
            s
        );
        assert!(
            s.contains(
                "type Posts{id:Int,author_id:Int,title:Str,author:Users,comments:[Comments]}"
            ),
            "{}",
            s
        );
    }

    #[test]
    fn database_errors_surface_without_panicking() {
        let ex = seeded();
        assert_eq!(answer_query(&ex, "nope{id}"), "{nope:null}");
    }
}
