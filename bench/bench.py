#!/usr/bin/env python3
"""CCP vs a conventional SQL-over-MCP server: bytes on the wire.

Real side:     mysql-ccp, driven over MCP exactly as Claude drives it.
Baseline side: the usual MCP SQL server shape (list_tables / describe_table /
               query tools; JSON rows with every key repeated per row),
               produced from the same live database and wrapped in the same
               JSON-RPC envelope. The baseline is a model of that shape, not a
               specific product. Tokens are estimated as bytes/4 (no tokenizer).
Requires: docker (mariadb:11), a built mysql-ccp (cargo build --release).
"""
import json, subprocess, time, os, sys, random

C = "ccp-bench-maria"; PW = "benchpw"; PORT = "33062"
BIN = os.environ.get("MYSQL_CCP", "target/release/mysql-ccp")

def sh(*a, inp=None):
    return subprocess.run(a, input=inp, capture_output=True, text=True, check=True).stdout

def sql(q):
    return sh("docker", "exec", C, "mariadb", "-uroot", f"-p{PW}", "shop", "--batch", "-e", q)

def setup():
    subprocess.run(["docker", "rm", "-f", C], capture_output=True)
    sh("docker", "run", "-d", "--name", C, "-e", f"MARIADB_ROOT_PASSWORD={PW}", "-e", "MARIADB_DATABASE=shop", "-p", f"127.0.0.1:{PORT}:3306", "mariadb:11")
    for _ in range(60):
        if subprocess.run(["docker", "exec", C, "mariadb", "-uroot", f"-p{PW}", "-e", "select 1"], capture_output=True).returncode == 0: break
        time.sleep(2)
    random.seed(7)
    words = "the quick brown fox jumps over lazy dog compact protocol query round trip schema field".split()
    txt = lambda n: " ".join(random.choice(words) for _ in range(n))
    stmts = ["CREATE TABLE users(id INT PRIMARY KEY, name VARCHAR(50), email VARCHAR(80), bio TEXT, created_at DATETIME)",
             "CREATE TABLE posts(id INT PRIMARY KEY, author_id INT, title VARCHAR(100), body TEXT)",
             "CREATE TABLE comments(id INT PRIMARY KEY, post_id INT, author_name VARCHAR(50), body TEXT)"]
    stmts += [f"INSERT INTO users VALUES({i},'user{i}','user{i}@example.com','{txt(25)}','2026-01-{i%28+1:02d} 10:00:00')" for i in range(1, 101)]
    stmts += [f"INSERT INTO posts VALUES({i},{i%100+1},'{txt(5)}','{txt(60)}')" for i in range(1, 301)]
    stmts += [f"INSERT INTO comments VALUES({i},{i%300+1},'user{i%100+1}','{txt(12)}')" for i in range(1, 601)]
    sh("docker", "exec", "-i", C, "mariadb", f"-uroot", f"-p{PW}", "shop", inp=";\n".join(stmts) + ";")

def rows(q):
    out = sql(q).strip().split("\n")
    if not out or not out[0]: return []
    cols = out[0].split("\t")
    return [dict(zip(cols, [None if v == "NULL" else (int(v) if v.lstrip("-").isdigit() else v) for v in l.split("\t")])) for l in out[1:]]

env = lambda: dict(os.environ, CCP_HOST="127.0.0.1", CCP_PORT=PORT, CCP_USER="root", CCP_PASSWORD=PW, CCP_DBNAME="shop")

class Mcp:
    def __init__(self):
        self.p = subprocess.Popen([BIN], stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True, env=env())
        self.n = 0
    def call(self, method, params=None):
        self.n += 1
        req = json.dumps({"jsonrpc": "2.0", "id": self.n, "method": method, "params": params or {}}, separators=(",", ":"))
        self.p.stdin.write(req + "\n"); self.p.stdin.flush()
        resp = self.p.stdout.readline().strip()
        return len(req), len(resp), resp
    def tool(self, name, args):
        return self.call("tools/call", {"name": name, "arguments": args})

def envelope(result_text, i=1):  # baseline response envelope, same shape as MCP's
    return len(json.dumps({"jsonrpc": "2.0", "id": i, "result": {"content": [{"type": "text", "text": result_text}], "isError": False}}, separators=(",", ":")))
