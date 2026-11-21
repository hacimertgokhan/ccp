# sqlite-ccp

CCP server for a **SQLite** database file, opened read-only. SQLite is bundled:
nothing else to install.

```bash
cargo install --git https://github.com/hacimertgokhan/ccp sqlite-ccp

claude mcp add notes -e CCP_PATH=/path/to/app.db -- sqlite-ccp
```

| Variable | Required | Notes |
|---|---|---|
| `CCP_PATH` | yes | path to the `.db` file (`CCP_DBNAME` is accepted as an alias) |

Foreign keys are read from `PRAGMA foreign_key_list`, so declare them in your
schema to get nested relations. `sqlite-ccp --ccp` speaks the raw protocol.
See the [protocol](../../docs/PROTOCOL.md).
