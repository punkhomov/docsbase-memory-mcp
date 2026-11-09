# CI testing tasks

## Delta 2026-11-08

### C1 — Inventory and phase measurements

**New crates/packages:** none.
**Scope:** `.github/scripts/observe-tests.py`, its Python regression checks,
`.github/workflows/ci.yml`, `docs/ci.md`, this spec.
**Steps:**
1. Pin observer failure/diagnostic contracts with subprocess regression checks.
2. Add a streaming, heartbeat-enabled observer and structured phase reports.
3. Wrap each existing CI test selection with prepare/inventory/execute steps;
   upload diagnostics with `always()`. Run the observer checks in the docs job.
4. Document coverage ownership and interpretation of the measurements.
**Verify:** Python regressions, workflow syntax/selection parity, real MSRV
full-profile prepare/inventory/test, clippy/fmt, fresh task and final reviews,
native CI in the PR. No speedup claim before comparable runner measurements.
**Acceptance:** FR-1–FR-5; SC-1–SC-3.

## Delta 2026-11-09

### C2 — Small ANN validation fixtures

**New crates/packages:** none.
**Scope:** `tests/ann_recall.rs`, CI-testing spec/ledger.
**Steps:** measure the two existing validation tests; use a two-vector fixture
with a valid-query control; cover both dimension-mismatch directions; replace
quadratic ID membership in high-fetch recall with a set. Preserve large recall
and roundtrip scenarios. Run a controlled invalid-acceptance mutation, restore
production source, and verify the complete target. Fresh task review before commit.
**Acceptance:** FR-6–FR-8; SC-4.

### C3 — Run nightly performance budgets once

**New crates/packages:** none.
**Selection revision — 2026-11-09:** Cargo-native opt-in replaces the planned
metadata helper. **Scope:** Cargo.toml target metadata, `ci.yml` explicit Linux
perf lint, `nightly.yml`, AGENTS/docs and this spec.
**Steps:** verify native Cargo selection/compile behavior with controlled probes;
make perf opt-in while retaining the isolated nightly step; preserve default
discovery/doctests and pre-merge compile/lint coverage. Compare actual inventories
and metadata delta; run release perf. Fresh task review, final branch review and
full native PR functional CI; merge remains a human action.
**Acceptance:** FR-9–FR-11; SC-5.

### C4 — Retain the install launcher's exit status

**Delta:** 2026-11-09. **New crates/packages:** none.
**Trigger:** macOS job128856213927, run42521642317: install_owned_artifacts
fails in Env::install with empty stderr; the old assertion omits exit status.
**Scope:** tests/install.rs assertion message and this ledger/spec.
**Steps:** include full subprocess Output without changing execution/assertion
semantics; run the existing install target and lint checks; fresh review.
Record the initial failure and retain the distinction between better diagnostics
and an unproven root cause. Native final-head CI completes branch close-out.
**Acceptance:** FR-12; SC-6.

### C5 — Deterministic daemon refresh and compact default-threshold fixture

**Delta:** 2026-11-09. **New crates/packages:** none.
**Scope:** private refresh worker boundary/call sites and unit tests in
src/daemon/server.rs, CI-testing spec/ledger, docs/ci.md.
**Steps:** measure original refresh tests; expose private worker completion and
warm injection without changing policy; add gated positive/negative controls
and real cleanup failure checks; retain default admission boundary using a
single indexed Markdown file. Prove sensitivity with controlled mutations;
restore production behavior and run targeted tests/lints/native CI. Fresh task
and final branch reviews; prepare the PR without merging.
**Acceptance:** FR-13–FR-17; SC-7–SC-8.

### C6 — Compact ANN search-path fixture

**Delta:** 2026-11-09. **New crates/packages:** none.
**Scope:** tests/hybrid_rank_ann.rs, CI-testing specs/ledger, docs/ci.md.
**Steps:** measure the original test with temporary phase probes; add checked
fixture counts and exact-path controls; demonstrate RED on the old growing
corpus. Replace repeated growing bodies with short unique documents while
retaining document counts/dimensions/search windows and real build/load/search.
Check controlled build-time/read-path/fallback mutations, restore source and
verify target/related suites/lints. Fresh task/final review and native PR CI.
**Acceptance:** FR-18–FR-20; SC-9. No whole-CI speedup claim without comparable data.
