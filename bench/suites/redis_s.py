"""Redis: N+1 round trips vs one pipelined call; field selection on hashes; INFO."""
import random, subprocess, time, socket
from common import *

NAME, PORT = "ccp-bench-redis", 63791

def resp_cmd(*a):
    b = b"*%d\r\n" % len(a)
    for x in a:
        x = x if isinstance(x, bytes) else str(x).encode(); b += b"$%d\r\n%s\r\n" % (len(x), x)
    return b

class Raw:
    """A plain sequential RESP client: how a conventional per-command Redis tool talks."""
    def __init__(self):
        self.s = socket.create_connection(("127.0.0.1", PORT)); self.f = self.s.makefile("rb")
    def read(self):
        l = self.f.readline(); t, body = l[:1], l[1:-2]
        if t in b"+-": return body.decode()
        if t == b":": return int(body)
        if t == b"$":
            n = int(body)
            return None if n < 0 else self.f.read(n + 2)[:-2].decode()
        if t == b"*":
            n = int(body); return None if n < 0 else [self.read() for _ in range(n)]
    def cmd(self, *a):
        self.s.sendall(resp_cmd(*a)); return self.read()

def seed():
    random.seed(11)
    cities = ["Istanbul", "Ankara", "Izmir", "Berlin", "London", "Tokyo"]
    r = Raw(); pipe = b""
    for i in range(1, 2001):
        pipe += resp_cmd("HSET", f"user:{i}", "name", f"user{i}", "email", f"user{i}@example.com", "city", random.choice(cities),
                         "bio", " ".join(random.choice(["alpha", "beta", "gamma", "delta", "omega"]) for _ in range(30)),
                         "plan", random.choice(["free", "pro", "team"]), "created", 1700000000 + i, "visits", random.randint(0, 999), "flags", "x" * 40)
    for i in range(1, 501): pipe += resp_cmd("SET", f"session:{i}", f"token-{i}-" + "z" * 40) + resp_cmd("EXPIRE", f"session:{i}", 3600)
    for i in range(1, 301): pipe += resp_cmd("ZADD", "leaderboard", random.randint(1, 100000), f"player{i}")
    r.s.sendall(pipe)
    for _ in range(2000 + 1000 + 300): r.read()
    return r

