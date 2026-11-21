# redis-ccp

CCP server for **Redis**. Read-only, dependency-free (a small RESP client with pipelining),
about half a megabyte.

```bash
cargo install --git https://github.com/hacimertgokhan/ccp redis-ccp

claude mcp add cache -e CCP_HOST=127.0.0.1 -e CCP_PASSWORD="$REDIS_PASSWORD" -- redis-ccp
```

| Variable | Required | Default |
|---|---|---|
| `CCP_HOST` | no | `127.0.0.1` |
| `CCP_PORT` | no | `6379` |
| `CCP_PASSWORD` | no | none |
| `CCP_USER` | no | none (ACL user, used with the password) |
| `CCP_DB` | no | `0` |

| Query | Does |
|---|---|
| `keys(match:"user:*",type:"hash",limit:50){key type ttl size}` | scan keys; only the requested extras are fetched, pipelined |
| `hashes(match:"user:*",limit:20){_key name email}` | many hashes as table rows: 1 scan + 1 pipelined `HMGET` batch instead of N+1 calls |
| `hash(key:"user:1"){name city}` | chosen fields of one hash |
| `get(key:"k")`, `mget(keys:"a,b"){key value}` | strings |
| `list(key:"q",start:0,stop:9)`, `members(key:"tags")` | list / set contents (bounded) |
| `zrange(key:"board",start:0,stop:9,reverse:true){member score}` | sorted sets |
| `stream(key:"events",limit:10){id type user}` | latest stream entries |
| `info(section:"memory"){used_memory}`, `dbsize` | server stats |

Safety: only this fixed set of read commands is ever sent; keys, patterns and arguments are
binary-safe bulk strings, so caller text can never become a command. Scans are bounded
(1,000 results per root, 2,000 scan calls). Plain TCP for now (TLS planned).
