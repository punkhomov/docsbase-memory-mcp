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
  `HNNSW_RS_VERSION` double-N name cosmetic — fixed in the human review pass
  (renamed `HNSW_RS_VERSION`).

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
  met); estimated resident ~691 MiB (NFR-4 ≤1 GiB met; overhead estimate
  1024 B/vertex, calibrated from the T1 spike dump ~780 B/vertex). `cargo build --release`
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

- Final convergence review (2026-10-22): fresh final reviewer over `d9ac518..HEAD` —
  verdict **converged**; one fix pass applied and re-reviewed (all 4 Important
  findings ADDRESSED, no new breakage): tie-test de-flake (distinct-but-tied
  vectors), rejected-ANN diagnostics (`reject` eprintln in `AnnStore::load`),
  daemon `cleanup` wiring (post-sync warm thread), digest LRU prune on
  eviction. Minor (deferred) from final review: digest cache retains older
  generations per resident project (bounded by project count × generations;
  fully bounded maintenance deferred); insufficient-candidates warning prints
  per-search when `fetch > N`; `fetch > corpus` always falls back to exact
  (correct, suboptimal); transient build-phase RSS (rows + building HNSW)
  not counted against the 1 GiB budget (steady state within budget). Merge
  gate (human): T1.5 revert of the 13-day cooldown — deferred by human, not
  committed; branch intentionally carries the temporary exception
  (commit 77b50ae).

- 2026-10-22 human review of T2–T7: verdict accept, no blockers. Four
  minors addressed in this pass: (1) direct digest-reuse test added
  (`hybrid::dense::tests::digest_is_cached_per_project_generation`, lib
  96/96); (2) silent `AnnIndex::load` graph failure now logs in `resolve`
  (`warning: ann load failed ...; falling back to exact`, completes FR-9
  diagnostics); (3) `HNNSW_RS_VERSION` renamed to `HNSW_RS_VERSION`;
  (4) `GRAPH_OVERHEAD_BYTES` 128 → 1024 (spike-measured ~780 B/vertex;
  75k×2048 estimate 691 MiB, still under the 1 GiB cap). No behavior
  changes affecting search or baseline.

- 2026-10-22: Delta 0.2 (post-T7 independent review). Opened T8–T11 for
  end-to-end gaps: (a) `VectorCache` 256 MiB vs 75k×2048 ~586 MiB → coverage
  never reaches 100% (cross-ref hybrid-providers DELTA-9/T13); (b)
  `DenseCache.max_bytes` does not cap exact rows; (c) cold start reads all
  vectors before persisted ANN; (d) failed build never retries because
  `start_build` reuses `Building`; (e) Windows `sync_all` uses read-only
  handles; (f) cleanup deletes active `tmp-*`; (g) fixed `ef=64` vs
  `max(64, fetch*4)`; (h) T6 numbers are core-`AnnIndex`, not end-to-end.
  Requirements/design/tasks updated with dated deltas and open OQ-4 until
  Windows runtime. No code changes in this entry; T1.5 remains merge gate.

- Task T8: complete (2026-10-22). `DenseCache.max_bytes` is now a global
  resident cap (byte LRU across entries, exact rows included); a single
  over-cap project becomes `TooLarge` and streams exact; the pre-decode check
  uses the `@dN` model width. Failed background builds enter `DenseRows::Failed`
  with `retry_at`; `Building` is no longer reused forever and `warm` retries
  after backoff. Fresh review: 2 rounds — Important findings (start_build
  bypassing caps, stale Failed bytes, Building under-accounting) fixed and
  re-reviewed ADDRESSED; no new breakage.
  Verify: `cargo test --locked --lib --test hybrid_ann --test hybrid_rank_ann
  --test ann_store` (96+12+14+1 pass); clippy `--all-targets -D warnings`
  clean; fmt clean.
  Minor (deferred): the pre-decode byte check uses total `chunk_count`, so a
  partially covered project can be classified `TooLarge` although its loaded
  rows would fit; results remain exact, only caching is lost. T9's ANN-first
  lookup will revisit this path.

