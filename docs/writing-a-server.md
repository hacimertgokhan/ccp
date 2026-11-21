# Writing a CCP server

A CCP server turns some data source into a schema plus a way to answer queries.
There are two ways to build one.

## A. A SQL-like database — implement two traits (≈150 lines)

[`ccp-sql`](../crates/ccp-sql) already provides introspection-driven schemas,
foreign-key relations, safe query building, an MCP server and the raw protocol.
Your crate supplies the driver.

```toml
# plugins/foo-ccp/Cargo.toml
[package]
name = "foo-ccp"
version.workspace = true
edition.workspace = true
license.workspace = true
description = "CCP server for Foo (read-only, schema-introspecting, relation-aware)"

[dependencies]
ccp-proto = { path = "../../crates/ccp-proto" }
ccp-sql = { path = "../../crates/ccp-sql" }
foo-driver = "1"
```

```rust
use ccp_proto::value::Value;
use ccp_sql::{build_sql, Backend, Catalog, Dialect, Select};

struct FooDialect;
impl Dialect for FooDialect {
    // defaults: "double-quoted" identifiers and `?` placeholders.
    // Override quote / select_expr / filter_lhs / placeholder when your
    // database differs (see mysql-ccp for backticks, postgres-ccp for casts).
}

struct Foo { /* connection (use RefCell if the driver needs &mut) */ }

impl Backend for Foo {
    fn kind(&self) -> &'static str { "Foo" }
    fn version(&self) -> String { /* SELECT version() */ }

    // Tables, columns (lowercase type names), primary keys, single-column foreign keys.
    fn introspect(&self) -> Result<Catalog, String> { /* read the catalog */ }

    // Run the generated SQL with bound parameters; return rows aligned with q.columns.
    fn select(&self, q: &Select) -> Result<Vec<Vec<Value>>, String> {
        let (sql, params) = build_sql(&FooDialect, q)?;
        /* bind `params` (DbValue::Int / DbValue::Str), execute, convert rows to Value */
    }
}

fn connect() -> Result<Box<dyn Backend>, String> { /* read CCP_* env vars, connect read-only */ }

fn main() { ccp_sql::main("foo-ccp", "Foo", connect); }
```

Rules every server must follow:

1. **Read-only.** Open the connection or session read-only where the database allows it.
2. **Never build SQL from caller text.** Always go through `build_sql`; values are parameters.
3. **Configuration through environment variables** (`CCP_HOST`, `CCP_PORT`, `CCP_USER`,
   `CCP_PASSWORD`, `CCP_DBNAME`, or `CCP_PATH` for files) so it works with `claude mcp add -e`.
4. **Scrub secrets** from every error message with `ccp_sql::scrub`.
5. **Normalise types** to lowercase names (`int`, `bigint`, `varchar`, `boolean`, …) so the
   schema maps to `Int`/`Str`/`Bool`.

Then:

* add the crate to `members` in the root `Cargo.toml`;
* add tests (see `plugins/sqlite-ccp` for an in-memory example covering filters and nested relations);
* add a row to the server table in the README and a `plugins/foo-ccp/README.md`;
* run the parity check in [`bench/bench.py`](../bench/bench.py) if the database can run in Docker:
  the same queries must return byte-identical output on every backend.

## B. Anything else — implement one trait

For a non-SQL source (key-value store, REST API, files) depend only on
[`ccp-proto`](../crates/ccp-proto): implement `RootResolver` and call `serve_stdio`.

```rust
use ccp_proto::engine::{Args, FieldResult, RootResolver};
use ccp_proto::query::Selection;
use ccp_proto::serve::serve_stdio;
use ccp_proto::value::Value;

struct Greeter;
impl RootResolver for Greeter {
    fn resolve_root(&self, name: &str, _args: &Args, _sub: &[Selection]) -> FieldResult {
        match name {
            "hello" => FieldResult::Scalar(Value::Str("world".into())),
            _ => FieldResult::Missing,
        }
    }
}

fn main() {
    serve_stdio("root hello:Str", &Greeter, &|| Ok("greeter".into()));
}
```

`FieldResult` can be a `Scalar`, a `Node` (object implementing `Resolvable`), a `List` of nodes,
`Missing` (renders `null`) or `Error(msg)`. The `sub` slice tells you which fields were
requested, so you can avoid fetching the rest. `crates/ccp/src/demo.rs` is a complete example.
