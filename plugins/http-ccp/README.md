# http-ccp

CCP server for **REST / JSON APIs**. The API answers with its full JSON; the server keeps only the
fields the query names. A 2.5 KB user object becomes a 40-byte answer.

```bash
cargo install --git https://github.com/hacimertgokhan/ccp http-ccp

claude mcp add github \
  -e CCP_URL=https://api.github.com \
  -e CCP_BEARER="$GITHUB_TOKEN" \
  -e CCP_HEADERS='Accept: application/vnd.github+json; X-GitHub-Api-Version: 2022-11-28' \
  -- http-ccp
```

```text
get(path:"/repos/rust-lang/rust"){full_name stargazers_count owner.login license.name}
get(path:"/repos/rust-lang/rust/issues",state:"open",per_page:5){number title user.login}
```

Extra arguments become query parameters (percent-encoded). Arrays are projected element by element;
dotted paths reach into nested objects; non-JSON responses come back as text.

| Variable | Required | Notes |
|---|---|---|
| `CCP_URL` | yes | base URL, `http(s)://…` |
| `CCP_BEARER` | no | `Authorization: Bearer …` |
| `CCP_HEADERS` | no | extra headers, `Name: value` separated by `;` or newlines |
| `CCP_TIMEOUT` | no | seconds (default 15) |
| `CCP_MAX_KB` | no | response size cap (default 4096) |

Safety: **GET only**. `path` is joined to the fixed base URL: no scheme, host, `//`, `\`, `#` or `..`
is accepted, and redirects are never followed, so the model cannot steer a request (or your token)
to another host. Secrets are scrubbed from errors.
