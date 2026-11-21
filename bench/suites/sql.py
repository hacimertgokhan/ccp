"""SQL servers: per-task cost, reporting (group/aggregate), scaling, agent session,
cross-database parity (SQLite / MySQL(MariaDB) / PostgreSQL)."""
import os, random, sqlite3, tempfile, time
from common import *

MY, PG = "ccp-bench-maria", "ccp-bench-pg"
PW = "benchpw"

def dataset():
    random.seed(7)
    words = ("the quick brown fox jumps over lazy dog compact protocol query round trip schema field "
             "token budget agent model context window latency stream batch index cache").split()
    txt = lambda n: " ".join(random.choice(words) for _ in range(n))
    U, P, C, O = 1000, 3000, 9000, 5000
    ddl = ["CREATE TABLE users(id INT PRIMARY KEY, name VARCHAR(50), email VARCHAR(80), bio TEXT, city VARCHAR(40))",
           "CREATE TABLE posts(id INT PRIMARY KEY, author_id INT, title VARCHAR(100), body TEXT, FOREIGN KEY (author_id) REFERENCES users(id))",
           "CREATE TABLE comments(id INT PRIMARY KEY, post_id INT, author_name VARCHAR(50), body TEXT, FOREIGN KEY (post_id) REFERENCES posts(id))",
           "CREATE TABLE orders(id INT PRIMARY KEY, user_id INT, status VARCHAR(20), channel VARCHAR(20), total INT, qty INT, FOREIGN KEY (user_id) REFERENCES users(id))"]
    cities = ["Istanbul", "Ankara", "Izmir", "Berlin", "London", "Tokyo"]
    users = [(i, f"user{i}", f"user{i}@example.com", txt(25), random.choice(cities)) for i in range(1, U + 1)]
    posts = [(i, random.randint(1, U), txt(5), txt(60)) for i in range(1, P + 1)]
    comments = [(i, random.randint(1, P), f"user{random.randint(1, U)}", txt(12)) for i in range(1, C + 1)]
    orders = [(i, random.randint(1, U), random.choice(["paid", "paid", "paid", "open", "refunded", "cancelled"]),
               random.choice(["web", "app", "store"]), random.randint(5, 500), random.randint(1, 9)) for i in range(1, O + 1)]
    return ddl, [("users", users), ("posts", posts), ("comments", comments), ("orders", orders)]

def lit(v): return str(v) if isinstance(v, int) else "'" + str(v).replace("'", "''") + "'"
def inserts(tables):
    out = []
    for name, rows in tables:
        for i in range(0, len(rows), 100):
            out.append(f"INSERT INTO {name} VALUES " + ",".join("(" + ",".join(lit(v) for v in r) + ")" for r in rows[i:i + 100]) + ";")
    return out

def start_dbs(tmp):
    ddl, tables = dataset()
    script = ";\n".join(ddl) + ";\n" + "\n".join(inserts(tables))
    path = os.path.join(tmp, "shop.db")
    con = sqlite3.connect(path); con.executescript(script); con.commit(); con.close()
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

def server(kind, path):
    if kind == "sqlite": return Mcp("sqlite-ccp", {"CCP_PATH": path})
    env = dict(CCP_HOST="127.0.0.1", CCP_PASSWORD=PW, CCP_DBNAME="shop")
    if kind == "mysql": env.update(CCP_USER="root", CCP_PORT="33062")
    else: env.update(CCP_USER="postgres", CCP_PORT="33063")
    return Mcp(f"{kind}-ccp", env)

BASE_TOOLS = [
  {"name": "list_tables", "description": "List all tables in the database.", "inputSchema": {"type": "object", "properties": {}}},
  {"name": "describe_table", "description": "Describe the columns of a table.", "inputSchema": {"type": "object", "properties": {"table": {"type": "string", "description": "Table name"}}, "required": ["table"]}},
  {"name": "query", "description": "Execute a read-only SQL query and return rows as JSON.", "inputSchema": {"type": "object", "properties": {"sql": {"type": "string", "description": "SQL SELECT statement"}}, "required": ["sql"]}},
]

class Baseline:
    """Conventional SQL-over-MCP: list_tables / describe_table / query, compact JSON rows."""
    def __init__(self, path):
        self.c = sqlite3.connect(path); self.c.row_factory = sqlite3.Row
    def rows(self, sql): return [dict(r) for r in self.c.execute(sql).fetchall()]
    def calls(self, sqls):
        return sum(call_bytes("query", {"sql": s}, J(self.rows(s)), i) for i, s in enumerate(sqls))
    def setup(self):
        tools = len(J({"jsonrpc": "2.0", "id": 1, "result": {"tools": BASE_TOOLS}}).encode()) + len(J({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}).encode())
        total = tools + call_bytes("list_tables", {}, J([r["name"] for r in self.rows("select name from sqlite_master where type='table'")]))
        n = 1
        for t in ("users", "posts", "comments", "orders"):
            d = J([{"Field": r["name"], "Type": r["type"], "Null": "YES", "Key": "PRI" if r["pk"] else "", "Default": None, "Extra": ""} for r in self.rows(f"pragma table_info({t})")])
            total += call_bytes("describe_table", {"table": t}, d); n += 1
        return total, n + 1

