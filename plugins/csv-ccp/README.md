# csv-ccp

CCP server for **CSV / TSV files**, built for reporting. Files are too big for a context
window; reports are not. Each file becomes a table, loaded once into memory, and
filtering, ordering, grouping and aggregation run inside the server. Only the small
answer reaches the model.

```bash
cargo install --git https://github.com/hacimertgokhan/ccp csv-ccp

claude mcp add sales -e CCP_PATH=/data/sales.csv -- csv-ccp        # a file…
claude mcp add data  -e CCP_PATH=/data/reports/   -- csv-ccp        # …or a folder of .csv/.tsv
```

```text
sales(group:"region",order:"-sum_price"){region count sum_price avg_price}
sales(group:"region,channel"){region channel count sum_qty}
sales(group:""){count distinct_customer max_price}
sales(region:"APAC",order:"-price",limit:3){order_id price}
```

| Variable | Required | Notes |
|---|---|---|
| `CCP_PATH` | yes | a file, or a directory of `.csv`/`.tsv` files (table = file name) |
| `CCP_DELIMITER` | no | single character or `tab`; default `,` (`.tsv` → tab) |
| `CCP_MAX_MB` | no | refuse to load more than this (default 256) |

Header cells are made addressable (`Total $` → `Total__`, duplicates get `_2`), columns are typed
`Int` / `Float` / `Str` by inspecting every row, and empty numeric cells become `null` so they never
poison a sum. Quoted fields, escaped quotes, CRLF and newlines inside fields are supported.
The file is read-only input; nothing is written.
