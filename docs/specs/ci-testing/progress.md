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
- C3: planned after C2; retain isolated perf and preserve the other release
  targets by automatic Cargo metadata discovery.