def scenarios(ccp, base):
    S = []
    def add(id_, title, q, naive, careful, rep=False):
        cb, out, ms = ccp.query(q)
        S.append(dict(id=id_, report=rep, row=Row("SQL", title, cb, base.calls(careful), 1, len(careful), base.calls(naive), len(naive)), ccp_ms=ms, query=q))
    add("point", "Point lookup: one user, 2 fields", "users(id:42){name email}", ["SELECT * FROM users WHERE id = 42"], ["SELECT name, email FROM users WHERE id = 42"])
    add("list100", "List 100 users, 2 fields", "users(limit:100){id name}", ["SELECT * FROM users LIMIT 100"], ["SELECT id, name FROM users LIMIT 100"])
    add("wide1000", "List 1,000 posts, id+title (big body)", "posts(limit:1000){id title}", ["SELECT * FROM posts LIMIT 1000"], ["SELECT id, title FROM posts LIMIT 1000"])
    add("filter", "Filter: one author's post titles", "posts(author_id:7){title}", ["SELECT * FROM posts WHERE author_id = 7"], ["SELECT title FROM posts WHERE author_id = 7"])
    uid = 7
    pids = [r["id"] for r in base.rows(f"select id from posts where author_id={uid}")]
    add("nested2", "Nested: user -> posts -> comments", f"users(id:{uid}){{name posts{{title comments{{body}}}}}}",
        [f"SELECT * FROM users WHERE id = {uid}", f"SELECT * FROM posts WHERE author_id = {uid}", f"SELECT * FROM comments WHERE post_id IN ({','.join(map(str, pids))})"],
        [f"SELECT u.name, p.title, c.body FROM users u LEFT JOIN posts p ON p.author_id = u.id LEFT JOIN comments c ON c.post_id = p.id WHERE u.id = {uid}"])
    aids = sorted({r["author_id"] for r in base.rows("select author_id from posts order by id limit 50")})
    add("belongs", "Reverse relation: 50 posts + author name", "posts(limit:50){title author{name}}",
        ["SELECT * FROM posts LIMIT 50", f"SELECT * FROM users WHERE id IN ({','.join(map(str, aids))})"],
        ["SELECT p.title, u.name FROM posts p JOIN users u ON u.id = p.author_id ORDER BY p.id LIMIT 50"])
    add("batch3", "Three independent lookups", "users(id:7){name email} posts(author_id:7){id title} comments(post_id:7){author_name body}",
        ["SELECT * FROM users WHERE id = 7", "SELECT * FROM posts WHERE author_id = 7", "SELECT * FROM comments WHERE post_id = 7"],
        ["SELECT name, email FROM users WHERE id = 7", "SELECT id, title FROM posts WHERE author_id = 7", "SELECT author_name, body FROM comments WHERE post_id = 7"])
    # reporting: the naive model has no choice but to pull rows and aggregate in its head
    add("rep_status", "Report: revenue by status (5,000 orders)", 'orders(group:"status",order:"-sum_total"){status count sum_total avg_total}',
        ["SELECT * FROM orders"], ["SELECT status, COUNT(*) AS count, SUM(total) AS sum_total, AVG(total) AS avg_total FROM orders GROUP BY status ORDER BY sum_total DESC"], True)
    add("rep_top", "Report: top 10 users by spend", 'orders(status:"paid",group:"user_id",order:"-sum_total",limit:10){user_id count sum_total}',
        ["SELECT * FROM orders WHERE status = 'paid'"], ["SELECT user_id, COUNT(*) AS count, SUM(total) AS sum_total FROM orders WHERE status = 'paid' GROUP BY user_id ORDER BY sum_total DESC LIMIT 10"], True)
    add("rep_matrix", "Report: channel x status matrix", 'orders(group:"channel,status",order:"channel,status"){channel status count sum_qty}',
        ["SELECT * FROM orders"], ["SELECT channel, status, COUNT(*) AS count, SUM(qty) AS sum_qty FROM orders GROUP BY channel, status ORDER BY channel, status"], True)
    return S

PARITY = ["users(limit:5){id name email city}", "users(id:7){name posts{title comments{body}}}", "posts(limit:20){title author{name city}}",
          'users(city:"Tokyo",limit:3){id name}', "comments(limit:5,offset:10){id post_id author_name}", "_tables{name columns{name type}}",
          "users(id:999999){name}", 'posts(author_id:"7"){id}',
          'orders(group:"status",order:"status"){status count sum_total min_total max_total distinct_user_id}',
          'orders(group:"channel,status",order:"-sum_qty,channel,status",limit:5){channel status sum_qty}',
          'orders(group:""){count sum_total max_qty}', 'orders(order:"-total,id",limit:5){id total}']

