#!/usr/bin/env python3
"""CCP vs a conventional SQL-over-MCP server: bytes, tokens, round trips.

Real side     : mysql-ccp / postgres-ccp / sqlite-ccp, driven over MCP exactly as
                Claude drives them (JSON-RPC envelopes included).
Baseline side : the usual SQL-MCP shape (list_tables / describe_table / query
                tools, compact JSON rows with every key repeated per row),
                executed against the same data. Two model behaviours:
                  naive   - SELECT * and one query per related table
                  careful - exact columns and a JOIN for related data
                The baseline is a model of that shape, not a specific product.
Tokens        : estimated as ceil(bytes / 4) on BOTH sides (no tokenizer needed;
                the same estimator keeps the comparison fair).

Needs: docker (mariadb:11, postgres:17-alpine), `cargo build --release`.
Usage: python3 bench/bench.py [--price 3.0] [--keep]
"""
import argparse, json, math, os, random, sqlite3, statistics, subprocess, sys, tempfile, time

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
BIN = lambda n: os.path.join(ROOT, "target", "release", n)
MY, PG = "ccp-bench-maria", "ccp-bench-pg"
PW = "benchpw"

def sh(*a, inp=None, check=True):
    return subprocess.run(a, input=inp, capture_output=True, text=True, check=check).stdout

# ---------------------------------------------------------------- dataset
def dataset():
    random.seed(7)
    words = ("the quick brown fox jumps over lazy dog compact protocol query round trip schema field "
             "token budget agent model context window latency stream batch index cache").split()
    txt = lambda n: " ".join(random.choice(words) for _ in range(n))
    U, P, C = 1000, 3000, 9000
    ddl = ["CREATE TABLE users(id INT PRIMARY KEY, name VARCHAR(50), email VARCHAR(80), bio TEXT, city VARCHAR(40))",
           "CREATE TABLE posts(id INT PRIMARY KEY, author_id INT, title VARCHAR(100), body TEXT, FOREIGN KEY (author_id) REFERENCES users(id))",
           "CREATE TABLE comments(id INT PRIMARY KEY, post_id INT, author_name VARCHAR(50), body TEXT, FOREIGN KEY (post_id) REFERENCES posts(id))"]
    cities = ["Istanbul", "Ankara", "Izmir", "Berlin", "London", "Tokyo"]
    users = [(i, f"user{i}", f"user{i}@example.com", txt(25), random.choice(cities)) for i in range(1, U + 1)]
    posts = [(i, random.randint(1, U), txt(5), txt(60)) for i in range(1, P + 1)]
    comments = [(i, random.randint(1, P), f"user{random.randint(1, U)}", txt(12)) for i in range(1, C + 1)]
    return ddl, [("users", users), ("posts", posts), ("comments", comments)]

def lit(v): return str(v) if isinstance(v, int) else "'" + str(v).replace("'", "''") + "'"
def inserts(tables):
    out = []
    for name, rows in tables:
        for i in range(0, len(rows), 100):
            out.append(f"INSERT INTO {name} VALUES " + ",".join("(" + ",".join(lit(v) for v in r) + ")" for r in rows[i:i + 100]) + ";")
    return out

# ---------------------------------------------------------------- databases
def start_dbs(tmp):
    ddl, tables = dataset(); ins = inserts(tables)
    script = ";\n".join(ddl) + ";\n" + "\n".join(ins)
    # sqlite
    path = os.path.join(tmp, "shop.db")
    con = sqlite3.connect(path); con.executescript(script); con.commit(); con.close()
    # docker dbs
    for c in (MY, PG): subprocess.run(["docker", "rm", "-f", c], capture_output=True)
    sh("docker", "run", "-d", "--name", MY, "-e", f"MARIADB_ROOT_PASSWORD={PW}", "-e", "MARIADB_DATABASE=shop", "-p", "127.0.0.1:33062:3306", "mariadb:11")
    sh("docker", "run", "-d", "--name", PG, "-e", f"POSTGRES_PASSWORD={PW}", "-e", "POSTGRES_DB=shop", "-p", "127.0.0.1:33063:5432", "postgres:17-alpine")
    for _ in range(90):
        a = subprocess.run(["docker", "exec", MY, "mariadb", "-uroot", f"-p{PW}", "-e", "select 1"], capture_output=True).returncode == 0
        b = subprocess.run(["docker", "exec", PG, "pg_isready", "-U", "postgres", "-d", "shop"], capture_output=True).returncode == 0
        if a and b: break
        time.sleep(2)
    time.sleep(3)
    sh("docker", "exec", "-i", MY, "mariadb", "-uroot", f"-p{PW}", "shop", inp=script)
    sh("docker", "exec", "-i", PG, "psql", "-U", "postgres", "-d", "shop", "-v", "ON_ERROR_STOP=1", "-q", inp=script)
    return path