- Task T9: code complete (2026-10-22); Windows/macOS runtime CI pending.
  Cold start now tries `AnnStore::load`/`AnnIndex::load` before opening
  `VectorCache`, using the `@dN` model width to build the key; a persisted ANN
  is served with `vectors.db` deleted. `sync_dump_files` opens graph/data/meta
  with a write handle (Windows `FlushFileBuffers` needs `GENERIC_WRITE`).
  Cleanup skips `tmp-<current-pid>-*`, keeps two generations per model and
  enforces a global ANN dump budget (4 GiB default, oldest first, newest
  kept). Verify: `cargo test --locked --test hybrid_ann --test ann_store`
  (15+15 pass); clippy clean; fmt clean. Remaining: full Windows/macOS
  runtime evidence (CI).
- Task T13 (hybrid-providers): capacity default landed (2026-10-22). `VectorCache`
  default `max_bytes` is now 1 GiB (75k×2048 ≈586 MiB) with a guard test;
  configurable key and per-project lease/pin remain open.
- Task T10: dynamic `ef` landed (2026-10-22): `max(64, fetch*4)` capped at
  4096, unit test for 0/10/50/1000/MAX. End-to-end 75k×2048 through
  `DenseCache`/`VectorCache`/`search_docs` remains open.

- Task T10: complete (2026-10-22). `tests/ann_e2e.rs` has an 8k×256 CI run
  and an ignored 75k×2048 manual run through the real `DenseCache` +
  `VectorCache` + `search_docs_with` path; both assert coverage=100%, ANN
  publication, high-fetch (limit 100) determinism. CI run ~18 s. Manual
  release run `ann_e2e_75k_x_2048` passed twice: first 398.77 s wall, then
  with diagnostics 415.57 s wall, p50 1.46 ms / p95 2.88 ms over 20
  `search_docs_with` calls, process RSS 1995.6 MiB (peak includes the
  in-process build + allocator retention; cold-start steady state is not
  isolated). `ann_spike` remains the core-only build/load/recall/latency
  measurement. RSS above the 1 GiB steady-state budget is a diagnostic to
  investigate, not hidden: the value is peak process RSS, not the
  `DenseCache::estimated_bytes` 691 MiB model.

- Delta 0.2 status (2026-10-22): T8, T9 (code), T10 and hybrid T13 are
  complete with fresh review. Evidence: lib 98; config 17; hybrid_config 10
  (feature), hybrid_jobs 4; vector_cache 12; ann_store 15; ann_recall 9;
  hybrid_ann 15; hybrid_rank_ann 1; ann_e2e 8k CI + 75k manual (398.77 s).
  Clippy `--all-targets` clean with and without `hybrid-remote`; fmt clean.
  Remaining T11 gates: T1.5 (revert 13-day cooldown on 2026-10-23), actual
  Windows/macOS runtime CI, and the final spec status flip. T10 real RSS is
  an un-instrumented diagnostic, not a blocker.

- Task T12: complete (2026-10-23). `[profile.release] panic = "abort"` removed:
  the ANN fallback seam is `catch_unwind` around `hnsw_rs`, and abort turned a
  corrupt-dump panic into a process abort in every shipped artifact. Added a
  const-assert (`panic = "unwind"` is required) and a release-profile CI test
  (`ann_load_failure_falls_back_to_exact`). Size evidence: 11,478,776 B →
  12,626,328 B (+1.1 MiB, +10%) on a stripped glibc build; NFR-9 budget 40 MiB.
  Verify: `cargo build --release --config 'profile.release.panic="abort"'`
  fails with the guard message; release test passes.

- Task T13: complete (2026-10-23). `BuildJob.building_bytes` = graph estimate +
  exact rows retained by the build thread; `seek_ann` admits a build only when
  that sum fits `max_bytes`, so an over-cap project stays exact `Ready` instead
  of an unaccounted build (Delta 0.2 FR-7/NFR-4). Tests:
  `build_is_skipped_when_exact_rows_and_graph_exceed_the_cap`,
  `building_entry_accounts_exact_rows_it_keeps_alive`.

