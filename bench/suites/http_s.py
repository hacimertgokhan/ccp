"""REST/JSON APIs: full bodies vs projected fields."""
import json, random, threading, time, urllib.request
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from common import *

def user(i):
    random.seed(i)
    return {"id": i, "username": f"user{i}", "name": f"User {i}", "email": f"user{i}@example.com", "phone": "+90 555 000 %04d" % i,
            "website": f"https://user{i}.example.com", "avatar": f"https://cdn.example.com/avatars/{i}.png?size=256&v=3",
            "address": {"street": f"{i} Long Street", "suite": "Apt. 12", "city": random.choice(["Izmir", "Berlin", "Tokyo"]), "zipcode": "35000",
                        "geo": {"lat": "38.4237", "lng": "27.1428"}},
            "company": {"name": f"Company {i}", "catchPhrase": "Multi-layered client-server neural-net", "bs": "harness real-time e-markets"},
            "bio": "lorem ipsum dolor sit amet " * 12,
            "settings": {"theme": "dark", "lang": "en", "notifications": {"email": True, "push": False, "sms": False}, "beta": [f"f{k}" for k in range(8)]},
            "stats": {"posts": i * 3, "followers": i * 7, "following": i * 2, "likes": i * 11},
            "links": [{"rel": "self", "href": f"/users/{i}"}, {"rel": "posts", "href": f"/users/{i}/posts"}, {"rel": "followers", "href": f"/users/{i}/followers"}],
            "created_at": "2024-01-02T03:04:05Z", "updated_at": "2026-05-06T07:08:09Z"}

class H(BaseHTTPRequestHandler):
    def log_message(self, *a): pass
    def do_GET(self):
        if self.path.startswith("/users/") and self.path[7:].isdigit(): body = user(int(self.path[7:]))
        elif self.path.startswith("/users"): body = [user(i) for i in range(1, 101)]
        else: self.send_response(404); self.end_headers(); return
        b = json.dumps(body).encode()
        self.send_response(200); self.send_header("Content-Type", "application/json"); self.send_header("Content-Length", str(len(b))); self.end_headers(); self.wfile.write(b)

def run(args):
    srv = ThreadingHTTPServer(("127.0.0.1", 0), H); port = srv.server_address[1]
    threading.Thread(target=srv.serve_forever, daemon=True).start()
    base = f"http://127.0.0.1:{port}"
    m = Mcp("http-ccp", {"CCP_URL": base})
    fetch = lambda p: urllib.request.urlopen(base + p).read().decode()
    rows = []
    def add(title, q, path):
        cb, out, ms = m.query(q); body = fetch(path)
        rows.append(Row("HTTP", title, cb, call_bytes("fetch", {"url": base + path}, body), 1, 1))
    add("One user: name, email, city", 'get(path:"/users/7"){name email address.city}', "/users/7")
    add("One user: just the follower count", 'get(path:"/users/7"){stats.followers}', "/users/7")
    add("List of 100 users: id + name", 'get(path:"/users"){id name}', "/users")
    add("List of 100 users: name + company + city", 'get(path:"/users"){name company.name address.city}', "/users")
    # overhead: CCP call vs a direct request
    lat = latency(m, 'get(path:"/users/7"){name email address.city}', 100)
    ts = []
    for _ in range(100):
        t = time.perf_counter(); fetch("/users/7"); ts.append((time.perf_counter() - t) * 1000)
    direct = dict(p50=sorted(ts)[50], p95=sorted(ts)[94])
    foot = dict(cold=m.cold_ms, rss=m.rss_mb(), lat=lat)
    m.close(); srv.shutdown()
    return dict(rows=rows, lat=lat, direct=direct, foot=foot)

def render(r, price):
    L = []; P = L.append
    P("## 5. REST / JSON APIs\n")
    P("A local API returns realistic bloated user objects (~2.5 KB each, nested). Baseline = a `fetch(url)` tool that returns the response body.\n")
    P("| Task | CCP | Full response | Saved |\n|---|---:|---:|---:|")
    for x in r["rows"]:
        P(f"| {x.title} | {x.ccp:,} B | {x.base:,} B | {pct(x.ccp, x.base)} |")
    P(f"\nOverhead of going through CCP: single-object call **{r['lat']['p50']:.2f} ms** p50 (incl. MCP round trip) vs **{r['direct']['p50']:.2f} ms** for a bare HTTP GET from Python on the same machine.\n")
    return "\n".join(L)
