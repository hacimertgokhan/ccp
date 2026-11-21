"""CSV reporting: files too big for any context window, answered as small reports."""
import os, random, tempfile, time
from common import *

def make(path, n):
    random.seed(5)
    regions = ["EMEA", "APAC", "AMER", "LATAM"]; products = [f"sku{i:03d}" for i in range(120)]
    chans = ["web", "app", "store", "partner"]
    with open(path, "w") as f:
        f.write("order_id,date,region,product,channel,qty,price,discount,customer,notes\n")
        for i in range(1, n + 1):
            f.write(f"{i},2026-{random.randint(1,12):02d}-{random.randint(1,28):02d},{random.choice(regions)},{random.choice(products)},{random.choice(chans)},"
                    f"{random.randint(1,9)},{random.randint(500,99900)/100:.2f},{random.choice([0,0,0,5,10])},cust{random.randint(1,20000)},\"note, {random.randint(1,9999)}\"\n")

def run(args):
    n = 200_000
    d = tempfile.mkdtemp(); path = os.path.join(d, "sales.csv"); make(path, n)
    size = os.path.getsize(path)
    t0 = time.perf_counter(); m = Mcp("csv-ccp", {"CCP_PATH": path}); load_s = time.perf_counter() - t0
    rows = []
    head_n = 200
    with open(path) as f: head = "".join([next(f) for _ in range(head_n + 1)])
    full = call_bytes("read_file", {"path": "sales.csv"}, "x" * size)
    headb = call_bytes("read_file", {"path": "sales.csv", "lines": head_n}, head)
    qs = [("Revenue + orders by region", 'sales(group:"region",order:"-sum_price"){region count sum_price avg_price}'),
          ("Top 5 products by quantity", 'sales(group:"product",order:"-sum_qty",limit:5){product sum_qty count}'),
          ("Region x channel matrix", 'sales(group:"region,channel",order:"region,channel"){region channel count sum_qty}'),
          ("Distinct customers and overall max price", 'sales(group:""){count distinct_customer max_price min_price}')]
    for title, q in qs:
        cb, out, ms = m.query(q)
        rows.append(Row("CSV", f"{title} ({n:,} rows)", cb, full, 1, 1, note=f"file {human(size)}"))
    cb, _, _ = m.query('sales(region:"APAC",channel:"app",order:"-price",limit:3){order_id price qty}')
    rows.append(Row("CSV", "3 priciest APAC app orders", cb, full, 1, 1))
    lat = {t: latency(m, q, 30, 3) for t, q in qs[:2]}
    foot = dict(cold=load_s * 1000, rss=m.rss_mb(), lat=lat[qs[0][0]])
    m.close()
    return dict(rows=rows, size=size, n=n, load_s=load_s, lat=lat, foot=foot, headb=headb, full=full)

def render(r, price):
    L = []; P = L.append
    P("## 4. CSV reporting\n")
    P(f"`sales.csv`: {r['n']:,} rows, {human(r['size'])}. Baseline = a file-reading tool; the whole file is ≈{tok(r['full']):,} tokens, far beyond any context window "
      f"(even a 200-row head costs {r['headb']:,} B and answers nothing).\n")
    P("| Task | CCP | Reading the file | Saved |\n|---|---:|---:|---:|")
    for x in r["rows"]:
        P(f"| {x.title} | {x.ccp:,} B | {x.base:,} B | {pct(x.ccp, x.base)} |")
    P(f"\nLoad (spawn → ready, parse + type-infer {r['n']:,} rows into memory): **{r['load_s']:.2f} s**. Aggregate query latency afterwards:\n")
    P("| Query | p50 | p95 |\n|---|---:|---:|")
    for t, l in r["lat"].items(): P(f"| {t} | {l['p50']:.1f} ms | {l['p95']:.1f} ms |")
    P("")
    return "\n".join(L)