- Task T14: complete (2026-10-23). Debug-only `set_test_build_delay` makes
  `Building` deterministic; `ann_search_count()` counts ANN read-path hits.
  `tests/hybrid_rank_ann.rs` now asserts: first `get` is `Building`, the
  in-build search does not touch ANN, the published ANN serves the read path,
  and answers are deterministic. `tests/ann_recall.rs` adds recall@fetch >=
  0.95 against `top_k` for fetch in {100, 1000} (the final
  `ef = max(64, fetch*4)` rule). The degenerate 600x64 e2e fixture was not used
  for neighbour equality: its vectors are collinear to f32 precision, so the
  exact test moved to the separated-cluster fixture.

- Task T15: complete (2026-10-23). Backfill leases are only taken when a build
  may follow (`enabled && ranking == rrf`); `unpin` runs eviction; expired
  leases retry eviction in the same round. When every candidate is leased the
  cache stays over budget and prints one warning per operation (not per put).
  Test: `unpinning_reclaims_the_budget_without_waiting_for_the_next_put`.

- Task T16: complete (2026-10-23). `refresh_ann_after_sync` gates on
  `enabled && ranking == rrf`, so lexical projects no longer build a persisted
  ANN nobody reads. `dense_candidates` logs `ann.search` failures separately
  from insufficient candidates (FR-9). `DenseCache::invalidate` doc matches the
  actual (reserved for project deletion) role. Tests:
  `sync_refresh_warms_ann_for_rrf`, `sync_refresh_skips_ann_for_lexical_ranking`.

- Delta 0.3 status (2026-10-23): T12–T16 complete; panel findings P0 (release
  panic strategy), P1 (read-path evidence, byte accounting, unbound leases)
  and the diagnostics/bookkeeping minors are closed. Verify: `cargo fmt
  --check`; `RUSTUP_TOOLCHAIN=1.88.0 cargo clippy --all-targets -D warnings`;
  full `cargo test --locked --features hybrid-remote`; `search_bench
  metrics_match_baseline` (baseline unchanged). Still open (unchanged):
  Windows/macOS runtime CI (OQ-4), 75k manual run, T1.5 cooldown revert.

- Task T17: complete (2026-10-23, re-review). The Delta 0.3 admission rule
  (`graph + exact <= max_bytes`) made the ANN unreachable for the documented
  75k×2048 corpus: 1,305,600,000 B needed vs a 1,073,741,824 B default cap
  (ceiling 61,680 chunks at d2048, no config key). Raised the default resident
  cap to 1.5 GiB and moved the rule into `ann_build_admitted` /
  `ann_build_admitted_for_model`; steady-state ANN stays 691 MB (NFR-4 model
  unchanged, the cap now covers the transient build peak). Guard test:
  `default_cap_admits_the_documented_target_corpus`. Note: the earlier T10
  figure "691 MiB, still under the 1 GiB cap" described the steady ANN, not the
  build peak; the cap is 1.5 GiB now.

- Task T18: complete (2026-10-23, re-review). Backfill leases now require build
  admissibility, not only `enabled && ranking == rrf`:
  `ann_build_admitted_for_model(hashes.len(), model_id, default.max_bytes)` is
  applied inside `embed_project_chunks_with_limits_and_pin`, so a corpus the
  cap cannot admit no longer suspends the vector-cache budget for one hour
  without a build consuming the reservation.

- Task T19: complete (2026-10-23, re-review). The e2e target is now wholly
  `#![cfg(debug_assertions)]` with a RAII delay reset, so
  `cargo test --release --test hybrid_rank_ann --no-run` compiles again (it
  failed with E0432 after the debug hooks landed). `refresh_ann_after_sync`
  gates only `warm` on `enabled && ranking == rrf`; `cleanup`/`cleanup_disk`
  run for every project with an embedding stage so an rrf→lexical flip still
  reclaims dumps. `ANN_SEARCHES` now counts only lookups the ANN actually
  served, so "the published ANN serves the read path" is not satisfied by a
  consultation that fell back to exact.

