# Writing a CCP server

A CCP server turns some data source into a vocabulary (a schema) plus a way to answer
queries. CCP is not only for databases: anything a model might want *a small, shaped
slice of* is a candidate: a cache, a log, a queue, an API, a feed, a file, a report.

Pick the closest starting point:

| Your source | Build on | Effort |
|---|---|---|
| A SQL database | [`ccp-sql`](../crates/ccp-sql): implement `Backend` + `Dialect` | ≈150 lines |
| A live event feed | [`ccp-stream`](../crates/ccp-stream): write the reader loop | ≈100 lines |
| JSON from anywhere (API, files, commands) | [`ccp-mcp`](../crates/ccp-mcp): `Service` + the JSON projector | ≈100 lines |
| Something else | [`ccp-proto`](../crates/ccp-proto): `RootResolver` + `serve_stdio` | as needed |

Every path ends in `ccp_mcp::main(...)`, which gives you the MCP server for Claude and the
raw `--ccp` protocol for free.

## The recipe (all server types)

1. **Roots are verbs on your data**, named for what the model would say (`messages`, `keys`, `get`).
2. **Selection is projection.** The `sub` slice passed to your resolver says which fields were
   requested: fetch and return only those. This is where the token savings come from.
3. **Bound everything**: rows, bytes, time, nesting. Give list roots a small default `limit`.
4. **Read-only.** Never expose a way to change the source.
5. **Never let caller text become a command.** Use bound parameters, fixed command sets, or
   fixed base URLs. Keys and patterns are data, not syntax.
6. **Configure through environment variables** (`CCP_HOST`, `CCP_PORT`, `CCP_USER`, `CCP_PASSWORD`,
   `CCP_URL`, `CCP_PATH`, …) so `claude mcp add -e` works, and scrub secrets from errors with `ccp_mcp::scrub`.
7. **Report problems in place** with `FieldResult::Error(msg)` rather than panicking.
8. Prefix any meta field you add with `_`.

## A. SQL database

```rust
use ccp_proto::value::Value;
use ccp_sql::{build_sql, Backend, Catalog, Dialect, Select};

struct FooDialect;
impl Dialect for FooDialect {
    // defaults: "double-quoted" identifiers and `?` placeholders.
    // Override quote / select_expr / filter_lhs / placeholder / agg_expr when your database
    // differs (mysql-ccp: backticks; postgres-ccp: explicit casts).
}

struct Foo { /* connection; use RefCell if the driver needs &mut */ }

impl Backend for Foo {
    fn kind(&self) -> &'static str { "Foo" }
    fn version(&self) -> String { /* SELECT version() */ }
    // Tables, columns (lowercase type names), primary keys, single-column foreign keys.
    fn introspect(&self) -> Result<Catalog, String> { /* read the catalog */ }
    fn select(&self, q: &Select) -> Result<Vec<Vec<Value>>, String> {
        let built = build_sql(&FooDialect, q)?;       // built.sql, built.params, built.kinds
        /* bind built.params (DbValue::Int / Str), run, decode rows by built.kinds */
    }
}

fn connect() -> Result<Box<dyn Backend>, String> { /* read CCP_* env vars, connect read-only */ }
fn main() { ccp_sql::main("foo-ccp", "Foo", connect); }
```

You get relations, filters, ordering, paging and `group`/aggregate reports without writing them.
`plugins/csv-ccp` shows reuse at its smallest: it loads files into in-memory SQLite and hands it to
the SQLite backend.

## B. Event stream

```rust
use ccp_stream::{Buffer, StreamService, spawn_reader};

fn session(buf: &Buffer) -> Result<(), String> {
    /* connect; buf.set_connected(true, None); loop { buf.push(Some(event_name), payload_text) } */
    /* return Err(reason) when the connection ends; the helper reconnects with backoff */
}

fn connect() -> Result<Box<dyn ccp_mcp::Service>, String> {
    let buf = Buffer::new(5000, 32 << 20);
    spawn_reader(buf.clone(), session);
    buf.wait_first(std::time::Duration::from_secs(12))?;          // fail fast on bad URL/auth
    Ok(Box::new(StreamService::new(buf, "Foo feed".into(), "Listening to foo.".into())))
}
fn main() { ccp_mcp::main(ccp_stream::meta("foo-ccp", "Foo"), connect); }
```

`messages`, `summary` and `status` roots, field selection over JSON payloads, `wait`/`since`
semantics and the ring buffer come with it. See `plugins/websocket-ccp` and `plugins/socketio-ccp`.

## C. JSON from anywhere

Implement `ccp_mcp::Service` and use the projector so only requested fields survive:

```rust
use ccp_mcp::{project, project_all, Meta, Service};
use ccp_proto::engine::{Args, FieldResult, RootResolver};
use ccp_proto::query::Selection;

struct Foo;
impl RootResolver for Foo {
    fn resolve_root(&self, name: &str, args: &Args, sub: &[Selection]) -> FieldResult {
        match name {
            "thing" => {
                let json: serde_json::Value = /* fetch it */;
                FieldResult::Scalar(if sub.is_empty() { project_all(&json) } else { project(&json, sub) })
            }
            _ => FieldResult::Missing,
        }
    }
}
// impl Service for Foo { schema(), root(), info(), hint() }  then:
// fn main() { ccp_mcp::main(Meta { name, kind, usage, example }, connect); }
```

`project` handles nested objects, arrays (element-wise), dotted paths and `null` for absent fields.
`plugins/http-ccp` and `plugins/redis-ccp` are complete examples (the latter with a hand-written
protocol client and pipelining).

## D. Bare protocol

For full control depend only on [`ccp-proto`](../crates/ccp-proto): implement `RootResolver`
and call `serve_stdio`. `FieldResult` is a `Scalar` (any pre-built `Value`), a `Node` (object
implementing `Resolvable`), a `List` of nodes, `Missing` (renders `null`) or `Error(msg)`.
`crates/ccp/src/demo.rs` is a complete example.

## Before you open the pull request

* Add the crate under `plugins/`, add it to the workspace `members`, add its README.
* Test it: unit tests for logic, plus an end-to-end test against a local mock or container.
* Add a scenario to [`bench/`](../bench) if it can run locally: a CCP server should be able to
  show it saves tokens, bytes or round trips against the conventional tool for that source.
* Add a row to the README server table and a line to `CHANGELOG.md`.
