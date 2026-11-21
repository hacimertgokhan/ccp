# CCP protocol

CCP — the **Core Context Protocol** — lets a client ask a data source for exactly
the context it needs, in one request, and get it back in a compact form. This is
the reference for protocol version 0.3.

A CCP server speaks over **stdio, one line in, one line out**. Every server in
this repository additionally speaks MCP by default, so it plugs straight into
Claude; see [MCP front end](#mcp-front-end).

## Messages

| Client sends | Server replies |
|---|---|
| `SCHEMA` | the schema advertisement, newlines replaced by ` \| ` |
| `PING` | `OK <info>` or `ERR <reason>` |
| anything else | a query: the compact result, or `ERR <reason>` |

Empty lines are ignored. A client reads the schema **once**, then only sends queries.

## Query language

```text
query      = selection { selection }
selection  = name [ "(" [ arg { [","] arg } ] ")" ] [ "{" selection { selection } "}" ]
arg        = name ":" value
value      = integer | "string" | true | false
name       = letters, digits, "_" ; after the first character also "." and "-"
integer    = ["-"] digits
```

* Selections are separated by whitespace; commas between arguments are optional.
* Braces choose which fields to return. A field with braces is a nested object or list.
* Several root selections in one query run in order and return in one object.
* Strings are delimited by `"`; inside them `\"`, `\\`, `\n` and `\t` are escapes.
* Names may contain `.` and `-`, so a field can be a **path** (`address.city`) or a
  JSON key such as `content-type`.
* Nesting is limited to 64 levels. Unknown characters are skipped.

```text
users{id name}
users(id:7){name posts{title comments{body}}}
users(city:"Tokyo",limit:5) posts(author_id:7){title}
get(path:"/users/1"){name address.city}
```

## Result encoding

```text
value  = null | true | false | integer | float | "string" | [ value, ... ] | { key:value, ... }
```

* Object keys are bare (unquoted); strings escape `\`, `"`, newline and carriage return.
* A root that returns many rows is a list; a to-one relation is an object or `null`.
* A field the server does not know resolves to `null`.
* A failure inside one root selection is reported in place as `{error:"..."}`;
  the rest of the query still returns. A malformed query returns `ERR <reason>`.

## Schema advertisement

```text
root users(id:Int,name:Str):[Users]
all table roots also take limit:Int,offset:Int,order:"-f,g",group:"cols"; ...
type Users{id:Int,name:Str,posts:[Posts]}
```

* `root name(args):Type` declares a root field and its arguments.
* `type Name{field:Type,...}` declares an object type.
* Scalar types are `Int`, `Float`, `Str`, `Bool`; `[T]` is a list; any other name is an object type.
* Servers over free-form data (JSON APIs, streams) describe their vocabulary in prose-like
  lines, e.g. `...any payload field; a.b.c paths ok`.

## Conventions every server follows

These are what make CCP servers feel the same, whatever they sit on:

| Convention | Meaning |
|---|---|
| Read-only | no root ever changes the source |
| Bounded | every root caps rows / bytes / time (`limit` defaults to a small number) |
| `limit`, `offset` | paging on every list root |
| `order:"-a,b"` | sort (SQL servers); `-` = descending |
| `_field` | meta fields a server adds are prefixed with `_` (`_seq`, `_ts`, `_key`) |
| Selection = projection | only the fields named in braces are fetched/returned; no braces = a small default |
| Errors in place | `{error:"..."}` for one root, never a dropped connection |

## SQL servers (`ccp-sql`)

MySQL/MariaDB, PostgreSQL, SQLite and CSV share one engine:

* Every table is a root named after the table, returning `[Table]`.
* Arguments that name a column filter by equality (several are ANDed);
  `limit` (default 100, max 1000), `offset` and `order` shape the result.
* **Relations.** Foreign keys become fields: a to-one field on the referencing table
  (`author_id` → `author`; otherwise the target table's name) and a to-many field named
  after the referencing table. Nested relations load in batches per level
  (`WHERE key IN (...)`), capped at 10,000 rows per level and 4 levels deep.
* **Reports.** `group:"a,b"` switches to aggregate mode (`group:""` aggregates the
  whole table). The braces then list group columns and aggregate fields:

  | Field | Meaning |
  |---|---|
  | `count` | rows per group |
  | `sum_C` `avg_C` `min_C` `max_C` | over numeric (min/max: any) column `C` |
  | `distinct_C` | number of distinct values of `C` |

  Equality arguments filter first, `order` may name group columns or aggregates
  (`order:"-sum_total"`), `limit`/`offset` page the groups.
  `orders(status:"paid",group:"region",order:"-sum_total"){region count sum_total}`
* `_tables{name columns{name type native}}` describes the source; `type` is the CCP type,
  `native` the database's own.
* Tables or columns whose names are not `[A-Za-z0-9_]`, and composite foreign keys, are not exposed.

## Stream servers (`ccp-stream`)

WebSocket and Socket.IO keep the connection open in the background and buffer the newest
messages (default 5,000 / 32 MB). The model queries the buffer:

| Root | Purpose |
|---|---|
| `messages(limit,wait,since,event,contains){fields}` | latest N messages; `wait:S` blocks for N *new* ones (max 30 s); `since:<_seq>` polls incrementally |
| `summary(wait,field,top,event,contains){count per_second values{value count} min max avg last}` | report on a window instead of listing it |
| `status{connected reconnects buffered total bytes uptime_s error}` | link health |

Message fields: `_seq`, `_ts`, `_event`, `_raw`, `_size`, `text` (non-JSON payloads), or any
JSON field / path of the payload. With no braces a message is `{_seq,_event,_raw}`.

## Other servers

* **redis-ccp** — `info`, `dbsize`, `keys(match,type,limit){key type ttl size}`, `get`, `mget`,
  `hash`, `hashes(match,limit){_key field...}` (many hashes as table rows), `list`, `members`, `zrange`, `stream`.
* **http-ccp** — `get(path:"/x",<query params>){json fields}`; GET only, fixed base URL, no redirects.

## MCP front end

Run without flags, a server is a standard MCP server (JSON-RPC over stdio,
newline-delimited) with two tools:

| Tool | Arguments | Purpose |
|---|---|---|
| `ccp_schema` | none | returns the schema advertisement (call once) |
| `ccp_query` | `query: string` | runs one CCP query |

Run with `--ccp`, it speaks the raw line protocol above. A server connects to its
source lazily, so a wrong password becomes a readable tool error rather than a failed start.