def stop_dbs():
    for c in (MY, PG): subprocess.run(["docker", "rm", "-f", c], capture_output=True)

def server_env(kind, sqlite_path):
    e = dict(os.environ)
    if kind == "sqlite": e.update(CCP_PATH=sqlite_path)
    else:
        e.update(CCP_HOST="127.0.0.1", CCP_USER="root" if kind == "mysql" else "postgres", CCP_PASSWORD=PW, CCP_DBNAME="shop",
                 CCP_PORT="33062" if kind == "mysql" else "33063")
    return e

class Mcp:
    def __init__(self, kind, sqlite_path):
        self.p = subprocess.Popen([BIN(f"{kind}-ccp")], stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True, env=server_env(kind, sqlite_path))
        self.n = 0; self.call("initialize", {"protocolVersion": "2024-11-05"})
    def call(self, method, params=None):
        self.n += 1
        req = json.dumps({"jsonrpc": "2.0", "id": self.n, "method": method, "params": params or {}}, separators=(",", ":"))
        t = time.perf_counter(); self.p.stdin.write(req + "\n"); self.p.stdin.flush()
        resp = self.p.stdout.readline().strip(); dt = time.perf_counter() - t
        return len(req.encode()), len(resp.encode()), resp, dt
    def tool(self, name, args): return self.call("tools/call", {"name": name, "arguments": args})
    def query(self, q):
        a, b, resp, dt = self.tool("ccp_query", {"query": q})
        return a + b, json.loads(resp)["result"]["content"][0]["text"], dt
    def close(self):
        self.p.stdin.close(); self.p.wait(timeout=10)

# ---------------------------------------------------------------- baseline model
J = lambda o: json.dumps(o, separators=(",", ":"), ensure_ascii=False)
def req_bytes(name, args, i=1): return len(J({"jsonrpc": "2.0", "id": i, "method": "tools/call", "params": {"name": name, "arguments": args}}).encode())
def resp_bytes(text, i=1): return len(J({"jsonrpc": "2.0", "id": i, "result": {"content": [{"type": "text", "text": text}], "isError": False}}).encode())

BASE_TOOLS = [
  {"name": "list_tables", "description": "List all tables in the database.", "inputSchema": {"type": "object", "properties": {}}},
  {"name": "describe_table", "description": "Describe the columns of a table.", "inputSchema": {"type": "object", "properties": {"table": {"type": "string", "description": "Table name"}}, "required": ["table"]}},
  {"name": "query", "description": "Execute a read-only SQL query and return rows as JSON.", "inputSchema": {"type": "object", "properties": {"sql": {"type": "string", "description": "SQL SELECT statement"}}, "required": ["sql"]}},
]

class Baseline:
    def __init__(self, path):
        self.c = sqlite3.connect(path); self.c.row_factory = sqlite3.Row
    def rows(self, sql): return [dict(r) for r in self.c.execute(sql).fetchall()]
    def calls(self, sqls):
        """bytes for a sequence of query tool calls (one round trip each)"""
        return sum(req_bytes("query", {"sql": s}, i) + resp_bytes(J(self.rows(s)), i) for i, s in enumerate(sqls))
    def setup(self):
        tools = len(J({"jsonrpc": "2.0", "id": 1, "result": {"tools": BASE_TOOLS}}).encode()) + len(J({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}).encode())
        lt = J([r["name"] for r in self.rows("select name from sqlite_master where type='table'")])
        total = tools + req_bytes("list_tables", {}) + resp_bytes(lt)
        for t in ("users", "posts", "comments"):
            d = J([{"Field": r["name"], "Type": r["type"], "Null": "YES", "Key": "PRI" if r["pk"] else "", "Default": None, "Extra": ""} for r in self.rows(f"pragma table_info({t})")])
            total += req_bytes("describe_table", {"table": t}) + resp_bytes(d)
        return total, 1 + 1 + 3  # tools/list + list_tables + 3 describes