def request(name, args, i=1):
    return len(json.dumps({"jsonrpc": "2.0", "id": i, "method": "tools/call", "params": {"name": name, "arguments": args}}, separators=(",", ":")))

BASE_TOOLS = [
  {"name": "list_tables", "description": "List all tables in the database.", "inputSchema": {"type": "object", "properties": {}}},
  {"name": "describe_table", "description": "Describe the columns of a table.", "inputSchema": {"type": "object", "properties": {"table": {"type": "string", "description": "Table name"}}, "required": ["table"]}},
  {"name": "query", "description": "Execute a read-only SQL query and return rows as JSON.", "inputSchema": {"type": "object", "properties": {"sql": {"type": "string", "description": "SQL SELECT statement"}}, "required": ["sql"]}},
]

def main():
    setup(); m = Mcp()
    m.call("initialize", {"protocolVersion": "2024-11-05"})
    res = []
    def row(name, ccp, base, note=""): res.append((name, ccp, base, note))
    J = lambda o: json.dumps(o, separators=(",", ":"))

    # A. session setup: tool definitions + schema the model must read once.
    rq, rs, resp = m.call("tools/list")
    sq, ss, _ = m.tool("ccp_schema", {})
    ccp_setup = rq + rs + sq + ss
    base_list = len(J({"jsonrpc": "2.0", "id": 1, "result": {"tools": BASE_TOOLS}})) + 40
    lt = J([r["Tables_in_shop"] for r in rows("show tables")])
    ds = [J(rows(f"describe {t}")) for t in ("users", "posts", "comments")]
    base_setup = base_list + request("list_tables", {}) + envelope(lt) + sum(request("describe_table", {"table": t}) + envelope(d) for t, d in zip(("users", "posts", "comments"), ds))
    row("A. Session setup (tool defs + schema)", ccp_setup, base_setup, "CCP: 1 schema call; baseline: list + 3 describes")

    # B. list 100 users, 2 columns
    q = "users(limit:100){id name}"
    a, b, _ = m.tool("ccp_query", {"query": q}); ccp = a + b
    star = J(rows("select * from users limit 100")); cols = J(rows("select id,name from users limit 100"))
    s1 = request("query", {"sql": "SELECT * FROM users LIMIT 100"}) + envelope(star)
    s2 = request("query", {"sql": "SELECT id, name FROM users LIMIT 100"}) + envelope(cols)
    row("B. 100 users, id+name  (vs SELECT *)", ccp, s1, "model overfetches")
    row("B. 100 users, id+name  (vs exact columns)", ccp, s2, "best case for baseline")

    # C. three independent lookups
    ccp_q = "users(id:7){name email} posts(author_id:7){id title} comments(post_id:7){author_name body}"
    a, b, _ = m.tool("ccp_query", {"query": ccp_q}); ccp = a + b
    qs = ["SELECT name,email FROM users WHERE id=7", "SELECT id,title FROM posts WHERE author_id=7", "SELECT author_name,body FROM comments WHERE post_id=7"]
    base = sum(request("query", {"sql": s}, i) + envelope(J(rows(s)), i) for i, s in enumerate(qs))
    row("C. 3 lookups (1 call vs 3 calls)", ccp, base, "CCP batches roots in one call")

    # D. wide table, narrow need: posts(body is large)
    a, b, _ = m.tool("ccp_query", {"query": "posts(limit:300){id title}"}); ccp = a + b
    s = request("query", {"sql": "SELECT * FROM posts LIMIT 300"}) + envelope(J(rows("select * from posts limit 300")))
    row("D. 300 posts, id+title (big body column)", ccp, s, "")

    print(f"{'scenario':46} {'CCP B':>8} {'base B':>8} {'saved':>7} {'~tok CCP':>9} {'~tok base':>10}")
    for n, c, b, note in res:
        print(f"{n:46} {c:8} {b:8} {100*(1-c/b):6.0f}% {c//4:9} {b//4:10}   {note}")
    m.p.stdin.close()
    subprocess.run(["docker", "rm", "-f", C], capture_output=True)

main()
