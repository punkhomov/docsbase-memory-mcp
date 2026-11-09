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

### C7 — Deterministic config-writer interleaving and lock retries

**Delta:** 2026-11-09. **New crates/packages:** none.
**Scope:** src/cli/models.rs and src/config/paths.rs private lock/wait seams and
unit tests; CI-testing specs/ledger and docs/ci.md.
**Steps:** measure the old writer test and demonstrate its stale-read blind
spot; thread a local wait callback with unchanged production sleeping/retries.
Force real lock contention before A publishes; assert complete preserved TOML
and cover the default entry. Record lock retry delays with real OS exclusion,
including exhaustion and acquisition after release. Prove sensitivity with
controlled mutations, restore source, verify suites/lints/reviews/native CI.
**Acceptance:** FR-21–FR-24; SC-10. No watchdog tightening or runtime-policy change.

### C8 — Observe stale-schema watcher refusal and unchanged data

**Delta:** 2026-11-09. **New crates/packages:** none.
**Scope:** tests/admission_ux.rs and CI-testing specs/ledger/docs only.
**Steps:** measure the old case; demonstrate its false negative with silently
ignored stale batches. Add healthy watcher control and persisted refusal
acknowledgements correlated to project/new job ids/canonical paths. Retain the
README edit plus a fresh witness path; assert schema, committed data and physical
index invariants without fixed sleeps. Controlled mutations verify sensitivity;
restore source and run affected tests/lints/reviews/native CI.
**Acceptance:** FR-25–FR-27; SC-11. No production or CI routing changes.


### C9 — Deterministic HTTP retry policy and accurate client smoke

**Delta:** 2026-11-09. **New crates/packages:** none.
**Scope:** src/hybrid/wire.rs private timing seam/unit tests,
tests/hybrid_openrouter.rs, CI-testing specs/ledger and docs/ci.md.
**Steps:** measure existing integrations and demonstrate the zero-header test's
blind spot; RED/GREEN a private sleep/jitter seam with unchanged production
policy. Assert ordered requests/delays/jitter, header cap/overrides, retryable
statuses, terminal paths and last-error source. Retain default client wiring
smoke and move two real-wait exhaustion tests into stronger units. Controlled
mutations, source restoration, inventory accounting and affected suite/lint
verification. Fresh task/final reviews and native PR CI; never merge.
**Acceptance:** FR-28–FR-30; SC-12.


### C10 — Separate the static artifact build from validation

**Delta:** 2026-11-09. **New crates/packages:** none.
**Scope:** tests/artifact.rs, its Cargo opt-in metadata, ci/nightly/release
workflow steps, AGENTS verification guide, CI-testing specs/ledger/docs/ci.md.
**Steps:** measure old nested builds; establish the supplied-file validator's
ELF/diagnostic preconditions. Remove hidden build, preserve static/size gates,
and route one explicit locked static build + checker + provider smoke through
required Linux. Keep nightly/release validation explicit and opt-in lint covered.
Verify bad artifacts and no hidden Cargo, default/explicit inventories/metadata,
MSRV build/functional checks/lints/workflow syntax. Fresh task/final reviews and
native PR CI; no merge.
**Acceptance:** FR-31–FR-34; SC-13.


### C11 — Four required native jobs and one periodic compatibility job

**Delta:** 2026-11-09. **New application crates/packages:** none; existing
cargo-hack CI tool installed locally for verification.
**Scope:** ci.yml, new feature-compatibility.yml, docs/ci.md, AGENTS CI index,
CI-testing ledger and dated source-compatibility rulings in affected specs.
**Steps:** verify actual protected check names; preserve native steps while
moving expensive compatibility steps unchanged into one weekly/manual job.
Retain/fix cheap default dependency gate with producer failure controls. Document
post-merge feature-off detection and supersede old linux-local-required ruling.
Prove selectors/metadata/commands/inventories, execute local compatibility flow,
workflow/observer checks, fresh task/final reviews and native PR CI. Attempt
manual optional workflow; honestly record registration limits. Human merge only.
**Scope revision:** the local lean run exposed get_doc framing setup indexing
2MB through a2s client and a weak assertion accepting client Transport errors.
Repair only that tests/mcp_docs.rs fixture/assertions under FR-40; sensitivity
probe and complete final checks, without production or timeout changes.
**Acceptance:** FR-35–FR-40; SC-14.