# ---------------------------------------------------------------- scenarios
def scenarios(ccp, base):
    S = []
    def add(id_, title, ccp_q, naive_sqls, careful_sqls):
        cb, out, _ = ccp.query(ccp_q) if isinstance(ccp_q, str) else (sum(ccp.query(q)[0] for q in ccp_q), "", 0)
        rt = 1 if isinstance(ccp_q, str) else len(ccp_q)
        S.append(dict(id=id_, title=title, ccp=cb, ccp_rt=rt, naive=base.calls(naive_sqls), naive_rt=len(naive_sqls),
                      careful=base.calls(careful_sqls), careful_rt=len(careful_sqls), query=ccp_q))
    add("point", "Point lookup: one user, 2 fields", "users(id:42){name email}",
        ["SELECT * FROM users WHERE id = 42"], ["SELECT name, email FROM users WHERE id = 42"])
    add("list100", "List 100 users, 2 fields", "users(limit:100){id name}",
        ["SELECT * FROM users LIMIT 100"], ["SELECT id, name FROM users LIMIT 100"])
    add("wide1000", "List 1000 posts, id+title (big body column)", "posts(limit:1000){id title}",
        ["SELECT * FROM posts LIMIT 1000"], ["SELECT id, title FROM posts LIMIT 1000"])
    add("filter", "Filter: one author's post titles", "posts(author_id:7){title}",
        ["SELECT * FROM posts WHERE author_id = 7"], ["SELECT title FROM posts WHERE author_id = 7"])
    uid = 7
    pids = [r["id"] for r in base.rows(f"select id from posts where author_id={uid}")]
    inlist = ",".join(map(str, pids))
    add("nested2", "Nested: user -> posts -> comments", f"users(id:{uid}){{name posts{{title comments{{body}}}}}}",
        [f"SELECT * FROM users WHERE id = {uid}", f"SELECT * FROM posts WHERE author_id = {uid}", f"SELECT * FROM comments WHERE post_id IN ({inlist})"],
        [f"SELECT u.name, p.title, c.body FROM users u LEFT JOIN posts p ON p.author_id = u.id LEFT JOIN comments c ON c.post_id = p.id WHERE u.id = {uid}"])
    aids = sorted({r["author_id"] for r in base.rows("select author_id from posts order by id limit 50")})
    add("belongs", "Reverse relation: 50 posts with author name", "posts(limit:50){title author{name}}",
        ["SELECT * FROM posts LIMIT 50", f"SELECT * FROM users WHERE id IN ({','.join(map(str, aids))})"],
        ["SELECT p.title, u.name FROM posts p JOIN users u ON u.id = p.author_id ORDER BY p.id LIMIT 50"])
    add("batch3", "Three independent lookups", "users(id:7){name email} posts(author_id:7){id title} comments(post_id:7){author_name body}",
        ["SELECT * FROM users WHERE id = 7", "SELECT * FROM posts WHERE author_id = 7", "SELECT * FROM comments WHERE post_id = 7"],
        ["SELECT name, email FROM users WHERE id = 7", "SELECT id, title FROM posts WHERE author_id = 7", "SELECT author_name, body FROM comments WHERE post_id = 7"])
    return S

def scaling(ccp, base):
    out = []
    for n in (10, 100, 1000):
        cb, _, _ = ccp.query(f"users(limit:{n}){{id name}}")
        out.append(dict(rows=n, ccp=cb, naive=base.calls([f"SELECT * FROM users LIMIT {n}"]), careful=base.calls([f"SELECT id, name FROM users LIMIT {n}"])))
    return out

