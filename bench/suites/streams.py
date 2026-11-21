"""WebSocket and Socket.IO: a live feed, the raw-stream tool vs CCP summaries/selections;
plus sustained ingest throughput of the background reader."""
import base64, hashlib, json, random, socket, struct, threading, time
from common import *

GUID = b"258EAFA5-E914-47DA-95CA-C5AB0DC85B11"
SYMS = ["BTC", "ETH", "SOL", "XRP", "ADA", "DOGE"]

def frame(text):
    b = text.encode(); n = len(b)
    h = b"\x81" + (bytes([n]) if n < 126 else b"\x7e" + struct.pack(">H", n) if n < 65536 else b"\x7f" + struct.pack(">Q", n))
    return h + b

def read_frame(conn):
    def rn(n):
        d = b""
        while len(d) < n:
            c = conn.recv(n - len(d))
            if not c: raise EOFError
            d += c
        return d
    b1, b2 = rn(2); n = b2 & 0x7F
    if n == 126: n = struct.unpack(">H", rn(2))[0]
    elif n == 127: n = struct.unpack(">Q", rn(8))[0]
    mask = rn(4) if b2 & 0x80 else None
    data = rn(n)
    if mask: data = bytes(c ^ mask[i % 4] for i, c in enumerate(data))
    return b1 & 0x0F, data.decode(errors="replace")

class Feeder:
    """A WebSocket (mode=ws) or Engine.IO v4 / Socket.IO (mode=sio) server pushing trades."""
    def __init__(self, mode, rate=200):
        self.mode, self.rate, self.sent, self.burst = mode, rate, 0, 0
        self.sock = socket.socket(); self.sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        self.sock.bind(("127.0.0.1", 0)); self.sock.listen(4); self.port = self.sock.getsockname()[1]
        self.stop = False; self.px = {s: 1000.0 * (i + 1) for i, s in enumerate(SYMS)}
        threading.Thread(target=self.accept, daemon=True).start()

    def msg(self, n):
        s = random.choice(SYMS); self.px[s] *= 1 + random.uniform(-0.0005, 0.0005)
        body = {"e": "trade", "sym": s, "px": round(self.px[s], 2), "qty": round(random.uniform(0.001, 2), 3), "id": n, "ts": 1700000000000 + n,
                "maker": random.random() < .5, "venue": "xch-1", "meta": {"seq": n, "shard": n % 8, "tags": ["spot", "live"]},
                "book": {"bids": [[round(self.px[s] - 1, 2), 1.2], [round(self.px[s] - 2, 2), 3.4]], "asks": [[round(self.px[s] + 1, 2), 0.9], [round(self.px[s] + 2, 2), 2.2]]}}
        if self.mode == "sio":
            ev = "trade" if random.random() < .8 else "heartbeat"
            return "42" + json.dumps([ev, body if ev == "trade" else {"alive": True, "n": n}], separators=(",", ":"))
        return json.dumps(body, separators=(",", ":"))

    def accept(self):
        while not self.stop:
            try: c, _ = self.sock.accept()
            except OSError: return
            threading.Thread(target=self.serve, args=(c,), daemon=True).start()

    def serve(self, c):
        try:
            data = b""
            while b"\r\n\r\n" not in data: data += c.recv(4096)
            key = [l.split(b": ")[1] for l in data.split(b"\r\n") if l.lower().startswith(b"sec-websocket-key")][0]
            acc = base64.b64encode(hashlib.sha1(key + GUID).digest())
            c.sendall(b"HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: " + acc + b"\r\n\r\n")
            if self.mode == "sio":
                c.sendall(frame('0{"sid":"bench","upgrades":[],"pingInterval":25000,"pingTimeout":20000,"maxPayload":1000000}'))
                _, p = read_frame(c); assert p.startswith("40")
                c.sendall(frame('40{"sid":"bench-ns"}'))
            else:
                c.settimeout(0.2)
                try: read_frame(c)          # an optional subscribe message
                except Exception: pass
                c.settimeout(None)
            n = 0; period = 0.05
            while not self.stop:
                if self.burst:
                    total, self.burst = self.burst, 0
                    for i in range(0, total, 500):
                        batch = b"".join(frame(self.msg(n + j)) for j in range(min(500, total - i))); n += min(500, total - i)
                        c.sendall(batch)
                    self.sent += total; continue
                k = max(1, int(self.rate * period))
                c.sendall(b"".join(frame(self.msg(n + j)) for j in range(k))); n += k; self.sent += k
                time.sleep(period)
        except Exception:
            pass
        finally:
            c.close()

    def close(self):
        self.stop = True; self.sock.close()

def cpu_s(pid):
    out = sh("ps", "-o", "cputime=", "-p", str(pid), check=False).strip()
    if not out: return 0.0
    parts = out.replace("-", ":").split(":"); parts = [float(x) for x in parts]
    s = 0.0
    for x in parts: s = s * 60 + x
    return s

