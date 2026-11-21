#!/usr/bin/env python3
"""CCP benchmark suite: bytes, tokens, round trips, latency, throughput, footprint.

  python3 bench/run_all.py                 # everything
  python3 bench/run_all.py --only sql,redis
  python3 bench/run_all.py --price 3.0     # USD per 1M input tokens used for cost rows

Needs: docker (mariadb:11, postgres:17-alpine, redis:7-alpine) and `cargo build --release`.
See bench/README.md for the method and the baseline models.
"""
import argparse, json, math, os, sys
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from common import *
from suites import sql, redis_s, streams, csv_s, http_s

SUITES = {"sql": sql, "redis": redis_s, "streams": streams, "csv": csv_s, "http": http_s}
CHART_PICK = ["List 100 users", "Nested: user", "Three independent", "Report: revenue by status", "50 user hashes", "Memory stats",
              "WebSocket: Report", "Socket.IO: Report", "Revenue + orders", "List of 100 users: id", "One user: name"]

def chart(rows, path):
    W, bar, gap, left = 900, 15, 26, 330
    scale = W - left - 140
    h = 76 + len(rows) * (bar * 3 + gap)
    c = {"ccp": "#2563eb", "base": "#9ca3af", "naive": "#d1d5db"}
    o = [f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {W} {h}" font-family="-apple-system,Segoe UI,Helvetica,Arial,sans-serif" font-size="12.5">',
         f'<rect width="{W}" height="{h}" fill="#ffffff"/>',
         '<text x="20" y="28" font-size="17" font-weight="600" fill="#111827">Bytes the model has to read per task (log scale, lower is better)</text>']
    lx = 20
    for lbl, col in (("CCP", c["ccp"]), ("conventional tool, careful", c["base"]), ("conventional tool, naive / raw", c["naive"])):
        o.append(f'<rect x="{lx}" y="42" width="12" height="12" rx="2" fill="{col}"/><text x="{lx+18}" y="52" fill="#374151">{lbl}</text>'); lx += 230 if lbl.startswith("conv") else 90
    mx = max(max(r.ccp, r.base, r.naive or 0) for r in rows)
    f = lambda v: scale * math.log10(max(v, 10)) / math.log10(mx)
    y = 72
    for r in rows:
        label = r.title if len(r.title) <= 50 else r.title.split(" (")[0]
        o.append(f'<text x="20" y="{y+bar+3}" fill="#111827">{label[:50]}</text>')
        o.append(f'<text x="20" y="{y+bar+17}" fill="#6b7280" font-size="11">{r.source}</text>')
        for i, (v, col) in enumerate(((r.ccp, c["ccp"]), (r.base, c["base"]), (r.naive, c["naive"]))):
            if v is None: continue
            w = f(v); yy = y + i * (bar + 2)
            o.append(f'<rect x="{left}" y="{yy}" width="{w:.1f}" height="{bar}" rx="2" fill="{col}"/>')
            o.append(f'<text x="{left+w+6:.1f}" y="{yy+bar-3}" fill="#374151">{human(v)}</text>')
        y += bar * 3 + gap
    o.append("</svg>")
    open(path, "w").write("\n".join(o))

def footprint(res):
    rows = []
    def add(name, binary, f):
        size = os.path.getsize(BIN(binary)) / 1048576 if os.path.exists(BIN(binary)) else 0
        l = f["lat"]; rows.append((name, size, f["cold"], f["rss"], l["p50"], l["p95"], l["qps"]))
    if "sql" in res:
        for k in res["sql"]["kinds"]: add(f"{k}-ccp", f"{k}-ccp", res["sql"]["foot"][k])
    if "redis" in res: add("redis-ccp", "redis-ccp", dict(**res["redis"]["foot"], lat=res["redis"]["lat"]["hash"]))
    for r in res.get("streams", []): add(r["binary"], r["binary"], r["foot"])
    if "csv" in res: add("csv-ccp", "csv-ccp", res["csv"]["foot"])
    if "http" in res: add("http-ccp", "http-ccp", res["http"]["foot"])
    L = ["## 6. Footprint and speed\n",
         "Release builds (LTO, stripped). *Cold start* = spawn → `initialize` + `tools/list` answered, which includes connecting and (for SQL) reading the catalog, "
         "(for CSV) loading and type-inferring 200k rows, (for streams) the first handshake. Latency is a typical query measured through the full MCP path (JSON-RPC in/out), 100 runs.\n",
         "| Server | Binary | Cold start | RSS | p50 | p95 | Sequential throughput |\n|---|---:|---:|---:|---:|---:|---:|"]
    for n, size, cold, rss, p50, p95, qps in rows:
        L.append(f"| {n} | {size:.1f} MB | {cold:,.0f} ms | {rss:.0f} MB | {p50:.2f} ms | {p95:.2f} ms | {qps:,.0f} queries/s |")
    L.append("")
    return "\n".join(L)

