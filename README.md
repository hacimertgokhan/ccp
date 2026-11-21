# CCP

A compact, GraphQL-style query protocol for AI tools: read the schema once,
then fetch exactly the fields you need in **one** round trip.

| Crate | What it is |
|---|---|
| `crates/ccp-proto` | Protocol core (parser, executor, compact encoding). No dependencies. |
| `crates/ccp` | Demo CLI (`ccp demo`, `ccp serve`). |
| `plugins/mysql-ccp` | Standalone MySQL/MariaDB server. Add it straight to Claude. |

## mysql-ccp with Claude

```bash
cargo install --path plugins/mysql-ccp      # installs the `mysql-ccp` binary

claude mcp add mysql \
  -e CCP_HOST=127.0.0.1 -e CCP_PORT=3306 \
  -e CCP_USER=readonly -e CCP_DBNAME=shop \
  -e CCP_PASSWORD='...' \
  -- mysql-ccp
```

One server per database: repeat with a different name and env. Claude gets two
tools: `ccp_schema` (call once) and `ccp_query`, e.g.
`users(limit:5,age:30){id name}`. Root fields are tables, arguments filter by
column equality (plus `limit`/`offset`), the braces pick the columns.

Safety: read-only session, SELECT only, identifiers come from
`information_schema`, values are bound parameters, results capped at 1000 rows.
Use a dedicated read-only DB user anyway. No TLS yet: use localhost or a tunnel.

`mysql-ccp --ccp` speaks the raw line protocol (`SCHEMA`, `PING`, queries) for
non-MCP clients.

## Benchmark: CCP vs a conventional SQL MCP server

`python3 bench/bench.py` (needs docker + `cargo build --release`). Real
`mysql-ccp` over MCP vs the usual SQL-MCP shape (list_tables / describe_table /
query tools, JSON rows) on the same MariaDB (100 users, 300 posts, 600
comments). Bytes on the wire incl. JSON-RPC envelopes; tokens ≈ bytes/4.

| Scenario | CCP | Baseline | Saved |
|---|---:|---:|---:|
| Session setup (tool defs + schema) | 1,354 B | 2,776 B | 51% |
| 100 users, id+name (baseline uses `SELECT *`) | 2,608 B | 26,125 B | 90% |
| 100 users, id+name (baseline picks exact columns) | 2,608 B | 3,405 B | 23% |
| 3 lookups: 1 batched call vs 3 calls | 693 B | 1,124 B | 38% |
| 300 posts, id+title (big `body` column) | 14,470 B | 130,095 B | 89% |

The big wins come from field selection (no overfetch) and one schema read; with
a careful model that always names columns, the gap shrinks to ~20-40% (no
repeated quoted keys, no per-call envelope/schema). The baseline is a model of
the common shape, not a specific product.
