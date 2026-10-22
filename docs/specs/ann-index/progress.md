# Progress: ann-index

## Ledger

- 2026-10-22: Draft 0.1 created (requirements/design/tasks + this ledger).
  Initial plan only; production code, dependency and baseline are unchanged.
  Ruling: persistence is in scope from day one because AnnKey/format/atomic
  write/load lifecycle would otherwise need a rewrite. Proposed crate:
  `hnsw_rs` + `ouroboros`; `instant-distance` rejected (archived 2026-07-23,
  no persistence); `sqlite-vector-rs`/`usearch` rejected (C++/FFI, Arrow,
  static/MSRV risk). ADR-15 to be landed with T7.
  Open: OQ-2 0.3.4 vs 0.3.5 (min-publish-age), OQ-3 thresholds/caps, OQ-4
  Windows mmap cleanup, OQ-5 warm policy, OQ-6 status diagnostics, OQ-7 build
  determinism.
- Next at draft time: T1 spike (`hnsw_rs` + `ouroboros`, dump/load/mmap, MSRV, platforms).

- Task T1: complete (2026-10-22). Spike in `/tmp/opencode/ann-spike` (not
  committed; repo deps unchanged). `hnsw_rs 0.3.4` + `ouroboros 0.18.5`;
  `HnswIo` load with `#[borrows(mut io)]` and `#[not_covariant]`.
  Verify: 10k × 64 — build 5.1 s, dump 18 ms, mmap/heap load 49 ms, top-10
  identical before/after, dump 10.6 MB, `LoadedAnn: Send + Sync`. Linux
  runtime; Windows gnu/macOS aarch64 `cargo check` on MSRV 1.88 passed.
  Ruling: `self_cell` rejected (`try_new` gives `&Owner`; `load_hnsw` needs
  `&mut`, `Hnsw` invariant). 0.3.4 is the T1 spike version only; v1 version
  deferred to T3 (later resolved to 0.3.5). Ruling: SC-1 build ≤5 s was not
  supported (5.1 s at 10k × 64); 75k build budget moves to T6.
  Next: T2 `AnnStore`.

- 2026-10-22: Temporary supply-chain exception: `.cargo/config.toml` lowered
  `global-min-publish-age` to `13 days` (from 14) to allow `hnsw_rs 0.3.5`
  (published 2026-10-09). `tests/smoke.rs` and `docs/supply-chain.md` carry
  the matching dated note. No dependency/lock change yet; T3 must audit the
  `Cargo.lock` diff for fresh transitives, then revert both config and test
  to `14 days` on 2026-10-23. This is a time-boxed exception, not a new
  default.

- 2026-10-22: Review A–G applied to spec (docs-only). A: v1 target is
  `hnsw_rs 0.3.5` (0.3.4 fallback), temporary 13-day cooldown; no dump
  migration, 0.3.4 dump must be rejected by 0.3.5; ADR-15 after T6.
  B: `parallel_insert` default, background warm, exact streaming during build,
  no blocking. C: `usize`/`u64` conversion. D: measure `env_logger` binary
  size; no upstream issue. E: `set_searching_mode` + basename assert.
  F: corrected `hnswlib-rs` rationale. G: `score=1-cos` test + recall-vs-ef
  curve. T1 remains 0.3.4 spike; T3 re-spikes 0.3.5.

- 2026-10-22: Verification fixes (docs-only): FR-3 now distinguishes
  `AnnKey.format_version` from `AnnMeta.hnsw_rs` (loader checks both);
  OQ-2 marked conditionally closed with finalization in T6. Cooldown rollback
  intentionally not changed here.

- 2026-10-22: T1.5 scheduled for 2026-10-23: revert the temporary **global**
  13-day cooldown to 14 days after `hnsw_rs 0.3.5` is pinned in
  `Cargo.lock`. Reason: Cargo 1.101 nightly has no declarative per-package
  `min-publish-age` allowlist (only global/per-registry); the env override is
  all-or-nothing for a resolve. T1.5 is a required step before the final PR;
  0.3.5 itself must not be downgraded by the revert.
