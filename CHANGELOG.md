# Changelog

All notable changes to this project are documented in this file.

Format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
versioning follows [Semantic Versioning](https://semver.org/).
Git tag `v<version>` must equal `version` in `Cargo.toml` (checked in CI).

## [Unreleased]

### Added
- `search_docs` accepts `explain: true` and returns `{hits, meta}` instead of the
  bare citation array: `meta` names the score scale (`bm25`/`rrf`/`rerank`), the
  stages that ran, per-stage timings, the candidate window, the index generation
  and any degradation reasons. Without the flag the response is unchanged.
- `status` reports `index_generation` and `ann` (`state`, `vectors`,
  `estimated_bytes`, `reason`) per project, so a silent fallback from the ANN tier
  to exact search is visible instead of living only in the daemon log.
- `docsbase search --explain` prints the same envelope as the MCP tool.

## [0.1.0-alpha.5] — 2026-10-30

### Fixed

- ANN publish/release ordering: a background build published the index and
  only then re-opened the vector cache to release its backfill lease, so a purge
  (`models remove`, cache clear) racing the tail of a build hit a Windows
  sharing violation on `vectors.db` (Unix unlinks the open file, so CI only ever
  failed on Windows). The build now releases the lease before the index becomes
  visible, so "ANN visible" implies no open `vectors.db` handle.
- `docsbase update` recorded the *calling* binary's `build_id` in
  `install.json` and echoed it on the `installed …` line, so an update from an
  older binary described the old build. The manifest now describes the installed
  artifact: the released version on `update`, and the artifact's own
  `--version` on `update --from` (`docsbase unknown` if it cannot answer).
  `docsbase install` is unchanged — there the caller is the installed file.
  ([#39](https://github.com/punkhomov/docsbase-memory-mcp/issues/39))


### Added

- Local hybrid provider `hybrid-local` (spec `docs/specs/hybrid-local`,
  ADR-17): offline fastembed embeddings + rerank behind the `hybrid-local`
  build feature. Adds `docsbase connect fastembed` (pinned ONNX Runtime
  1.24.4 install with sha256 verification, `--check`/`--force`), `docsbase
  models fetch|list` (pinned HF revisions, per-file sha256, atomic install,
  size caps), and purpose-first `docsbase models use embed|rerank <model>`
  (project/global scope matrix, provider inference from the registry,
  offline probes). Canonical local model id
  `fastembed/<name>@d<dim>@<l2|raw>@r<rev8>@p<prompt_version>`. `docsbase
  models remove <name>` removes all cached revisions (refuses while a model
  is loaded; `--force` deletes anyway). `docsbase uninstall --dry-run`
  lists the ONNX Runtime install and model cache before any deletion. The
  default build stays fully offline (NFR-1); no weights or runtime are
  shipped in the binary.

### Changed

- **Breaking (CLI):** every model is now addressed as `<provider>/<model>` in
  `docsbase models use|fetch|remove` — a bare name is rejected with the corrected
  command, and `--provider` is gone (`models fetch bge-small-en-v1.5` becomes
  `models fetch fastembed/bge-small-en-v1.5`). Config files are unaffected: the
  address lives only on the CLI boundary and `provider`/`model` stay separate
  keys, so cache keys and vector stores remain valid.
- Hybrid providers are a registry, not `match` branches (spec
  `docs/specs/provider-agnostic`, ADR-18): stage resolution, status, `models
  list` and `connect` are provider-agnostic, `[providers.<name>]` records are
  generic TOML validated by the provider itself, and adding a provider needs one
  module plus one registration row. `docsbase connect <provider> [--check]
  [--force]` now has the same flags for every provider (`connect openrouter`
  gained `--check`/`--force`).
- CI lanes rebalanced (`docs/ci.md`): the combo feature build is now the single
  runtime lane for provider features, the feature axis is covered by
  `cargo hack --feature-powerset --depth 2`, macOS/Windows additionally execute
  the hermetic `--features hybrid-local --lib` suite, and the release ANN
  (SC-4) check moved to `nightly`. Offline latency budgets are host-relative
  (`baseline_p95 × 1.5 × DOCSBASE_BENCH_LATENCY_SLACK + 5 ms`, CI factor 3).
- Hybrid default model pins are free OpenRouter routes
  (`nvidia/nemotron-3-embed-1b:free` @2048, `nvidia/llama-nemotron-rerank-vl-1b-v2:free`).
  Free routes may train on prompts and rotate monthly; paid pins stay
  available via explicit `docsbase models use`.

### Fixed

- Windows/macOS CI portability of the `hybrid-local` code: the macOS signature
  helper is gated on the feature (dead-code lint in the default build),
  `OsStrExt::encode_wide` replaces the non-existent `OsStr::encode_utf16`, and
  the Win32 `GetDiskFreeSpaceExW` call passes a raw pointer (clippy
  `borrow_as_ptr`). `config_runtime::watcher_uses_reopened_config` gets a
  platform deadline (60 s on Windows) instead of a fixed 15 s.
- ANN tier (spec `docs/specs/ann-index`, Delta 0.3): release builds no longer
  use `panic = "abort"`, which had silently disabled the corrupt-dump → exact
  fallback and aborted the daemon instead (compile-time guard added, release
  CI test added; binary +1.1 MiB). `DenseCache.max_bytes` now counts the exact
  rows a background build keeps alive, an over-cap build stays exact instead of
  exceeding the resident cap, backfill leases are only taken when an ANN build
  can follow (including the resident-cap admission rule), releasing a lease
  evicts, and a cache that is over budget while every row is leased warns once
  instead of on every write. The default resident cap is 1.5 GiB so the
  documented 75k×2048 corpus (1.22 GiB build peak) is still admitted.
  Backfill leases follow the chunk-row count the build admits (duplicate texts
  no longer pin a corpus that can never build), a second background build is
  not admitted while the first occupies the cap, and publish releases the
  source rows and the built graph before loading the copy it publishes. `search_docs` logs
  ANN search errors separately from "insufficient candidates", and the daemon
  no longer warms a persisted ANN for projects whose ranking is lexical.

### Performance

- Dense channel gains a persisted HNSW ANN tier for large projects
  (spec `docs/specs/ann-index`): exact fallback on any ANN failure; daemon
  builds in the background, direct CLI never builds. Core `AnnIndex`
  benchmark at 75k×2048: p95 ~5 ms on synthetic separated clusters; the
  end-to-end `DenseCache`/`VectorCache`/`search_docs` path is covered by an
  8k×256 CI run and a manual 75k×2048 run (ann-index T10: p50 1.46 ms, p95
  2.88 ms over 20 searches, peak process RSS 1995.6 MiB including the
  in-process build). The 1.5 GiB resident cap is an accounting model
  (graph 691 MB + exact rows 586 MB), not an RSS clamp.

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

[Unreleased]: https://github.com/punkhomov/docsbase-memory-mcp/compare/v0.1.0-alpha.5...HEAD
[0.1.0-alpha.5]: https://github.com/punkhomov/docsbase-memory-mcp/releases/tag/v0.1.0-alpha.5
[0.1.0-alpha.4]: https://github.com/punkhomov/docsbase-memory-mcp/releases/tag/v0.1.0-alpha.4
[0.1.0-alpha.3]: https://github.com/punkhomov/docsbase-memory-mcp/releases/tag/v0.1.0-alpha.3
[0.1.0-alpha.2]: https://github.com/punkhomov/docsbase-memory-mcp/releases/tag/v0.1.0-alpha.2
[0.1.0-alpha.1]: https://github.com/punkhomov/docsbase-memory-mcp/releases/tag/v0.1.0-alpha.1
