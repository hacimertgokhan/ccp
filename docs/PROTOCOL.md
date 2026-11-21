# CCP protocol

CCP — the **Core Context Protocol** — lets a client ask for exactly the context
it needs in one request and get it back in a compact form. This document is the
reference for protocol version 0.2.

A CCP server speaks over **stdio, one line in, one line out**. (The database
servers in this repository additionally speak MCP by default; see
[MCP front end](#mcp-front-end).)

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
name       = letter-or-digit-or-underscore, one or more
```

* Selections are separated by whitespace; commas are optional between arguments.
* The braces choose which fields to return. A field with braces is a nested object or list.
* Several root selections in one query run in order and return in one object.
* Strings are delimited by `"` and cannot contain a `"` (no escapes yet).
* Nesting is limited to 64 levels. Unknown characters are skipped.

Examples:

```text
users{id name}
users(id:7){name posts{title comments{body}}}
users(city:"Tokyo",limit:5) posts(author_id:7){title}
```

## Result encoding

```text
value  = null | true | false | integer | "string" | [ value, ... ] | { key:value, ... }
```

* Object keys are bare (unquoted); strings escape `\`, `"`, newline and carriage return.
* A root selection that returns a table is a list; a to-one relation is an object or `null`.
* A field the schema does not know resolves to `null`.
* A failure inside one root selection is reported in place as `{error:"..."}`;
  the rest of the query still returns. A malformed query returns `ERR <reason>`.

## Schema advertisement

```text
root users(limit:Int,offset:Int,id:Int,name:Str):[Users]
type Users{id:Int,name:Str,posts:[Posts]}
```

* `root name(args):Type` declares a root field and its accepted arguments.
* `type Name{field:Type,...}` declares an object type.
* Scalar types are `Int`, `Str`, `Bool`; `[T]` is a list; any other name is an object type.

## Database servers (`ccp-sql`)

The SQL servers map the protocol onto a database catalog:

* Every table is a root field named after the table, returning `[Table]`.
* Arguments that name a column filter by equality (several are ANDed);
  `limit` (default 100, max 1000) and `offset` page the result.
* Foreign keys become relation fields: a to-one field on the referencing
  table (`author_id` → `author`; otherwise the target table name) and a to-many
  field named after the referencing table on the referenced one.
* Nested relations load in batches per level (`WHERE key IN (...)`), capped at
  10,000 rows per level and 4 levels of nesting.
* `_tables{name columns{name type native}}` describes the database; `type` is the
  CCP type, `native` the database's own.
* Tables or columns whose names are not `[A-Za-z0-9_]`, and composite foreign
  keys, are not exposed.

## MCP front end

Run without flags, a database server is a standard MCP server (JSON-RPC over
stdio, newline-delimited) exposing two tools:

| Tool | Arguments | Purpose |
|---|---|---|
| `ccp_schema` | none | returns the schema advertisement (call once) |
| `ccp_query` | `query: string` | runs one CCP query |

Run with `--ccp`, it speaks the raw line protocol above.
