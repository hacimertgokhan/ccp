# Changelog

All notable changes are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com) and the project uses [Semantic Versioning](https://semver.org).

## [Unreleased]

## [0.3.0] - 2026-11-21

CCP is no longer only for databases: any data source can be a CCP server.

### Added
- **New servers:** `redis-ccp`, `websocket-ccp`, `socketio-ccp`, `http-ccp` (REST/JSON) and `csv-ccp`.
- **Reporting:** SQL servers (and `csv-ccp`) accept `order:"-a,b"` and `group:"cols"` with `count`, `sum_C`, `avg_C`, `min_C`, `max_C`, `distinct_C`, so reports are computed in the server.
- `ccp-mcp`: generic MCP/CCP runtime (`Service`, `Meta`, `main`) and a JSON field projector, shared by all servers.
- `ccp-stream`: background reader, ring buffer, `messages` / `summary` / `status` roots, reconnection with backoff.
- Protocol: `Float` values, negative integers, string escapes (`\"`, `\\`, `\n`, `\t`), dotted/dashed field names (`address.city`, `content-type`).
- Benchmark suite covering every server: bytes, tokens, round trips, latency, throughput, footprint, cross-database parity (`bench/`).
- Documentation: server conventions, guides for four ways to write a server, per-server READMEs.

### Changed
- The compact encoder writes into a single buffer (no intermediate strings per value).
- `limit`/`offset`/`order`/`group` are declared once in the SQL schema instead of on every table root (smaller schema).
- `ccp-sql`'s MCP front end moved to `ccp-mcp`; `sqlite-ccp` now exposes a library used by `csv-ccp`.


## [0.2.0] - 2026-11-21

### Added
- `postgres-ccp` and `sqlite-ccp` servers next to `mysql-ccp`.
- `ccp-sql`: shared engine (catalog introspection, foreign-key relations, safe SQL generation, MCP front end).
- Nested relations in both directions, loaded in batches (`users{posts{comments{...}}}`).
- `_tables` introspection root with portable `type` and database-specific `native`.
- Reproducible benchmark (`bench/`) with cross-database parity check, token and session estimates.
- Documentation: protocol reference, server-writing guide, contributing and security policies.

### Changed
- Servers are standalone MCP servers configured through `CCP_*` environment variables.
- Workspace split into `ccp-proto`, `ccp-sql`, `ccp` and `plugins/*`.

### Removed
- The connection registry and plugin host in the `ccp` binary (`ccp db`, `ccp plugin`).

## [0.1.0]
- Initial protocol core and demo.
