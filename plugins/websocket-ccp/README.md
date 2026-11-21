# websocket-ccp

CCP server for **WebSocket feeds**. It listens in the background and keeps the newest messages
in a ring buffer, so the model never receives the firehose: it asks for the fields, windows and
summaries it needs.

```bash
cargo install --git https://github.com/hacimertgokhan/ccp websocket-ccp

claude mcp add trades \
  -e CCP_URL=wss://stream.example.com/trades \
  -e CCP_SUBSCRIBE='{"op":"subscribe","channel":"btc"}' \
  -- websocket-ccp
```

| Variable | Required | Notes |
|---|---|---|
| `CCP_URL` | yes | `ws://` or `wss://` |
| `CCP_BEARER` | no | sent as `Authorization: Bearer …` |
| `CCP_SUBSCRIBE` | no | message(s) sent after connecting, one per line. Set by *you*; the model cannot send anything |
| `CCP_BUFFER`, `CCP_BUFFER_MB` | no | messages / megabytes kept (5000 / 32) |

```text
messages(limit:10){_seq sym px}                       the latest 10, two fields each
messages(wait:5,limit:20,contains:"BTC"){sym px}      wait up to 5 s for 20 new matching messages
messages(since:1200,limit:50){_seq px}                everything after sequence 1200 (incremental polling)
summary(wait:10,field:"sym"){count per_second values{value count}}   report instead of listing
summary(field:"px"){count min max avg last}           numeric stats over the buffer
status{connected reconnects buffered total}
```

The connection is kept alive (pings answered, idle links probed) and re-established with backoff;
a bad URL or credentials fail fast with a readable error. JSON is parsed lazily, only for messages a
query touches, so a busy feed costs little CPU (≈0.2 µs per message to ingest). Non-JSON frames are
available as `text`. Everything the server can reach is read-only.
