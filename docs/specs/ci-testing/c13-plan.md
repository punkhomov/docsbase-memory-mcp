# C13 — release-compatible lease assertions (#143)

Goal: FR-42 / SC-16; retain the existing lease admission and lifetime regressions
in actual optimized release builds. This is a bounded test-only correction.
Base: 3b347320d9399edebbb3d597df74fd720d2f7f14. New crates: none.

Files: tests/support/vector_leases.rs; tests/hybrid_jobs.rs;
tests/cache_settings.rs; tests/ann_lifetimes.rs; ci-testing specification/ledger.
The helper is explicitly imported by those targets, not the shared support mod.
Use existing rusqlite and VECTOR_DB_FILE, READ_ONLY, parameterized model key,
checked u64 count. Preserve fake Embedder execution and all original counts,
model independence, TTL and failure cleanup assertions. No provider/model I/O.

Task C13, one implementation task:

1. RED: preserve actual nightly43413584035 E0599 and local all-three-target
   release compile failure in /workspace/scratch/agent-b/143-release-red.log.
   Reconfirm on current base before editing tests; current-base evidence is
   /workspace/scratch/agent-b/143/release-base-red.log. Strengthen the admitted
   hybrid_jobs control to exactly 4 observed leases on its old shared fixture;
   capture a focused debug sensitivity RED when its untouched u64::MAX sentinel
   reveals that the already-filled cache skipped fake Embedder execution.
2. Replace debug-only inspection calls with shared readonly test helper.
   Give only that admitted control a fresh indexed fixture with the same corpus
   and policy; retain the exact 4 assertion to require a real callback observation.
   Add targeted helper checks to hybrid_jobs: missing file must not be created;
   model keys remain parameterized; an expired row is counted; reads preserve DB.
3. GREEN: run the three targets with --features cli-full in debug and release;
   run the helper checks in lean release. Build Web (and SDK if present) then
   cargo test --locked --features cli-full --release with debug assertions disabled.
   Run cargo clippy --locked --features cli-full --all-targets -- -D warnings,
   cargo fmt --check. Local debug symbols may be disabled for disk budget only.
4. Fresh task reviewer and most-capable whole-diff final reviewer; delegate any
   finding fixes, obtain scoped re-review. Preserve RED/GREEN logs/manifest.
5. PR closes #143; four green native checks on final integrated head and inventory
   audit before normal squash merge. No red gate override.

Review focus: observer does not mutate cache or bypass ownership writes; no cfg
skips, weaker counts, TTL changes, hidden errors, new production API/dependencies.
Existing release-gated ANN graph diagnostics are intentionally debug-only and
are not broadened by this fix. No latency baseline or threshold is changed.


## Delta 2026-11-13 — verification checkpoints and integration

The full optimized runtime suite passed on27c80a4/base0c6d707:1384/0/18,
86targets. Following independent A146/A148 merges, the four changed C13 Rust
files remain byte-identical. A146 combined runtime release targets passed29/0;
A148 final all-release-tests typecheck, all-target Clippy and fmt passed. The full
runtime result is not relabelled as final-head runtime. Fresh task review passed;
whole-diff final review and four final-head native checks/inventory are retained
as integration/delivery gates. No skipped test, weaker assertion or changed
release profile is used to satisfy the debug-only API regression.
