# Progress: ann-index

## Ledger

- 2026-10-22: Draft 0.1 created (requirements/design/tasks + this ledger).
  Initial plan only; production code, dependency and baseline are unchanged.
  Ruling: persistence is in scope from day one because AnnKey/format/atomic
  write/load lifecycle would otherwise need a rewrite. Proposed crate:
  `hnsw_rs` + `ouroboros`; `instant-distance` rejected (archived 2026-07-23,
  no persistence); `sqlite-vector-rs`/`usearch` rejected (C++/FFI, Arrow,
  static/MSRV risk). ADR-15 recorded in product design (T7 finalizes version/format).
  Open: OQ-2 cooldown/version, OQ-3 thresholds/caps, OQ-4
  Windows mmap cleanup, OQ-5 warm policy, OQ-6 status diagnostics, OQ-7 build
  determinism.
- Next at draft time: T1 spike (`hnsw_rs` + `ouroboros`, dump/load/mmap, MSRV, platforms).

- Task T1: complete (2026-10-22). Spike in `/tmp/opencode/ann-spike` (not
  committed; repo deps unchanged). `hnsw_rs 0.3.5` + `ouroboros 0.18.5`;
  `HnswIo` load with `#[borrows(mut io)]` and `#[not_covariant]`.
  Verify: 10k × 64 — build 4.8 s, dump 15 ms, mmap/heap load 43/42 ms, top-10
  identical before/after, dump 10.3 MB, `LoadedAnn: Send + Sync`. Linux
  runtime; Windows gnu/macOS aarch64 `cargo check` on MSRV 1.88 passed.
  Ruling: `self_cell` rejected (`try_new` gives `&Owner`; `load_hnsw` needs
  `&mut`, `Hnsw` invariant). T1 spike is on 0.3.5; v1 version is 0.3.5. Ruling: SC-1 build ≤5 s was not
  supported (4.8 s at 10k × 64); 75k build budget moves to T6.
  Next: T2 `AnnStore`.

- 2026-10-22: Temporary supply-chain exception: `.cargo/config.toml` lowered
  `global-min-publish-age` to `13 days` (from 14) to allow `hnsw_rs 0.3.5`
  (published 2026-10-09). `tests/smoke.rs` and `docs/supply-chain.md` carry
  the matching dated note. No dependency/lock change yet; T3 must audit the
  `Cargo.lock` diff for fresh transitives, then revert both config and test
  to `14 days` on 2026-10-23. This is a time-boxed exception, not a new
  default.

- 2026-10-22: Review A–G applied to spec (docs-only). A: v1 target is
  `hnsw_rs 0.3.5`, temporary 13-day cooldown; no dump
  migration, dump чужой версии must be rejected; ADR-15 recorded; version/format finalization in T6.
  B: `parallel_insert` default, background warm, exact streaming during build,
  no blocking. C: `usize`/`u64` conversion. D: measure `env_logger` binary
  size; no upstream issue. E: `set_searching_mode` + basename assert.
  F: corrected `hnswlib-rs` rationale. G: `score=1-cos` test + recall-vs-ef
  curve. T1 spike is on 0.3.5.

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

- 2026-10-22: ADR-15 recorded in `docsbase-memory-mcp/design.md`;
  `hnsw_rs 0.3.5` is the only v1 ANN version, with the temporary global
  13-day cooldown and T1.5 revert. Spec tightened: DenseCache digest is cached
  per generation, background `Building`/`warm` state added, T2 persistence is
  split from HNSW build, stale `usearch`/ANN-later references removed.