def headline(res, price):
    H = []
    if "sql" in res:
        s = res["sql"]["sess"]; H.append(("SQL — a 7-question agent session", s["ccp"], s["careful"], s["naive"], s["ccp_rt"], s["careful_rt"]))
    def row(suite, key, label):
        if suite in res:
            items = res[suite]["rows"] if suite != "streams" else [x for r in res[suite] for x in r["rows"]]
            for x in items:
                if key in f"{x.source}: {x.title}": H.append((label, x.ccp, x.base, x.naive, x.ccp_calls, x.base_calls)); return
    row("redis", "50 user hashes", "Redis — 50 hashes, 2 fields each")
    row("streams", "WebSocket: Report", "WebSocket — report on a 3 s window")
    row("streams", "Socket.IO: Report", "Socket.IO — report on a 3 s window")
    row("csv", "Revenue + orders", "CSV — revenue by region, 200k rows")
    row("http", "List of 100 users: id", "REST API — 100 users, 2 fields")
    L = ["## Headline\n", "| Source / task | CCP | Conventional tool | ~Tokens (CCP → conventional) | Calls |\n|---|---:|---:|---:|---:|"]
    for label, c, b, n, rc, rb in H:
        ref = n if (n and label.startswith("SQL")) else b
        L.append(f"| {label} | {human(c)} | {human(b)}{f' (naive: {human(n)})' if n and label.startswith('SQL') else ''} | {tok(c):,} → {tok(b):,} | {rc} / {rb} |")
    L.append("")
    return "\n".join(L)

def main():
    ap = argparse.ArgumentParser(); ap.add_argument("--only", default=",".join(SUITES)); ap.add_argument("--price", type=float, default=3.0)
    a = ap.parse_args()
    res = {}; md = {}; allrows = []
    for name in a.only.split(","):
        print(f"== {name} ...", flush=True)
        mod = SUITES[name]; res[name] = mod.run(a); md[name] = mod.render(res[name], a.price)
        rs = res[name]["rows"] if name not in ("sql", "streams") else ([s["row"] for s in res[name]["S"]] if name == "sql" else [x for r in res[name] for x in r["rows"]])
        allrows += rs
    head = ["<!-- generated by bench/run_all.py -->\n", "# CCP benchmark results\n",
            f"Token estimate: ceil(bytes/4), applied identically to CCP and the baselines. Cost rows assume ${a.price:g} per million input tokens (illustrative). "
            "Baselines are *models* of the conventional approach for each source (see bench/README.md), run against the same data.\n",
            headline(res, a.price)]
    out = "\n".join(head + [md[n] for n in a.only.split(",")] + [footprint(res)])
    open(os.path.join(ROOT, "bench", "RESULTS.md"), "w").write(out)
    picks = []
    for k in CHART_PICK:
        for r in allrows:
            if k in r.title or k in f"{r.source}: {r.title}":
                if r not in picks: picks.append(r); break
    os.makedirs(os.path.join(ROOT, "docs"), exist_ok=True)
    chart(picks, os.path.join(ROOT, "docs", "benchmark.svg"))
    json.dump({n: (r if n not in ("sql", "redis", "streams", "csv", "http") else None) for n, r in res.items()} | {"rows": [x.d() for x in allrows]}, open(os.path.join(ROOT, "bench", "results.json"), "w"), indent=1, default=str)
    print(out)

if __name__ == "__main__":
    main()