def run(args):
    subprocess.run(["docker", "rm", "-f", NAME], capture_output=True)
    sh("docker", "run", "-d", "--name", NAME, "-p", f"127.0.0.1:{PORT}:6379", "redis:7-alpine")
    try:
        for _ in range(40):
            try: raw = seed(); break
            except Exception: time.sleep(0.5)
        m = Mcp("redis-ccp", {"CCP_HOST": "127.0.0.1", "CCP_PORT": str(PORT)})
        rows = []
        # 1. list 50 users, 2 fields
        cb, out, ms = m.query('hashes(match:"user:*",limit:50){_key name email}')
        keys = raw.cmd("SCAN", 0, "MATCH", "user:*", "COUNT", 1000)[1][:50]
        scan_b = call_bytes("scan", {"pattern": "user:*", "count": 50}, J(keys))
        hg = sum(call_bytes("hgetall", {"key": k}, J(dict(zip(*[iter(raw.cmd("HGETALL", k))] * 2))), i) for i, k in enumerate(keys))
        hm = sum(call_bytes("hmget", {"key": k, "fields": ["name", "email"]}, J(raw.cmd("HMGET", k, "name", "email")), i) for i, k in enumerate(keys))
        rows.append(Row("Redis", "50 user hashes, name+email", cb, scan_b + hm, 1, 51, scan_b + hg, 51))
        # 2. one key
        cb, _, _ = m.query('hash(key:"user:77"){name city}')
        full = J(dict(zip(*[iter(raw.cmd("HGETALL", "user:77"))] * 2)))
        rows.append(Row("Redis", "One hash, 2 fields", cb, call_bytes("hmget", {"key": "user:77", "fields": ["name", "city"]}, J(raw.cmd("HMGET", "user:77", "name", "city"))), 1, 1,
                        call_bytes("hgetall", {"key": "user:77"}, full), 1))
        # 3. keys with type + ttl
        cb, _, _ = m.query('keys(match:"session:1*",limit:100){key type ttl}')
        ks = raw.cmd("SCAN", 0, "MATCH", "session:1*", "COUNT", 5000)[1][:100]
        scan_b = call_bytes("scan", {"pattern": "session:1*"}, J(ks))
        per = sum(call_bytes("type", {"key": k}, J(raw.cmd("TYPE", k)), i) + call_bytes("ttl", {"key": k}, J(raw.cmd("TTL", k)), i) for i, k in enumerate(ks))
        rows.append(Row("Redis", f"{len(ks)} keys with type + TTL", cb, scan_b + per, 1, 1 + 2 * len(ks)))
        # 4. INFO memory
        cb, _, _ = m.query('info(section:"memory"){used_memory used_memory_peak maxmemory}')
        info_txt = raw.cmd("INFO", "memory")
        rows.append(Row("Redis", "Memory stats (3 numbers)", cb, call_bytes("info", {"section": "memory"}, J(info_txt)), 1, 1))
        # 5. leaderboard
        cb, _, _ = m.query("zrange(key:\"leaderboard\",start:0,stop:9,reverse:true){member score}")
        z = raw.cmd("ZRANGE", "leaderboard", 0, 9, "REV", "WITHSCORES")
        rows.append(Row("Redis", "Top-10 leaderboard", cb, call_bytes("zrange", {"key": "leaderboard", "start": 0, "stop": 9, "rev": True, "withscores": True}, J(z)), 1, 1))
        setup = m.setup_cost()
        lat = dict(
            hashes50=latency(m, 'hashes(match:"user:*",limit:50){_key name email}', 100),
            hash=latency(m, 'hash(key:"user:77"){name city}', 200),
        )
        # network model: one pipelined call vs N+1 sequential round trips
        t0 = time.perf_counter()
        for k in keys: raw.cmd("HMGET", k, "name", "email")
        seq_ms = (time.perf_counter() - t0) * 1000
        foot = dict(cold=m.cold_ms, rss=m.rss_mb())
        m.close()
        return dict(rows=rows, lat=lat, seq_ms=seq_ms, foot=foot, setup=setup)
    finally:
        subprocess.run(["docker", "rm", "-f", NAME], capture_output=True)

def render(r, price):
    L = []; P = L.append
    P("## 2. Redis\n")
    P("Dataset: 2,000 user hashes (8 fields), 500 session strings with TTL, a 300-member leaderboard. Baseline = a conventional per-command Redis tool set (`scan`, `type`, `ttl`, `hgetall`/`hmget`, `info`, `zrange`), one MCP call per command.\n")
    P("| Task | CCP | Baseline (typical) | Baseline (field-aware) | Calls (CCP / baseline) | Saved |\n|---|---:|---:|---:|---:|---:|")
    for x in r["rows"]:
        typ, tc = (x.naive, x.naive_calls) if x.naive is not None else (x.base, x.base_calls)
        fa = f"{x.base:,} B" if x.naive is not None else "same"
        P(f"| {x.title} | {x.ccp:,} B | {typ:,} B | {fa} | {x.ccp_calls} / {tc} | {pct(x.ccp, typ)} |")
    P("\n*Typical* = `HGETALL`-style tools returning whole values; *field-aware* = the baseline picks fields with `HMGET`.\n")
    P("### Speed: one pipelined call vs N+1 round trips\n")
    seq = r["seq_ms"]; one = r["lat"]["hashes50"]["p50"]
    P(f"Fetching 50 hashes' fields: CCP **{one:.2f} ms** (1 MCP call; to Redis it is 2 round trips: one `SCAN` plus one pipelined batch) vs **{seq:.2f} ms** for 50 sequential `HMGET` on localhost alone. "
      f"Each extra round trip also costs the *network* RTT, so against a remote Redis (modelled, not measured):\n")
    P("| Network RTT | CCP (2 RTT + server) | 51 sequential commands |\n|---:|---:|---:|")
    for rtt in (0.5, 2, 10, 50):
        P(f"| {rtt} ms | {one + 2 * rtt:.1f} ms | {seq + 51 * rtt:.1f} ms |")
    P("")
    return "\n".join(L)
