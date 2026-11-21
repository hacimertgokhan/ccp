<div align="center">

# CCP — Core Context Protocol

**Give your AI the core context it needs. Nothing more.**

[![CI](https://github.com/hacimertgokhan/ccp/actions/workflows/ci.yml/badge.svg)](https://github.com/hacimertgokhan/ccp/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
![Rust](https://img.shields.io/badge/built%20with-Rust-orange)

[Quick start](#quick-start) · [Servers](#servers) · [How it works](#how-it-works) · [Benchmark](#benchmark) · [Build your own](#build-your-own-server) · [Protocol](docs/PROTOCOL.md) · [Contributing](CONTRIBUTING.md)

</div>

---

Most of what an AI agent reads is not what it needed: whole rows when it wanted two columns, a
hundred calls where one would do, a firehose when it wanted a summary, a 2 MB file when it wanted a
total. Context windows fill up, bills grow, answers slow down.

**CCP is a small protocol built on one idea: a model should receive exactly the *core context* its
question requires, from any data source, in one round trip.** It is not limited to databases: a
cache, a live feed, an API, a CSV file and a report all speak the same compact language.

```text
SQL         users(id:7){name posts{title comments{body}}}
Reports     orders(group:"status",order:"-sum_total"){status count sum_total avg_total}
Redis       hashes(match:"user:*",limit:50){_key name email}
WebSocket   summary(wait:10,field:"sym"){count per_second values{value count}}
CSV         sales(group:"region"){region count sum_price}
REST API    get(path:"/users"){id name address.city}
```

One request each. Only the fields you named. Related data nested. Aggregates computed *in the
server*, so a 13 MB file, a 600-message window or a 144 KB API response costs the model a few hundred bytes.

## Why CCP

| | |
|---|---|
| **Ask for what you need** | Fields are chosen per request; results carry no unused data. |
| **Compute where the data is** | Filters, ordering, grouping and aggregates run in the server; the model gets the report, not the rows. |
| **One call, not N+1** | Relations nest (`users{posts{comments{...}}}`); Redis reads are pipelined; stream windows are summarised. |
| **Read the vocabulary once** | The schema is advertised a single time, not repeated per call. |
| **Compact on the wire** | No quoted keys, no per-call envelope: fewer bytes, fewer tokens. |
| **Light and fast** | Small native binaries (0.5–2 MB), single-digit-millisecond start-up, µs–ms queries, flat memory. |
| **Safe by construction** | Read-only, bounded, caller text is never syntax (bound parameters, fixed command sets, fixed base URLs). |
| **Drop-in for Claude** | Every CCP server is also a standard MCP server: one command to install. |

## Quick start

```bash
# 1. install the servers you need
cargo install --git https://github.com/hacimertgokhan/ccp mysql-ccp      # or any server in the table below

# 2. add one to Claude
claude mcp add shop \
  -e CCP_HOST=127.0.0.1 -e CCP_USER=readonly -e CCP_DBNAME=shop \
  -e CCP_PASSWORD="$DB_PASSWORD" \
  -- mysql-ccp
```

Claude gets two tools, `ccp_schema` (read once) and `ccp_query`. Ask it something about your data and
it sends one compact query. More examples:

```bash
claude mcp add notes   -e CCP_PATH=/path/to/app.db                        -- sqlite-ccp
claude mcp add sales   -e CCP_PATH=/data/sales.csv                        -- csv-ccp
claude mcp add cache   -e CCP_HOST=127.0.0.1                              -- redis-ccp
claude mcp add trades  -e CCP_URL=wss://stream.example.com/trades         -- websocket-ccp
claude mcp add prices  -e CCP_URL=https://realtime.example.com            -- socketio-ccp
claude mcp add github  -e CCP_URL=https://api.github.com -e CCP_BEARER="$GITHUB_TOKEN" -- http-ccp
```

Want to look first? `cargo run -p ccp -- demo` shows a query and its byte cost with no setup at all.

## Servers

All servers live in [`plugins/`](plugins).

| Server | Source | Good for | Config |
|---|---|---|---|
| [`mysql-ccp`](plugins/mysql-ccp) | MySQL, MariaDB | tables, relations, reports | `CCP_HOST` `CCP_USER` `CCP_PASSWORD` `CCP_DBNAME` `CCP_PORT` |
| [`postgres-ccp`](plugins/postgres-ccp) | PostgreSQL | tables, relations, reports | same |
| [`sqlite-ccp`](plugins/sqlite-ccp) | SQLite file | app databases, local data | `CCP_PATH` |
| [`csv-ccp`](plugins/csv-ccp) | CSV / TSV files | **reporting** over files too big to read | `CCP_PATH` |
| [`redis-ccp`](plugins/redis-ccp) | Redis | keys, hashes as table rows, lists, sets, zsets, streams, INFO | `CCP_HOST` `CCP_PASSWORD` … |
| [`websocket-ccp`](plugins/websocket-ccp) | WebSocket feeds | live data: latest messages, windows, summaries | `CCP_URL` `CCP_BEARER` `CCP_SUBSCRIBE` |
| [`socketio-ccp`](plugins/socketio-ccp) | Socket.IO | events by name, windows, summaries | `CCP_URL` `CCP_AUTH` `CCP_EMIT` `CCP_EVENTS` |
| [`http-ccp`](plugins/http-ccp) | any REST / JSON API | trimming bloated responses to the fields you need | `CCP_URL` `CCP_BEARER` `CCP_HEADERS` |

**Something else?** Kafka, MQTT, MongoDB, a log file, an internal service: a server is about 100–150
lines. See [Build your own server](#build-your-own-server).

## How it works

```text
┌────────┐   MCP (stdio)    ┌──────────────┐   native protocol    ┌─────────────┐
│ Claude │ ───────────────▶ │  *-ccp       │ ───────────────────▶ │ data source │
└────────┘  ccp_query(...)  │  server      │ ◀─────────────────── └─────────────┘
                            └──────────────┘
```

1. The server learns the source's shape: a database catalog, a Redis key space, a stream's
   fields, a file's columns.
2. It advertises a small **schema** the model reads once (SQL example):
   ```text
   root users(id:Int,name:Str,city:Str):[Users]
   type Users{id:Int,name:Str,city:Str,posts:[Posts]}
   all table roots also take limit:Int,offset:Int,order:"-f,g",group:"cols"; with group the braces take count,sum_C,...
   ```
3. Each question becomes **one query**. Only requested fields are fetched; nested relations load in
   batches; aggregates run inside the source; stream windows wait server-side.
4. The answer is encoded **compactly** and returned.

```text
users{id name}                               list (limit 100), two fields
users(id:7){name email}                      filter by column
users(city:"Tokyo",order:"-id",limit:5)      filter + sort + page
posts(limit:20){title author{name}}          follow a foreign key
orders(group:"channel,status"){channel status count sum_total}     a report
messages(wait:5,limit:20,event:"trade"){px symbol}                 wait for new events
```

Full grammar, wire format and per-server vocabulary: [docs/PROTOCOL.md](docs/PROTOCOL.md).

## Benchmark

Real CCP servers, driven over MCP exactly as Claude drives them, against the conventional tool for each
source, on the same data: MariaDB 11 / PostgreSQL 17 / SQLite, Redis 7, a live WebSocket and Socket.IO
feed, a 200,000-row CSV, a REST API. Bytes include JSON-RPC envelopes; tokens are estimated as bytes/4
on both sides. Reproduce everything with `python3 bench/run_all.py` (method and baselines in
[bench/README.md](bench/README.md), every table in [bench/RESULTS.md](bench/RESULTS.md)).

![Bytes the model reads per task](docs/benchmark.svg)

| Source / task | CCP | Conventional tool | ≈Tokens (CCP → conventional) | Calls |
|---|---:|---:|---:|---:|
| SQL: a 7-question agent session | 8.8 KB | 11.4 KB careful · 738 KB naive | 2.2k → 2.9k · 189k | 9 vs 15 · 18 |
| Redis: 50 hashes, 2 fields each | 3.6 KB | 13.4 KB | 0.9k → 3.4k | **1 vs 51** |
| WebSocket: report on a 3 s window | 457 B | 165.8 KB | 115 → 42.4k | 1 vs 1 |
| Socket.IO: report on a 3 s window | 472 B | 141.0 KB | 118 → 36.1k | 1 vs 1 |
| CSV: revenue by region, 200k rows | 598 B | 13.1 MB | 150 → 3.4M | 1 vs 1 |
| REST API: 100 users, 2 fields | 2.7 KB | 140.9 KB | 0.7k → 36.1k | 1 vs 1 |

**Token spend.** In an SQL agent session CCP uses **24% fewer tokens than a careful baseline** (exact
columns, JOINs) and **99% fewer than a naive one** (`SELECT *`); at an illustrative $3 per million input
tokens that is $6.56 vs $8.76 vs $566.92 per 1,000 sessions. Reports are where it matters most: "revenue by
status over 5,000 orders" is 587 B against 478 KB of rows. Streams and files are not even comparable:
the raw alternative does not fit a context window.

**Speed.** One pipelined Redis call took 0.6 ms against 8.8 ms for 50 sequential commands on localhost,
and the gap grows with the network: at 10 ms RTT, 21 ms against 519 ms. The reader ingests **~175,000
WebSocket/Socket.IO messages per second** on one core (bound by the test feeder, so a lower bound) at
≈0.2 µs/message of CPU for WebSocket, and queries over a full buffer return in 30 µs to 0.5 ms. Going
through CCP to an HTTP API adds no measurable overhead (0.21 ms vs 0.19 ms bare).

**Footprint.** Release binaries are 0.5–2.2 MB; start-up is 10 ms (4 ms for Redis; PostgreSQL 41 ms
including introspection); idle memory is 2–7 MB per server. `csv-ccp` loads 13 MB / 200k rows in 0.7 s
into 24 MB of RAM and answers a group-by over all rows in ~60 ms.

| Server | Binary | Cold start | RSS | Typical query p50 / p95 |
|---|---:|---:|---:|---:|
| sqlite-ccp | 2.2 MB | 10 ms | 6 MB | 0.73 / 1.64 ms |
| mysql-ccp | 2.1 MB | 18 ms | 4 MB | 0.77 / 0.99 ms |
| postgres-ccp | 1.2 MB | 41 ms | 7 MB | 2.02 / 3.44 ms |
| redis-ccp | 0.5 MB | 10 ms | 2 MB | 0.23 / 0.33 ms |
| websocket-ccp | 1.6 MB | 10 ms | 7 MB | 0.03 / 0.06 ms |
| socketio-ccp | 1.7 MB | 10 ms | 7 MB | 0.03 / 0.03 ms |
| csv-ccp | 2.2 MB | 708 ms (loads the file) | 24 MB | 62.9 / 74.8 ms (group-by over 200k rows) |
| http-ccp | 1.8 MB | 10 ms | 5 MB | 0.21 / 0.25 ms |

**Where CCP does not win**, shown in the full results rather than hidden: a careful model that always
names columns and writes JOINs closes much of the SQL gap (CCP is still 0–29% smaller per task, with
fewer calls); a Redis leaderboard costs *more* with CCP (objects use more bytes than a flat array);
and the Socket.IO server was verified against a protocol-faithful Engine.IO v4 mock, not yet against
a real Socket.IO server. The three SQL servers return byte-identical output for the same 12 queries
(filters, relations, aggregates, ordering).

## Security

* **Read-only**: sessions open read-only; only `SELECT` and read commands are ever generated.
* **Caller text is data, never syntax**: SQL identifiers come from the database catalog and every value is
  a bound parameter; Redis uses a fixed command set with binary-safe arguments; `http-ccp` joins a
  validated path to a fixed base URL and never follows redirects.
* **The model cannot send**: anything a stream server transmits (a subscribe message, an emitted
  event) is configured by *you* through environment variables.
* **Bounded**: rows, relation rows, nesting, scan calls, buffer size, response size and file size are all capped.
* **No secrets in output**: credentials are scrubbed from error messages.
* Use dedicated read-only credentials anyway. Connections are plain TCP for now (TLS for MySQL,
  PostgreSQL and Redis is on the [roadmap](#roadmap); `wss://` and `https://` are supported): use
  localhost, a tunnel or a private network.

Report vulnerabilities privately: see [SECURITY.md](SECURITY.md).

## Build your own server

CCP servers are small because the shared crates do the heavy lifting. Pick the closest starting point:

| Your source | Build on | You write |
|---|---|---|
| A SQL database | `ccp-sql` | a connection, catalog introspection and one `select` (≈150 lines); relations, reports and paging come free |
| A live feed | `ccp-stream` | the reader loop; buffering, `messages`/`summary`/`status`, reconnection come free |
| JSON from anywhere | `ccp-mcp` | the fetch; field projection and the MCP server come free |
| Anything else | `ccp-proto` | a `RootResolver` |

Step-by-step with code: [docs/writing-a-server.md](docs/writing-a-server.md).

## Repository layout

```text
crates/
  ccp-proto/    the protocol: parser, executor, compact encoding (zero dependencies)
  ccp-mcp/      runtime for every server: MCP front end, raw CCP mode, JSON projector
  ccp-sql/      SQL engine: catalog, relations, reports, safe SQL generation
  ccp-stream/   event-stream core: reader, ring buffer, messages / summary / status
  ccp/          demo CLI
plugins/        ready-to-use servers (one crate each)
  mysql-ccp  postgres-ccp  sqlite-ccp  csv-ccp  redis-ccp  websocket-ccp  socketio-ccp  http-ccp
docs/           protocol spec, server-writing guide
bench/          reproducible benchmark suite
```

## Roadmap

- [ ] TLS for MySQL, PostgreSQL and Redis connections
- [ ] Nested arguments (`posts(limit:3)` inside a query) and `having`
- [ ] More servers: Kafka, MQTT, MongoDB, log files, gRPC
- [ ] Prebuilt release binaries and a Homebrew tap
- [ ] Verification of `socketio-ccp` against a real Socket.IO server in CI

Ideas welcome: open an [issue](https://github.com/hacimertgokhan/ccp/issues/new/choose).

## Contributing

Contributions are very welcome, new servers above all. Start with [CONTRIBUTING.md](CONTRIBUTING.md)
and [Build your own server](docs/writing-a-server.md). Good first issues are labelled `good first issue`.

## License

[MIT](LICENSE) © 2026 Hacı Mert Gökhan
