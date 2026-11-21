"""Shared helpers for the CCP benchmark suite."""
import json, math, os, subprocess, time, statistics, socket

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
BIN = lambda n: os.path.join(ROOT, "target", "release", n)
J = lambda o: json.dumps(o, separators=(",", ":"), ensure_ascii=False)
tok = lambda b: math.ceil(b / 4)          # token estimate, applied identically to every side
PRICE = 3.0                                # USD per 1M input tokens, illustrative (override with --price)

def sh(*a, inp=None, check=True):
    return subprocess.run(a, input=inp, capture_output=True, text=True, check=check).stdout

def free_port():
    s = socket.socket(); s.bind(("127.0.0.1", 0)); p = s.getsockname()[1]; s.close(); return p

def pct(c, b):
    return f"{100 * (1 - c / b):.0f}%" if b else "n/a"

def human(n):
    return f"{n:,} B" if n < 10_000 else (f"{n/1024:,.1f} KB" if n < 10 * 1024 * 1024 else f"{n/1048576:,.1f} MB")

# ---- JSON-RPC byte accounting for the baseline side --------------------------------
def req_bytes(name, args, i=1):
    return len(J({"jsonrpc": "2.0", "id": i, "method": "tools/call", "params": {"name": name, "arguments": args}}).encode())

def resp_bytes(text, i=1):
    return len(J({"jsonrpc": "2.0", "id": i, "result": {"content": [{"type": "text", "text": text}], "isError": False}}).encode())

def call_bytes(name, args, result_text, i=1):
    """bytes for one baseline tool call: request + response envelope + payload"""
    return req_bytes(name, args, i) + resp_bytes(result_text, i)

# ---- driving a CCP server over MCP exactly as Claude does ----------------------------
class Mcp:
    def __init__(self, binary, env=None, args=()):
        e = dict(os.environ); e.update(env or {})
        # the first run of a fresh binary is slowed by OS security scanning: warm it so cold-start numbers mean *our* start-up
        subprocess.run([BIN(binary), "--version"], capture_output=True)
        t0 = time.perf_counter()
        self.p = subprocess.Popen([BIN(binary), *args], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True, env=e)
        self.n = 0
        self.call("initialize", {"protocolVersion": "2024-11-05"})
        self.tools = self.call("tools/list")           # includes connect + introspection
        self.cold_ms = (time.perf_counter() - t0) * 1000
        self.setup_bytes = 0

    def call(self, method, params=None):
        self.n += 1
        req = J({"jsonrpc": "2.0", "id": self.n, "method": method, "params": params or {}})
        t = time.perf_counter()
        self.p.stdin.write(req + "\n"); self.p.stdin.flush()
        resp = self.p.stdout.readline().strip()
        self.last_ms = (time.perf_counter() - t) * 1000
        self.last_bytes = len(req.encode()) + len(resp.encode())
        return resp

    def query(self, q):
        resp = self.call("tools/call", {"name": "ccp_query", "arguments": {"query": q}})
        d = json.loads(resp)["result"]
        return self.last_bytes, d["content"][0]["text"], self.last_ms

    def schema(self):
        self.call("tools/call", {"name": "ccp_schema", "arguments": {}})
        return self.last_bytes

    def setup_cost(self):
        """tools/list + schema read: what the model pays before its first answer"""
        tl = len(J({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}).encode()) + len(self.tools.encode())
        return tl + self.schema()

    def pid(self): return self.p.pid

    def rss_mb(self):
        out = sh("ps", "-o", "rss=", "-p", str(self.p.pid), check=False).strip()
        return int(out) / 1024 if out else 0.0

    def close(self):
        try: self.p.stdin.close(); self.p.wait(timeout=10)
        except Exception: self.p.kill()

def percentile(xs, p):
    xs = sorted(xs); k = (len(xs) - 1) * p; f = math.floor(k); c = min(f + 1, len(xs) - 1)
    return xs[f] + (xs[c] - xs[f]) * (k - f)

def latency(mcp, query, runs=100, warm=10):
    for _ in range(warm): mcp.query(query)
    ts = [mcp.query(query)[2] for _ in range(runs)]
    return dict(p50=statistics.median(ts), p95=percentile(ts, .95), qps=1000 / statistics.mean(ts))

class Row:
    """One comparable task: CCP vs the conventional approach."""
    def __init__(self, source, title, ccp, base, ccp_calls=1, base_calls=1, naive=None, naive_calls=None, note=""):
        self.source, self.title, self.ccp, self.base = source, title, ccp, base
        self.ccp_calls, self.base_calls, self.naive, self.naive_calls, self.note = ccp_calls, base_calls, naive, naive_calls, note
    def d(self): return dict(self.__dict__)