# ---------------------------------------------------------------- parity + latency
PARITY = ["users(limit:5){id name email city}", "users(id:7){name posts{title comments{body}}}", "posts(limit:20){title author{name city}}",
          "users(city:\"Tokyo\",limit:3){id name}", "comments(limit:5,offset:10){id post_id author_name}", "_tables{name columns{name type}}",
          "users(id:999999){name}", "posts(author_id:\"7\"){id}"]

def parity(kinds, path):
    res = {k: [] for k in kinds}; lat = {}
    for k in kinds:
        m = Mcp(k, path); ts = []
        for q in PARITY:
            _, text, _ = m.query(q); res[k].append(text)
        heavy = "users(id:7){name posts{title comments{body}}}"
        for _ in range(30):
            ts.append(m.query(heavy)[2] * 1000)
        lat[k] = (statistics.median(ts), max(ts))
        m.close()
    ok = all(res[k] == res[kinds[0]] for k in kinds)
    bad = [PARITY[i] for i in range(len(PARITY)) if len({res[k][i] for k in kinds}) > 1]
    return ok, bad, lat

# ---------------------------------------------------------------- output
tok = lambda b: math.ceil(b / 4)
def pct(c, b): return f"{100 * (1 - c / b):.0f}%"

def chart_svg(S, path):
    rows = [(s["title"].split(":")[0] if len(s["title"]) > 34 else s["title"], s) for s in S]
    W, bar, gap, left = 880, 16, 30, 270
    scale = (W - left - 120)
    h = 70 + len(rows) * (bar * 3 + gap)
    c = {"ccp": "#2563eb", "careful": "#9ca3af", "naive": "#d1d5db"}
    o = [f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {W} {h}" font-family="-apple-system,Segoe UI,Helvetica,Arial,sans-serif" font-size="13">',
         f'<rect width="{W}" height="{h}" fill="#ffffff"/>',
         f'<text x="20" y="28" font-size="17" font-weight="600" fill="#111827">Bytes on the wire per task (log scale, lower is better)</text>']
    x0 = 20 + left - 20
    for lbl, col in (("CCP", c["ccp"]), ("careful SQL-MCP", c["careful"]), ("naive SQL-MCP", c["naive"])):
        pass
    lx = 20
    for lbl, col in (("CCP", c["ccp"]), ("SQL-MCP, careful", c["careful"]), ("SQL-MCP, naive", c["naive"])):
        o.append(f'<rect x="{lx}" y="40" width="12" height="12" rx="2" fill="{col}"/><text x="{lx+18}" y="51" fill="#374151">{lbl}</text>'); lx += 150
    mx = max(max(s["ccp"], s["careful"], s["naive"]) for s in S)
    f = lambda v: scale * math.log10(max(v, 10)) / math.log10(mx)
    y = 70
    for lbl, s in rows:
        o.append(f'<text x="20" y="{y+bar+4}" fill="#111827">{lbl}</text>')
        for i, (key, col) in enumerate((("ccp", c["ccp"]), ("careful", c["careful"]), ("naive", c["naive"]))):
            w = f(s[key]); yy = y + i * (bar + 2)
            o.append(f'<rect x="{x0}" y="{yy}" width="{w:.1f}" height="{bar}" rx="2" fill="{col}"/>')
            o.append(f'<text x="{x0+w+6:.1f}" y="{yy+bar-3}" fill="#374151">{s[key]:,} B</text>')
        y += bar * 3 + gap
    o.append("</svg>")
    open(path, "w").write("\n".join(o))

