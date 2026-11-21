# postgres-ccp

CCP server for **PostgreSQL**. Read-only; exposes the connection's current
schema, with primary and foreign keys discovered automatically.

```bash
cargo install --git https://github.com/hacimertgokhan/ccp postgres-ccp

claude mcp add shop \
  -e CCP_HOST=127.0.0.1 -e CCP_PORT=5432 \
  -e CCP_USER=readonly -e CCP_DBNAME=shop \
  -e CCP_PASSWORD="$DB_PASSWORD" \
  -- postgres-ccp
```

| Variable | Required | Default |
|---|---|---|
| `CCP_HOST` | yes | |
| `CCP_USER` | yes | |
| `CCP_DBNAME` | yes | |
| `CCP_PASSWORD` | no | empty |
| `CCP_PORT` | no | `5432` |

Notes: `default_transaction_read_only` is on for the session; values are cast
explicitly (ints as `int8`, everything else as `text`), so columns keep using
their indexes for integer lookups. Connections are plain TCP (TLS planned).
`postgres-ccp --ccp` speaks the raw protocol. See the [protocol](../../docs/PROTOCOL.md).