def one(mode, binary, window, rate):
    f = Feeder(mode, rate)
    env = {"CCP_URL": f"ws://127.0.0.1:{f.port}/feed"} if mode == "ws" else {"CCP_URL": f"http://127.0.0.1:{f.port}"}
    kind = "WebSocket" if mode == "ws" else "Socket.IO"
    m = Mcp(binary, env)
    time.sleep(1.0)  # let the buffer fill
    rows = []
    # raw messages as the baseline tool would return them (identical bytes to what the feed sent)
    probe = Feeder(mode, rate); probe.px = dict(f.px)
    raw = lambda n: [probe.msg(i) for i in range(n)]
    msg_of = lambda x: json.loads(x[2:]) if mode == "sio" else json.loads(x)   # what a tool hands the model
    # 1. report on a window
    ev = ',event:"trade"' if mode == "sio" else ""
    cb, out, ms = m.query(f'summary(wait:{window},field:"sym"{ev}){{count per_second values{{value count}}}}')
    n_window = window * rate          # the raw tool returns every event, trades and heartbeats alike
    base = call_bytes("receive", {"seconds": window}, J([msg_of(x) for x in raw(n_window)]))
    rows.append(Row(kind, f"Report a {window}s window ({n_window} events): count + per-symbol", cb, base, 1, 1,
                    note=f"~{n_window} msgs/{window}s"))
    # 2. price statistics over the window
    cb2, out2, _ = m.query(f'summary(wait:{window},field:"px"{ev}){{count min max avg}}')
    rows.append(Row(kind, f"Price min/max/avg over {window}s", cb2, base, 1, 1))
    # 3. the last 5 trades, two fields
    cb3, out3, _ = m.query(f'messages(limit:5{ev}){{sym px}}')
    five = raw(5)
    b5 = call_bytes("receive", {"count": 5}, J([msg_of(x) for x in five]))
    rows.append(Row(kind, "Last 5 trades, symbol + price", cb3, b5, 1, 1))
    # 4. poll for what is new since a cursor
    lat = dict(msgs=latency(m, 'messages(limit:20){sym px}', 100), summ=latency(m, 'summary(field:"px"){count min max avg}', 100))
    foot = dict(cold=m.cold_ms, rss=m.rss_mb(), lat=lat["msgs"])
    # 5. sustained ingest throughput (burst as fast as the feeder can write)
    st0 = m.query("status{total bytes}")[1]
    total0 = int(st0.split("total:")[1].split(",")[0]); b0 = cpu_s(m.pid())
    N = 100_000; t0 = time.perf_counter(); f.burst = N
    while True:
        cur = int(m.query("status{total}")[1].split("total:")[1].rstrip("}"))
        if cur - total0 >= N or time.perf_counter() - t0 > 60: break
        time.sleep(0.05)
    dt = time.perf_counter() - t0
    ingest = dict(msgs=cur - total0, secs=dt, per_s=(cur - total0) / dt, cpu_s=cpu_s(m.pid()) - b0, rss=m.rss_mb())
    bytes_each = len(f.msg(1)) + 6
    ingest["mb_s"] = ingest["per_s"] * bytes_each / 1e6
    m.close(); f.close(); probe.close()
    return dict(kind=kind, rows=rows, lat=lat, foot=foot, ingest=ingest, binary=binary)

def run(args):
    window = 3
    return [one("ws", "websocket-ccp", window, 200), one("sio", "socketio-ccp", window, 200)]

def render(rs, price):
    L = []; P = L.append
    P("## 3. Event streams (WebSocket, Socket.IO)\n")
    P("A local feed pushes ~330-byte JSON trades at 200 msg/s (Socket.IO: 80% `trade`, 20% `heartbeat`, Engine.IO v4 framing). The baseline is the usual stream tool: `receive(seconds)` returns every raw message to the model.\n")
    P("| Task | CCP | Raw-stream tool | Saved |\n|---|---:|---:|---:|")
    for r in rs:
        for x in r["rows"]:
            P(f"| {r['kind']}: {x.title} | {x.ccp:,} B | {x.base:,} B ({human(x.base)}) | {pct(x.ccp, x.base)} |")
    P("\n### Reader throughput and query latency\n")
    P("| Server | Ingest (burst of 100k) | Throughput | Reader CPU | RSS after | `messages(limit:20)` p50 / p95 | `summary(field)` on full buffer p50 / p95 |\n|---|---:|---:|---:|---:|---:|---:|")
    for r in rs:
        i = r["ingest"]; a = r["lat"]["msgs"]; b = r["lat"]["summ"]
        P(f"| {r['binary']} | {i['msgs']:,} msgs in {i['secs']:.2f}s | {i['per_s']:,.0f} msg/s ({i['mb_s']:.0f} MB/s) | {i['cpu_s']:.2f}s | {i['rss']:.0f} MB | {a['p50']:.2f} / {a['p95']:.2f} ms | {b['p50']:.2f} / {b['p95']:.2f} ms |")
    P("\nThe buffer keeps the newest 5,000 messages (32 MB cap), so memory stays flat however long the stream runs; JSON is parsed lazily, only for messages a query actually touches.\n")
    return "\n".join(L)
