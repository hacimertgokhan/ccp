# CCP benchmark suite

```bash
cargo build --release --workspace
python3 bench/run_all.py                 # everything (≈ 6 minutes)
python3 bench/run_all.py --only sql,redis --price 3.0
```

Needs Docker (`mariadb:11`, `postgres:17-alpine`, `redis:7-alpine`) and Python 3 (standard library only).
Writes [`RESULTS.md`](RESULTS.md), `results.json` and `docs/benchmark.svg`.

## What is measured

| Suite | Source | Measures |
|---|---|---|
| `sql` | MySQL/MariaDB, PostgreSQL, SQLite | per-task bytes, calls, reports (group/aggregate), scaling, a whole agent session, **cross-database parity** |
| `redis` | Redis | N+1 round trips vs one pipelined call, field selection, speed under modelled network RTT |
| `streams` | WebSocket, Socket.IO | a raw-stream tool vs summaries/selections, **sustained ingest throughput**, reader CPU, query latency on a full buffer |
| `csv` | a 200,000-row, 13 MB CSV | reports vs "read the file", load time, aggregate latency |
| `http` | a local REST API | full bodies vs projected fields, overhead of the CCP hop |
| footprint | every server | binary size, cold start, RSS, p50/p95 latency, sequential throughput |

All numbers come from the **real servers over MCP** (JSON-RPC envelopes included) against real
data: containers for the databases, local servers for the feeds and the API.

## Baselines

A baseline is a *model of the conventional approach* for each source, executed against the same data.
It is not a specific product, and where there are two plausible behaviours both are shown:

* **SQL** — `list_tables` / `describe_table` / `query` tools returning compact JSON rows.
  *careful*: exact columns and a JOIN for related data. *naive*: `SELECT *` and one query per table.
* **Redis** — one MCP call per command (`scan`, `type`, `ttl`, `hgetall`/`hmget`, `info`, `zrange`).
  *typical* returns whole hashes; *field-aware* uses `HMGET`.
* **Streams** — a `receive(seconds)` tool that returns every raw message to the model.
* **CSV** — a `read_file` tool.
* **HTTP** — a `fetch(url)` tool that returns the response body.

## Method and honesty notes

* **Tokens** are estimated as `ceil(bytes / 4)` for **both** sides: no tokenizer is needed and the
  same estimator keeps the comparison fair. Real tokenizers differ by content; expect the ratio, not
  the absolute number, to carry over. Cost rows use an illustrative $3 per million input tokens.
* The baselines use *compact* JSON. Real servers often pretty-print, which would only widen the gap.
* Where CCP loses it is shown (e.g. a Redis leaderboard: objects cost more bytes than a flat array).
* "Network RTT" tables are a **model** (calls × RTT added to measured local time), labelled as such.
* Latency/throughput are measured on the machine running the suite (localhost, one process per server);
  treat them as relative, not as production SLAs. Throughput of the stream reader is limited by the
  Python feeder, so it is a lower bound.
* A fresh binary's first run is slowed by OS security scanning; the suite warms each binary once so
  *cold start* measures our start-up.
* The Socket.IO server is exercised against a protocol-faithful Engine.IO v4 mock, not a real
  Socket.IO server.
