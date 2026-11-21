# socketio-ccp

CCP server for **Socket.IO** (Engine.IO v4 over WebSocket). It connects as a read-only listener,
answers pings, and buffers events so the model can query them by event name, field and window,
the same way as [`websocket-ccp`](../websocket-ccp).

```bash
cargo install --git https://github.com/hacimertgokhan/ccp socketio-ccp

claude mcp add prices \
  -e CCP_URL=https://realtime.example.com \
  -e CCP_AUTH='{"token":"..."}' \
  -e CCP_EMIT='[["subscribe","btc"]]' \
  -e CCP_EVENTS=trade,ticker \
  -- socketio-ccp
```

| Variable | Required | Notes |
|---|---|---|
| `CCP_URL` | yes | `http(s)://` or `ws(s)://` base URL |
| `CCP_PATH` | no | Socket.IO path (default `/socket.io/`) |
| `CCP_NAMESPACE` | no | default `/` |
| `CCP_AUTH` | no | JSON object sent as the connect `auth` payload |
| `CCP_BEARER` | no | sent as `Authorization: Bearer …` |
| `CCP_EMIT` | no | events sent after connecting, e.g. `[["subscribe","room1"]]`. Set by *you*; the model cannot emit |
| `CCP_EVENTS` | no | comma-separated event names to keep (default: all) |
| `CCP_BUFFER`, `CCP_BUFFER_MB` | no | messages / megabytes kept (5000 / 32) |

```text
messages(event:"trade",limit:10){_seq price symbol}
summary(wait:10,event:"trade",field:"symbol"){count per_second values{value count}}
summary(event:"trade",field:"price"){min max avg last}
status{connected reconnects}
```

`_event` is the Socket.IO event name; the payload's fields are the message fields (a single-argument
event's argument, or an array for multi-argument events). Binary attachments are ignored.
Events are recognised with a single-pass scanner (no JSON tree per event), so ingest stays cheap.
