# Changelog

All notable changes are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com) and the project uses [Semantic Versioning](https://semver.org).

## [Unreleased]

## [0.2.0] - 2026-11-21

### Added
- `postgres-ccp` and `sqlite-ccp` servers next to `mysql-ccp`.
- `ccp-sql`: shared engine (catalog introspection, foreign-key relations, safe SQL generation, MCP front end).
- Nested relations in both directions, loaded in batches (`users{posts{comments{...}}}`).
- `_tables` introspection root with portable `type` and database-specific `native`.
- Reproducible benchmark (`bench/`) with cross-database parity check, token and session estimates.
- Documentation: protocol reference, server-writing guide, contributing and security policies.

### Changed
- Servers are standalone MCP servers configured through `CCP_*` environment variables.
- Workspace split into `ccp-proto`, `ccp-sql`, `ccp` and `plugins/*`.

### Removed
- The connection registry and plugin host in the `ccp` binary (`ccp db`, `ccp plugin`).

## [0.1.0]
- Initial protocol core and demo.