- Task T2: complete (commits 895c5fd..8b5f708; tests: `cargo test --locked
  --test ann_store` → 12/12 pass; `cargo clippy --locked --all-targets -- -D
  warnings` → clean; `cargo fmt --check` → clean). Fresh reviewer: spec
  compliance PASS, quality APPROVED. Ruling: T1.5 deferred by human (2026-10-22);
  13-day cooldown stays in effect until the human schedules the revert.
  Minor (deferred): persist_dump removes existing final dir before rename
  (not an atomic swap; Windows rename-over limitation; crash window only ever
  yields exact fallback). Minor (deferred): no directory fsync after rename
  (cache semantics; NFR-5 satisfied via READY ordering). Minor (deferred):
  persist_dump does not pre-validate dump file presence/size (load rejects
  later with fallback). Minor (deferred): tests don't assert `format_version`/
  `dim` mismatch explicitly (matches_key covers all fields). Minor (deferred):
  `HNNSW_RS_VERSION` double-N name cosmetic.

- Task T3: complete (commits 90b8774..1097da4, incl. temporary 13-day
  cooldown commit 77b50ae; tests: `cargo test --locked --lib` → 94/94; `--test
  ann_recall` → 9/9; `--test ann_store` → 12/12; `--test smoke` → 5/5;
  clippy `--all-targets -- -D warnings` → clean; fmt → clean). Fresh reviewer:
  spec compliance PASS, quality CHANGES → one Important (tests/support/mod.rs
  `pub mod ann;` not committed) fixed and re-reviewed ADDRESSED, no new
  breakage. Ruling: hnsw_rs 0.3.5 exposes no seed parameter, so the "double
  single-thread build with fixed seed" check cannot be asserted at build time;
  double-build determinism is observed in T6 (OQ-7) instead. Ruling: lock diff
  audited — hnsw_rs 0.3.5 (2026-10-09) passes the temporary 13-day cooldown;
  remaining new transitives are older; no fresh publish besides the pinned
  version. Minor (deferred): `build` runs `parallel_insert_slice`/
  `set_searching_mode` outside `run_unwind` (panics there would escape; low
  likelihood, validation covers realistic cases). Minor (deferred): mmap load
  error is swallowed when the heap retry succeeds (diagnostics only). Minor
  (deferred): `ouroboros = "0.18.5"` caret pin vs exact `hnsw_rs` (lockfile
  pins anyway; align on exact pin in T7 if desired). Recall-vs-ef measured:
  2k×32 random → recall@10 0.994/0.999/1.000/1.000/1.000 at ef 16/32/64/128/256.
  Next: T4 `DenseCache` integration.

- Task T4: complete (commits 3c77d12..7e7eb52; tests: `cargo test --locked
  --lib` → 94/94; `--test hybrid_ann` → 9/9; `--test hybrid_rank_ann` →
  1/1 (4 consecutive runs); `--test hybrid_ranking` → 10/10; `--test
  ann_store` → 12/12; `--test ann_recall` → 9/9; `--test smoke` → 5/5;
  clippy `--all-targets -- -D warnings` → clean; fmt → clean). Fresh review:
  round 1 — 3 Important (expect/unreachable in src; zero coverage of
  `dense_candidates` Ann/Building arms) + minors; fixed in rounds 1–2 and
  re-reviewed (ADDRESSED except deferred minors). Ruling: `DenseCache::new()`
  stays ANN-disabled; daemon/CLI opt in via `with_policy` (T5). Ruling:
  `DenseCacheLimits.min_vectors` defaults to `ANN_MIN_VECTORS` and is used by
  tests to exercise the ANN path at small N (not a public contract). Ruling:
  search-level ANN test asserts per-run determinism and non-blocking
  correctness instead of exact-vs-ANN top-k equality (recall is asserted in
  `ann_recall`; HNSW boundary ties on near-equal vectors are nondeterministic).
  Minor (deferred): after a failed background build the entry state stays
  `Building` with `failed_until` set; `get` retries after backoff
  (`expired_building`), so behavior is correct while diagnostics stay
  "loading" until retry. Minor (deferred): digest-reuse and "no rows+Ann
  double memory" are implemented but not directly observable from outside.
  Next: T5 daemon/CLI policy + warm/invalidate.

