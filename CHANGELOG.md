# Changelog

All notable changes to this project are documented in this file.

Format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
versioning follows [Semantic Versioning](https://semver.org/).
Git tag `v<version>` must equal `version` in `Cargo.toml` (checked in CI).

## [Unreleased]

### Changed

- Hybrid default model pins are free OpenRouter routes
  (`nvidia/nemotron-3-embed-1b:free` @2048, `nvidia/llama-nemotron-rerank-vl-1b-v2:free`).
  Free routes may train on prompts and rotate monthly; paid pins stay
  available via explicit `docsbase models use`.

### Performance

- Dense channel gains a persisted HNSW ANN tier for large projects
  (spec `docs/specs/ann-index`): near-instant reload, p95 search ~5 ms at
  75k×2048, exact fallback on any ANN failure; daemon builds in the
  background, direct CLI never builds.

## [0.1.0-alpha.4] — 2026-10-14

### Added

- Search-quality cycle (spec `docs/specs/search-quality`): golden fixture and
  metrics harness with a no-regression gate (all six golden classes at
  hit rate / nDCG 1.0), Unicode-aware chunk cap with recursive token-boundary
  split and overlap, `TOKENIZER_VERSION` plus version file with automatic
  index rebuild, single position per segment with a 40-token limit,
  Russian/English Snowball stemming (the stem replaces the surface form),
  Arabic normalization and stemming (harakat, tatweel, form folding), CJK
  bigrams, tuned boosts and long-chunk penalty. Perf evidence: Latin search
  p95 564 µs, CJK 4.9 ms (budget 200 ms), CJK index 77% of source text,
  RSS delta 64 MiB at 10k chunks (budget 128 MiB).
- macOS (Apple Silicon) support: platform seam (zombie-aware liveness via
  `proc_pidinfo`, descriptor counters, macOS system-dir guards, socket-path
  validation, case-insensitive path comparison), a `macos` CI job, release
  artifact `docsbase-<ver>-macos-aarch64.tar.gz` with ad-hoc signing and a
  `codesign -v` gate, `install.sh` (Darwin/arm64) and `docsbase update`
  (asset name + `shasum` fallback).
- `status` surfaces the last watcher drop: `last_error` plus the lost paths
  in `warnings`.
- README install and multilingual sections (EN/RU) and `AGENTS.md` guidance
  for contributors.

### Changed

- Watcher batches retry transient failures (three attempts with backoff;
  lease conflicts stay unbounded) and tantivy background merges are
  serialized via `wait_merging_threads()`, fixing a race on `.managed.json`
  (upstream tantivy#2847); indexing 1000 files 160 to 170 ms.

### Fixed

- `daemon stop` retries the stop request until the deadline: a daemon at
  connection capacity could shed the single request, making stop time out
  on a healthy daemon.
- `index` reports an actionable `index <path> is missing` message when a
  project's index directory was removed.
- `install.sh`: no more `curl: (23)` EPIPE noise while resolving the release
  tag (`grep -m1` closes the pipe early); network failures still surface via
  the actionable empty-tag message.

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

[Unreleased]: https://github.com/punkhomov/docsbase-memory-mcp/compare/v0.1.0-alpha.4...HEAD
[0.1.0-alpha.4]: https://github.com/punkhomov/docsbase-memory-mcp/releases/tag/v0.1.0-alpha.4
[0.1.0-alpha.3]: https://github.com/punkhomov/docsbase-memory-mcp/releases/tag/v0.1.0-alpha.3
[0.1.0-alpha.2]: https://github.com/punkhomov/docsbase-memory-mcp/releases/tag/v0.1.0-alpha.2
[0.1.0-alpha.1]: https://github.com/punkhomov/docsbase-memory-mcp/releases/tag/v0.1.0-alpha.1
