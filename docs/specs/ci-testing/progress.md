# CI testing progress

## Delta 2026-11-08

- Task C1: complete (2026-11-08), local implementation/verification. Base
  `a293856` (#85). All six job names, triggers, runners, test selections,
  features, environments and timeouts verified unchanged. No product/test
  assertions, dependencies, baselines or scheduled workflows changed.
  RED: observer absent; later a nested helper summary counted twice (2 targets
  instead of 1). GREEN: six Python subprocess regressions. MSRV cli-full
  prepare/inventory/execute: exit 0; 877 inventory names, 859 passed /18 ignored,
  helper subprocess separately. Parser fix checked against the saved full log:
  859 passed /18 ignored /0 failed. Full clippy/fmt, actionlint and diff checks
  clean. Native execution and review rulings follow in PR close-out.
  Local measurements: prepare 65.271 s; inventory 0.556 s; execute 294.794 s,
  including artifact target 122.47 s with a nested static release build. These
  are one host/run, not a CI speedup result or a cold/warm classification.

  Ruling: first fresh C1 review — Spec FAIL / Quality CHANGES, one Important:
  inherited stdout encoding can reject UTF-8 diagnostics on Windows. RED
  reproduced with strict ASCII stdout and Russian/Japanese child output.
  Fix: configure observer stdout/stderr as UTF-8; console/log/state regression
  now passes. Scoped re-review: Important ADDRESSED; no new Critical/Important
  findings. Final branch review and native CI follow in PR close-out.

## Delta 2026-11-09

- Task C2: complete (local verification) on `ffeadf6` (#88 merged).
  Published network changes verified:
  Windows Actions log and diagnostic artifact download successfully. C1 Windows
  target timings identify hybrid_rank_ann 197.89 s, lib 197.25 s, ann_e2e
  150.45 s and ann_recall 67.73 s. These are one runner/run, not a CI p95.
  Before: two validation tests 24.16 s, Cargo preparation 0.52 s. After: both
  pass in libtest's 0.00 s resolution; full ann_recall 10/10 passes (54.32 s).
  Large quality/persistence scenarios and thresholds unchanged; high-fetch
  membership now uses a set. Mutation accepting empty/mismatched queries makes
  both tests fail (0 passed/2 failed); source restored byte-for-byte and both
  tests pass again. MSRV1.88 full cli-full clippy/fmt clean; production diff
  empty. Fresh task review and native CI recorded at PR close-out.
  Ruling: fresh C2 review — Spec PASS / Quality APPROVED; findings none.
  Native PR CI remains pending for final branch close-out.
- Task C3: complete (local verification). Use the Cargo-native opt-in target
  (selection revision dated 2026-11-09), retain
  explicit isolated perf and add a separate Linux PR/nightly Clippy invocation.
  Controlled MSRV Cargo probes PASS: default/new targets retained, explicit
  perf runs, and its deliberate compile error is caught by explicit Clippy;
  `--all-targets` alone omits it. Initial release-all preparation was stopped
  after observing expensive LTO across many unchanged targets; it is not a
  passing full-suite result. Validation uses actual before/after default
  inventories, the metadata selection delta, isolated release perf and full
  native PR functional CI, without changing any build profile or budget.
  Actual inventories PASS: 888 = 882 + 6 names, zero overlap; 64 = 63 + 1 target
  headers, including empty bin/doctest targets. Cargo metadata differs only at
  perf_budget.test (true -> false). The isolated release cli-full target runs
  all six tests: 6 passed/0 failed, 12.62 s execution (309.319 s wall including
  optimized compilation). Full cli-full Clippy and explicit perf Clippy pass;
  fmt, actionlint, original six Python observer checks and workflow
  jobs/triggers/runners/timeouts parity pass. No production/dependency/baseline
  changes; performance bodies/ignore attributes unchanged. Fresh C3 review,
  final branch review and native PR CI follow at close-out.
  Ruling: fresh C3 review — Spec PASS / Quality APPROVED; findings none.
  Final branch review and native PR functional CI remain pending.

- Task C4: complete (local verification). First native run42521642317
  at91d4701: macOS failed
  install_owned_artifacts at tests/install.rs:104 with empty stderr. The helper
  discarded exit status/stdout, so termination cause is unknown. Capture full
  Output while retaining the exact success assertion and retry behavior. No
  unconditional rerun/retry or timing-budget change is added to the test.
  Existing install target: 15 passed/0 failed; scoped cli-full Clippy and fmt
  pass. Fresh review and final-head native CI follow. This improves evidence
  for a recurrence and does not diagnose/fix the original termination cause.
  Ruling: fresh C4 review — Spec PASS / Quality APPROVED; findings none.
  Final-head native CI remains pending; original macOS cause remains unknown.

## Delta 2026-11-09 — C5

- Task C5: complete (local verification) on29ea686 (#90 merged).
  Replace the fixed30s lexical
  non-publication wait with observed warm calls and worker completion. Keep a
  real default ANN threshold test while reducing file count; no runtime knob,
  feature/profile, performance-budget or workflow change planned.
  Baseline original pair:2passed,68.11s execution (2.20s Cargo preparation).
  Initial four policy/cleanup cases:4passed,0.61s execution. Real below/at-default
  boundary:1passed,21.07s, with one file and asserted DB chunk counts.
  Full cli-full Clippy and explicit perf Clippy/fmt pass. All eight controlled
  mutations are caught by the expected assertions: unconditional/no warm,
  ignored enabled flag, skipped project/disk cleanup, early returns after warm
  or project-cleanup error, and lowered constructor threshold. Source restored
  byte-for-byte. Final restored library suite:254passed/0failed,38.05s.
  Relevant integration suites pass: config_runtime8, hybrid_jobs4, mcp_registry18.
  Final restored-source full/explicit-perf Clippy and fmt clean. No dependency,
  workflow, baseline or numeric runtime-default/budget changes. Fresh C5 and
  final branch reviews/native PR CI follow at close-out.
  Ruling: fresh C5 review — Spec PASS / Quality APPROVED; findings none.
  Native PR CI and final branch review remain pending for PR close-out.
