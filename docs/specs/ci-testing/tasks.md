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