def parity(kinds, path):
    res = {k: [] for k in kinds}; foot = {}
    for k in kinds:
        m = server(k, path)
        for q in PARITY: res[k].append(m.query(q)[1])
        foot[k] = dict(cold=m.cold_ms, rss=m.rss_mb(), lat=latency(m, "users(id:7){name posts{title comments{body}}}", 100))
        m.close()
    bad = [PARITY[i] for i in range(len(PARITY)) if len({res[k][i] for k in kinds}) > 1]
    return not bad, bad, foot

def run(args):
    tmp = tempfile.mkdtemp(); path = start_dbs(tmp)
    try:
        kinds = ["sqlite", "mysql", "postgres"]
        ok, bad, foot = parity(kinds, path)
        ccp = server("sqlite", path); base = Baseline(path)
        setup_ccp = ccp.setup_cost(); setup_base, setup_rt = base.setup()
        S = scenarios(ccp, base)
        SC = []
        for n in (10, 100, 1000):
            cb, _, _ = ccp.query(f"users(limit:{n}){{id name}}")
            SC.append(dict(rows=n, ccp=cb, naive=base.calls([f"SELECT * FROM users LIMIT {n}"]), careful=base.calls([f"SELECT id, name FROM users LIMIT {n}"])))
        ccp.close()
        mix = ["point", "filter", "nested2", "belongs", "batch3", "rep_status", "rep_top"]
        sel = [s["row"] for s in S if s["id"] in mix]
        sess = dict(ccp=setup_ccp + sum(r.ccp for r in sel), careful=setup_base + sum(r.base for r in sel), naive=setup_base + sum(r.naive for r in sel),
                    ccp_rt=2 + sum(r.ccp_calls for r in sel), careful_rt=setup_rt + sum(r.base_calls for r in sel), naive_rt=setup_rt + sum(r.naive_calls for r in sel))
        return dict(parity=ok, bad=bad, foot=foot, S=S, SC=SC, sess=sess, setup=(setup_ccp, setup_base, setup_rt), kinds=kinds)
    finally:
        stop_dbs()

def render(r, price):
    L = []; P = L.append
    P("## 1. SQL servers (MySQL / PostgreSQL / SQLite)\n")
    P("Dataset: 1,000 users, 3,000 posts, 9,000 comments, 5,000 orders (foreign keys users←posts←comments, users←orders).\n")
    P(f"**Parity** — {len(PARITY)} queries (filters, nested relations, aggregates, ordering, `_tables`) on SQLite, MariaDB 11 and PostgreSQL 17: "
      f"{'**byte-identical on all three**' if r['parity'] else 'MISMATCH: ' + str(r['bad'])}.\n")
    P("### Per-task cost\n")
    P("| Task | CCP | SQL-MCP careful | SQL-MCP naive | Calls (CCP / careful / naive) | Saved vs careful | Saved vs naive |\n|---|---:|---:|---:|---:|---:|---:|")
    for s in r["S"]:
        x = s["row"]
        P(f"| {x.title} | {x.ccp:,} B | {x.base:,} B | {x.naive:,} B | {x.ccp_calls} / {x.base_calls} / {x.naive_calls} | {pct(x.ccp, x.base)} | {pct(x.ccp, x.naive)} |")
    c, b, rt = r["setup"]
    P(f"\n### Session setup\n\n| | CCP | SQL-MCP |\n|---|---:|---:|\n| tool definitions + schema | {c:,} B (2 calls) | {b:,} B ({rt} calls) |\n")
    P("### Scaling: `users{id name}`\n\n| Rows | CCP | careful | naive | saved vs careful | saved vs naive |\n|---:|---:|---:|---:|---:|---:|")
    for x in r["SC"]:
        P(f"| {x['rows']:,} | {x['ccp']:,} B | {x['careful']:,} B | {x['naive']:,} B | {pct(x['ccp'], x['careful'])} | {pct(x['ccp'], x['naive'])} |")
    s = r["sess"]
    P("\n### A whole agent session\n\nSetup + 7 questions (point, filter, nested, reverse relation, 3 batched lookups, two reports).\n")
    P(f"| | Bytes | ~Tokens | Round trips | Input cost / 1,000 sessions (@${price:g}/M) |\n|---|---:|---:|---:|---:|")
    for key, lbl in (("ccp", "**CCP**"), ("careful", "SQL-MCP careful"), ("naive", "SQL-MCP naive")):
        P(f"| {lbl} | {s[key]:,} | {tok(s[key]):,} | {s[key+'_rt']} | ${tok(s[key]) * 1000 * price / 1e6:,.2f} |")
    P(f"\nSaved: **{pct(s['ccp'], s['careful'])}** vs careful, **{pct(s['ccp'], s['naive'])}** vs naive.\n")
    return "\n".join(L)
