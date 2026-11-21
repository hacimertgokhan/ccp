# Contributing to CCP

Thank you for helping. CCP — the Core Context Protocol — stays useful by staying
small, so the bar is: **does it give a model the context it needs, with fewer
bytes and fewer calls?**

## Ways to help

| You want to… | Do this |
|---|---|
| Report a bug | [Open a bug report](https://github.com/hacimertgokhan/ccp/issues/new?template=bug_report.yml) with the query, the server and the output |
| Suggest a feature | [Open a feature request](https://github.com/hacimertgokhan/ccp/issues/new?template=feature_request.yml): describe the question you could not ask |
| Add a database or data source | Read [Writing a CCP server](docs/writing-a-server.md), then [propose it](https://github.com/hacimertgokhan/ccp/issues/new?template=new_server.yml) so we agree on scope first |
| Improve the protocol | Open a feature request first; protocol changes need an update to [docs/PROTOCOL.md](docs/PROTOCOL.md) |
| Improve docs or benchmarks | Pull requests welcome, no issue needed |
| Report a vulnerability | **Do not open an issue**: see [SECURITY.md](SECURITY.md) |

Look for issues labelled `good first issue` and `help wanted`.

## Setup

```bash
git clone https://github.com/hacimertgokhan/ccp && cd ccp
cargo build --workspace
cargo test --workspace
```

Requirements: a recent stable Rust toolchain. Docker is only needed for the
benchmark (`python3 bench/bench.py`: MariaDB and PostgreSQL containers).

## Layout

| Path | What lives there |
|---|---|
| `crates/ccp-proto` | the protocol (parser, executor, encoding). Keep it dependency-free. |
| `crates/ccp-sql` | shared engine for database servers |
| `crates/ccp` | demo CLI |
| `plugins/*-ccp` | one thin crate per database; all servers live here |
| `docs/`, `bench/` | specification, guides, reproducible benchmark |

## Before you open a pull request

```bash
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

* One focused change per pull request; explain **why** in the description.
* Add tests: unit tests for logic, an in-memory test like `plugins/sqlite-ccp` for query behaviour.
* Update the docs and `CHANGELOG.md` (under *Unreleased*) when behaviour changes.
* If your change touches results, run `python3 bench/bench.py`: the three databases must still
  return identical output (the parity line), and byte counts should not regress.

## Rules that are not negotiable

These keep CCP servers safe to point at real data:

1. **Read-only.** No server may write.
2. **No caller text in SQL.** Identifiers come from the catalog, values are bound parameters.
3. **No secrets in output.** Scrub credentials from errors; never log them.
4. **Environment-based configuration.** Passwords never go on a command line or into a file the project writes.

## Commit style

Short imperative subject (`Add redis-ccp server`), body explains the motivation.
Releases follow [Semantic Versioning](https://semver.org); the protocol version is
documented in [docs/PROTOCOL.md](docs/PROTOCOL.md).

## Code of conduct

Be kind and constructive. See [CODE_OF_CONDUCT.md](CODE_OF_CONDUCT.md).