- Task T5: complete (commits ef659bb..a83d150; tests: `cargo test --locked
  --lib` → 95/95 (incl. daemon E2E `sync_refresh_builds_ann_in_background`);
  `--test hybrid_ann` → 11/11; `--test hybrid_rank_ann` → 1/1; `--test
  hybrid_ranking` → 10/10; `--test cli_routing` → 16/16; `--test cli_read`
  → 4/4; `--test smoke` → 5/5; clippy/fmt clean). Fresh review: round 1 —
  Critical (uncommitted `snapshot` visibility fix) + Important (daemon hook
  untested) + minors; fixed and re-reviewed (ADDRESSED except deferred).
  Ruling: `refresh_ann_after_sync` is warm-only — invalidation is not needed
  because stale on-disk indexes are rejected by the AnnKey
  (generation/digest/format), and `warm` is idempotent, so a no-op sync
  keeps a valid ANN (FR-4 satisfied via key matching; avoids rebuild churn).
  Ruling: daemon E2E test uses a 5000-chunk project so the real
  `ANN_MIN_VECTORS` threshold applies; fake provider name is rejected by
  `from_config` but the hook only uses `cache_model_id` from config, so no
  network/feature is needed. Ruling: FR-9 diagnostics are log-only
  (`eprintln` warnings on build/search failure); public `status` contract
  untouched. Minor (deferred): the background warm uses `std::thread::spawn`
  rather than tokio `spawn_blocking` (call sites include both runtime and
  non-runtime contexts; bounded by one build per key). Minor (deferred):
  E2E test hardcodes `projects/1/ann` and a 10s poll (fails loudly, not
  silently). Next: T6 scale/recall/golden verification.

- Task T6: complete (2026-10-22). Verify commands and results recorded fresh
  this session: `cargo test --locked --test search_bench` → 68/68 pass incl.
  `metrics_match_baseline` (SC-7: golden baseline unchanged, ANN inactive at
  331 chunks < `ANN_MIN_VECTORS`). `cargo test --locked --release --test
  ann_spike -- --ignored` (75k × 2048, release): data gen 0.39 s; build
  (parallel insert) 56.5 s; dump 1.29 s; mmap load 549 ms (no rebuild,
  SC-3/NFR-3); search p50 3.84 ms / p95 5.40 ms over 100 queries
  (NFR-1 ≤10 ms met); recall@10 on separated clusters 1.0000 (NFR-2 ≥0.95
  met); estimated resident 624 MiB (NFR-4 ≤1 GiB met). `cargo build --release`
  binary 12 MiB (NFR-9: 40 MiB budget met; delta from pre-ANN release not
  archived, budget-checked forward). Ruling: 75k build budget is fixed at
  observed ~56 s (release) — SC-1's "5 s" was provisional for 10k × 64; a
  sub-minute background build is acceptable for daemon warm (non-blocking).
  Ruling: hnsw_rs 0.3.5 exposes no seed API, so double-build byte determinism
  cannot be asserted; persisted-index query results are deterministic (repeat
  equality in `hybrid_rank_ann`), parallel-insert graph nondeterminism is
  acceptable (OQ-7). Ruling: `ANN_MIN_VECTORS=4096`, `max_bytes=1 GiB`,
  `max_projects=8`, `ef_search=64` stay provisional-final (no calibration
  change needed: recall 0.994/0.999/1.0 at ef 16/32/64 on 2k×32; p95 5.4 ms
  at 75k×2048). Next: T7 ADR-15 finalization + docs + ledger.

- Task T7: complete (2026-10-22). ADR-15 finalized in
  `docs/specs/docsbase-memory-mcp/design.md`: `hnsw_rs 0.3.5` +
  `ouroboros 0.18.5` — единственные версии v1; format v1
  (`meta.json`/`READY`/atomic rename); scale numbers recorded (release
  75k×2048: build 56.5 s, p95 5.4 ms, recall 1.0000, mmap load 549 ms,
  binary 12 MiB ≤ 40 MiB budget). `CHANGELOG.md` gets a Performance entry.
  `tasks.md` status header updated (T1–T7 complete; T1.5 human-deferred).
  No stale "ANN later" statements remain (grep checked). Docs-only changes;
  no code touched. Next: final whole-branch convergence review.
