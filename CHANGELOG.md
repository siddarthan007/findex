# Changelog

All notable changes to this project are documented here. Versions follow
[Semantic Versioning](https://semver.org/).

## [3.1.5] - 2026-07-17

### Fixed
- **Tauri 2 ACL regression that blocked every frontend command.** The
  `permissions/allow-all.toml` file used a bare top-level `allow = [...]` key,
  which is not a field on Tauri's `Permission` struct (see
  `tauri-utils/src/acl/mod.rs`). Because `commands` is `#[serde(default)]`,
  the missing `commands.allow` silently defaulted to empty, so the
  `allow-all-commands` permission whitelisted **zero** commands. Every
  `invoke()` from the webview was rejected at runtime with errors like
  `Command select_directory not allowed by ACL` and
  `Command auth_login not allowed by ACL`. The list is now correctly nested
  under `commands.allow` and audited against all 23 registered commands.
- `capabilities/default.json` now uses the bare permission identifier
  (`allow-all-commands`) required for app-defined permissions. The dotted
  `dev.findex.desktop:` prefix form is rejected by Tauri's identifier
  validator (only lowercase ASCII, hyphens, and a single colon are allowed).

### Changed
- Bumped `findex-core`, `findex-cli`, and `findex-tauri` crates from 3.1.1 to
  3.1.5. The `tauri.conf.json` and `package.json` versions now match.

### Verified
- `cargo check -p findex-tauri` validates the ACL files at compile time.
- `cargo build -p findex-tauri` produces a working binary.
- `cargo test --workspace` — 112 passed, 0 failed.

## [3.1.1] - 2026-07-16

Initial public desktop packaging with Tauri 2, deep-link, and updater plugins.

## [3.1.0] - 2026-07-15

Retrieval MVP with hybrid search, PageRank repo map, and MCP server.