def main():
    ap = argparse.ArgumentParser(); ap.add_argument("--price", type=float, default=3.0, help="USD per 1M input tokens (illustrative)")
    ap.add_argument("--keep", action="store_true"); a = ap.parse_args()
    tmp = tempfile.mkdtemp(); path = start_dbs(tmp)
    try:
        kinds = ["sqlite", "mysql", "postgres"]
        ok, bad, lat = parity(kinds, path)
        ccp = Mcp("sqlite", path); base = Baseline(path)
        setup_ccp = sum(ccp.call("tools/list")[:2]) + sum(ccp.tool("ccp_schema", {})[:2])
        setup_base, setup_rt = base.setup()
        S = scenarios(ccp, base); SC = scaling(ccp, base); ccp.close()

        # agent session = setup + a realistic mix of questions
        mix = ["point", "filter", "nested2", "belongs", "batch3"]
        sel = [s for s in S if s["id"] in mix]
        sess = dict(ccp=setup_ccp + sum(s["ccp"] for s in sel), careful=setup_base + sum(s["careful"] for s in sel), naive=setup_base + sum(s["naive"] for s in sel),
                    ccp_rt=2 + sum(s["ccp_rt"] for s in sel), careful_rt=setup_rt + sum(s["careful_rt"] for s in sel), naive_rt=setup_rt + sum(s["naive_rt"] for s in sel))

        L = []; P = L.append
        P("<!-- generated by bench/bench.py -->")
        P(f"Dataset: 1,000 users, 3,000 posts, 9,000 comments (foreign keys users<-posts<-comments).  Token estimate: bytes/4 on both sides.\n")
        P(f"**Parity check** (same 8 queries on SQLite, MySQL/MariaDB and PostgreSQL): {'identical output on all three' if ok else 'MISMATCH on ' + str(bad)}.\n")
        P("### Per-task cost\n")
        P("| Task | CCP | SQL-MCP careful | SQL-MCP naive | CCP calls | careful / naive calls | saved vs careful | saved vs naive |")
        P("|---|---:|---:|---:|---:|---:|---:|---:|")
        for s in S:
            P(f"| {s['title']} | {s['ccp']:,} B | {s['careful']:,} B | {s['naive']:,} B | {s['ccp_rt']} | {s['careful_rt']} / {s['naive_rt']} | {pct(s['ccp'], s['careful'])} | {pct(s['ccp'], s['naive'])} |")
        P("\n### Session setup (what the model pays before the first answer)\n")
        P("| | CCP | SQL-MCP | saved |\n|---|---:|---:|---:|")
        P(f"| tool definitions + schema | {setup_ccp:,} B (2 calls) | {setup_base:,} B ({setup_rt} calls) | {pct(setup_ccp, setup_base)} |")
        P("\n### Scaling: `users{id name}` as the result grows\n")
        P("| Rows | CCP | careful | naive | saved vs careful | saved vs naive |\n|---:|---:|---:|---:|---:|---:|")
        for r in SC:
            P(f"| {r['rows']:,} | {r['ccp']:,} B | {r['careful']:,} B | {r['naive']:,} B | {pct(r['ccp'], r['careful'])} | {pct(r['ccp'], r['naive'])} |")
        P("\n### A whole agent session\n")
        P("Setup + 5 questions (point lookup, filter, nested 2-level, reverse relation, 3 batched lookups).\n")
        P(f"| | Bytes | ~Tokens | Round trips | Input cost per 1,000 sessions (@${a.price:g}/M tokens) |\n|---|---:|---:|---:|---:|")
        for key, lbl in (("ccp", "**CCP**"), ("careful", "SQL-MCP careful"), ("naive", "SQL-MCP naive")):
            P(f"| {lbl} | {sess[key]:,} | {tok(sess[key]):,} | {sess[key+'_rt']} | ${tok(sess[key]) * 1000 * a.price / 1e6:,.2f} |")
        P(f"\nSaved: **{pct(sess['ccp'], sess['careful'])}** vs careful, **{pct(sess['ccp'], sess['naive'])}** vs naive "
          f"({tok(sess['careful']) - tok(sess['ccp']):,} / {tok(sess['naive']) - tok(sess['ccp']):,} tokens per session).")
        P("\n### CCP server latency, nested 2-level query (30 runs)\n")
        P("| Backend | median | max |\n|---|---:|---:|")
        for k in kinds: P(f"| {k}-ccp | {lat[k][0]:.1f} ms | {lat[k][1]:.1f} ms |")
        md = "\n".join(L)
        open(os.path.join(ROOT, "bench", "RESULTS.md"), "w").write(md + "\n")
        json.dump(dict(scenarios=S, scaling=SC, session=sess, setup=dict(ccp=setup_ccp, base=setup_base), parity=ok, latency_ms=lat), open(os.path.join(ROOT, "bench", "results.json"), "w"), indent=1)
        chart_svg(S, os.path.join(ROOT, "docs", "benchmark.svg"))
        print(md)
    finally:
        if not a.keep: stop_dbs()

main()
