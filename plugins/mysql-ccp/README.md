# mysql-ccp

CCP server for **MySQL** and **MariaDB**. Read-only; schema, primary keys and
foreign keys are discovered automatically.

```bash
cargo install --git https://github.com/hacimertgokhan/ccp mysql-ccp

claude mcp add shop \
  -e CCP_HOST=127.0.0.1 -e CCP_PORT=3306 \
  -e CCP_USER=readonly -e CCP_DBNAME=shop \
  -e CCP_PASSWORD="$DB_PASSWORD" \
  -- mysql-ccp
```

| Variable | Required | Default |
|---|---|---|
| `CCP_HOST` | yes | |
| `CCP_USER` | yes | |
| `CCP_DBNAME` | yes | |
| `CCP_PASSWORD` | no | empty |
| `CCP_PORT` | no | `3306` |

Notes: the session is `READ ONLY`; connections are plain TCP (TLS planned), so use
localhost or a tunnel; create a dedicated read-only user.
`mysql-ccp --ccp` speaks the raw protocol instead of MCP. See the
[protocol](../../docs/PROTOCOL.md).
