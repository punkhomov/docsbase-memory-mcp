# Changelog

All notable changes to this project are documented in this file.

Format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
versioning follows [Semantic Versioning](https://semver.org/).
Git tag `v<version>` must equal `version` in `Cargo.toml` (checked in CI).

## [Unreleased]

## [0.1.0-alpha.3] — 2026-10-05

### Added

- `DOCSBASE_CHANNEL=prerelease` opt-in for `install.sh` / `install.ps1`;
  `stable` is the default channel, and prerelease-only repositories now fail
  with an actionable hint instead of a raw 404.

### Fixed

- `uninstall` now also removes the `PATH` launcher copy recorded at install
  time and leaves no empty data directories; a tampered manifest cannot point
  the launcher removal at a foreign file (byte-equality check).
- Document the `status` MCP tool (9 tools total) in `docs/MCP.md` and both
  READMEs.
- Release pipeline no longer attaches the stray unversioned `sbom-action`
  artifact; only the versioned SBOM pair ships.

## [0.1.0-alpha.2] — 2026-10-05

### Added

- `docsbase update` — fetch the latest GitHub release, verify its sha256,
  then atomically replace both the owned copy and the invoking binary (FR-5).
  Flags: `--check`, `--force`, `--version TAG`, `--from PATH`.

### Changed

- `install.json`/binary swap is now safe when the target is the running
  executable (Windows self-update moves the live image aside first).
- Dependencies: tantivy 0.25 → 0.26 (TopDocs builder API), rmcp 3.3 → 3.4
  (`ServerInfo` renamed to `ServerConfig`), clap 4.6.7, ignore 0.4.33.
- Release pipeline: action bumps (checkout v7, download-artifact v8,
  attest-build-provenance v4, action-gh-release v3) and SBOM staging fix.

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

[Unreleased]: https://github.com/punkhomov/docsbase-memory-mcp/compare/v0.1.0-alpha.3...HEAD
[0.1.0-alpha.3]: https://github.com/punkhomov/docsbase-memory-mcp/releases/tag/v0.1.0-alpha.3
[0.1.0-alpha.2]: https://github.com/punkhomov/docsbase-memory-mcp/releases/tag/v0.1.0-alpha.2
[0.1.0-alpha.1]: https://github.com/punkhomov/docsbase-memory-mcp/releases/tag/v0.1.0-alpha.1
