# Changelog

All notable changes to this project are documented in this file.

Format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
versioning follows [Semantic Versioning](https://semver.org/).
Git tag `v<version>` must equal `version` in `Cargo.toml` (checked in CI).

## [Unreleased]

## [0.1.0-alpha.1] — 2026-10-04

First public alpha. Expect breaking changes before `0.1.0`.

### Added

- Single `docsbase` binary with three roles: CLI, stdio MCP frontend,
  per-account daemon (local socket transport, no network in runtime).
- CLI: `index`, `search`, `list`, `sync`, `config`, `status`,
  `serve`, `mcp`, `daemon stop`, `install` / `uninstall --yes`.
- MCP tools (read-only over indexed docs): `search_docs`, `get_doc`,
  `read_neighbors`, `list_docs`, `list_projects`, `index_project`,
  `sync_start`, `sync_status`.
- Per-project Markdown indexing: walk with ignores, YAML frontmatter,
  heading chunking, tantivy FTS with identifier tokenizer, SQLite registry.
- Watcher with debounce, incremental re-index, crash-safe single-writer store.
- Platform seam `src/platform/`: Linux (Unix socket) and Windows x64
  (named pipe via `interprocess`) backends; macOS out of scope for v1.
- Linux x86_64 static artifact (`+crt-static`, 40 MiB budget); Windows x64 `.exe`.
- Install scripts `install.sh` / `install.ps1` and `docsbase install` owned
  manifest (`install.json`).
- Supply-chain: `Cargo.lock` committed, `--locked` CI builds,
  `min-publish-age = 14 days`, GitHub Releases with sha256, SBOM
  and SLSA build provenance.

[Unreleased]: https://github.com/punkhomov/docsbase-memory-mcp/compare/v0.1.0-alpha.1...HEAD
[0.1.0-alpha.1]: https://github.com/punkhomov/docsbase-memory-mcp/releases/tag/v0.1.0-alpha.1
