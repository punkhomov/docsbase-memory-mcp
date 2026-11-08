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
**Scope:** release-suite selection helper and its Python checks, `nightly.yml`,
`docs/ci.md`, CI-testing spec/ledger.
**Steps:** pin automatic discovery/exclusion/failure behavior; derive remaining
Cargo test targets from metadata, preserve doctests; retain isolated perf step;
compare actual release inventories and execute both groups. Fresh task review,
final branch review and native PR CI; merge remains a human action.
**Acceptance:** FR-9–FR-11; SC-5.
