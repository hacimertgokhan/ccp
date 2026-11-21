## What and why

<!-- What does this change, and what problem does it solve? -->

## Checklist

- [ ] `cargo fmt --all`, `cargo clippy --workspace --all-targets -- -D warnings` and `cargo test --workspace` pass
- [ ] Tests added or updated
- [ ] Docs / `CHANGELOG.md` updated when behaviour changes
- [ ] Servers stay read-only, bind every value and take identifiers only from the catalog
- [ ] If results changed: `python3 bench/bench.py` still reports identical output on all databases