- Delta 0.3.1 status (2026-10-23): re-review of `19c1766..7768870` by three
  models: one BLOCK (corpus admission), two OK WITH NOTES (cleanup behind the
  warm gate, release test target, counter semantics). All four are closed in
  T17–T19. Verify: `cargo fmt --check`; clippy `--all-targets -D warnings`
  (1.88); full default and `--features hybrid-remote` suites; `cargo test
  --release --test hybrid_rank_ann --no-run`; arithmetic guard test. Known
  pre-existing flakes (not introduced here, not fixed): `tie_break_by_chunk_id`
  and `scores_are_one_minus_cosine` on 2–3 point graphs.

- Task T20: complete (2026-10-23, second re-review). The lease gate counted
  distinct hashes (`hashes.len()`) while build admission counts chunk rows
  (`chunks.len()`), so a duplicate-heavy corpus near the ceiling took a
  one-hour lease without a build. It now uses `texts.len()`. New debug-only
  hooks `set_test_ann_max_bytes` (jobs) and `VectorCache::lease_count` let
  `backfill_leases_only_when_the_build_can_be_admitted` pin the boundary: at
  dim 3 a 4500-byte cap admits 4 distinct hashes but not 5 chunk rows, and the
  lease probe inside `embed` observes 0 leases (old code: 4). Default-cap
  control asserts a positive lease.

- Task T21: complete (2026-10-23, second re-review). Publish now drops the
  source rows and the built graph before `AnnIndex::load`, so the heap-reload
  path cannot hold a third full copy at 75k x 2048. Build admission adds
  `other_resident`, and `evict_oldest` prefers non-`Building` entries, so two
  admitted builds cannot silently double the resident cap
  (`second_build_is_not_admitted_while_the_first_occupies_the_cap`). Counter
  and cleanup regressions are covered: an oversized `fetch` falls back without
  counting (`hybrid_rank_ann`), and a lexical refresh removes a crashed
  `tmp-*` dir (`sync_refresh_skips_ann_for_lexical_ranking`).

- Delta 0.3.2 status (2026-10-23): second re-review round (two reviewers,
  `7768870..97da889`) closed. Verify: lib 108; hybrid_jobs 5; hybrid_rank_ann
  debug + release compile; fmt/clippy clean. Known pre-existing flakes remain
  (`tie_break_by_chunk_id`, `scores_are_one_minus_cosine`) and are not fixed
  here; the 75k peak is an accounting model, not an RSS measurement.

- Task T21 follow-up (2026-10-23, verifier): the aggregate admission check in
  `resolve_rows` was only a fast path — the lock was released before
  `start_build` inserted, so two concurrent lookups could both reserve. The
  re-check now happens inside the `start_build` entries lock and falls back to
  exact (`Ready`) when the reservation does not fit; verifier claim C3 was
  accepted as PARTIAL and is closed. Regression test
  `concurrent_lookups_admit_only_one_build` (barrier, two threads) passes
  repeatedly. Verifier claim C4 (design still said 1 GiB in the limits block)
  also closed; `grep "1 GiB" docs/specs/ann-index/design.md` is empty.

- Task T1.5: complete (2026-10-23). The temporary 13-day `min-publish-age`
  window for `hnsw_rs 0.3.5` is reverted: `.cargo/config.toml` carries the
  policy default `14 days` again (2026-10-09 + 14 days = 2026-10-23, so 0.3.5
  still resolves), `tests/smoke.rs` asserts the 14-day string, and
  `docs/supply-chain.md` records the closed exception instead of the open
  workaround. `hnsw_rs 0.3.5` stays pinned in `Cargo.lock`; no dependency
  change. Verify: `cargo test --locked --test smoke`, full suite, clippy/fmt.

- CI fix (2026-10-23): the first PR run failed Windows on the new
  `sync_refresh_warms_ann_for_rrf` test — the 20 s wait expired under the
  parallel lib suite on a 2-core runner (both new server tests reported "over
  60 seconds" during 5000-file indexing). The fixture is now 4200 documents
  (still above `ANN_MIN_VECTORS = 4096`) and the waits are 180 s (warm),
  120 s (publish poll), 5 s (lexical negative window), 60 s (cleanup). Linux
  and macOS were already green.
