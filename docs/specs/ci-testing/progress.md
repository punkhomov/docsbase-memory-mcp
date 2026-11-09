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


## Delta 2026-11-09 — C6

- Task C6: complete (local verification) on6f8a74a (#91 merged).
  The old600-document fixture yields12660chunks/18,570,000body bytes;
  instrumented original test passes in55.87s (Cargo prepare15.33s).
  RED: new one-chunk-per-document oracle rejects12660vs600. Compact unique
  documents retain600paths/600chunks/64dimensions,9,000body bytes; small
  project20docs/chunks. Real build gate/persistence/read-path controls remain;
  add ANN-disabled exact oracles for gated search and insufficient fallback,
  ten published hits, checked fallback diagnostics and zero served-counter delta.
  Initial after8.08s; temporary phase probe7.07s (main indexing/setup0.401s,
  build/persist/load wait1.230s; old16.849s/9.895s). Phase boundaries differ in
  DB-open placement; no controlled A/B or whole-CI speedup claim.
  All5mutants fail their expected assertions: reordered build-time candidates,
  ANN bypass, counted/empty insufficient fallback, duplicate fixture text.
  Both sources restored byte-for-byte; subsequent exact-helper extraction
  fixes Clippy110lines without an allow or changed oracle logic.
  Full/explicit-perf Clippy and fmt pass. Restored related suites: ann_e2e1pass
  /1ignored104.54s, ann_recall10pass77.16s, hybrid_rank_ann1pass6.88s.
  No production/workflow/dependency/default/budget/large-quality-test changes.
  Fresh task/final reviews and native CI follow at PR close-out.
  Ruling: fresh C6 review — Spec PASS / Quality APPROVED; findings none.
  Final branch review and native PR CI remain pending for close-out.


## Delta 2026-11-09 — C7

- Task C7: complete (local verification); base revision d0fbc60 (#92 merged).
  Old writer test1passed0.42s; the stale-pre-lock-read mutant also passes
  it0.42s, proving the old ordering can miss lost updates. Original source
  restored. RED new tests lack the wait seams (E0425/E0061). GREEN new writer
  test1passed0.02s; paths group3passed0.01s plus child wrapper verification.
  A holds a real lock until B confirms failed acquisition, then publishes
  and releases before B's next attempt. Literal full-TOML expectations retain
  both stages/values/ranking/unrelated providers; project test uses the default
  production entry. Per-call FnMut wait callbacks record policy and control
  retries; production still sleeps with the same50x20ms policy and OS guards.
  All5mutants fail the expected assertions: stale read, early guard drop,
  omitted wait, write after exhaustion, wrong delay. Both candidate sources
  restored byte-for-byte and checked before final verification.
  Restored full library254passed0failed51.06s; CLI integrations39passed
  (cli_models23/config_runtime8/connect8). Full/perf Clippy and fmt/diff pass.
  No dependency/default/budget/workflow/baseline changes; actual CLI writes,
  data-dir routing and read/merge/fsync/rename behavior preserved. This fixes
  test sensitivity, not a newly reproduced production lost-update defect.
  Fresh task/final reviews and native PR CI follow at close-out.
  Ruling: fresh C7 review — Spec PASS / Quality APPROVED. Minor: identify
  d0fbc60 as the base, not C7's completed revision; wording corrected in-task.
  Native CI/final branch review remain pending for PR close-out.


## Delta 2026-11-09 — C8

- Task C8: complete (local verification); base revision d983f87 (#93 merged).
  Original stale-watch test1passed2.99s (Cargo prep17.29s); with silently ignored
  stale batches it still passes2.74s, demonstrating absent progress evidence.
  Source restored. New scenario retains README edit and adds healthy committed
  data/search control plus a fresh post-downgrade path. New terminal jobs after
  a project-scoped cursor acknowledge canonical paths across batches. Schema,
  generation/docs/hashes/chunks stay unchanged; fresh physical reader retains
  healthy content and excludes stale edits. No mandatory2.5s quiet period.
  New normal case1passed3.41s; stronger controls add a phase, no speedup claim.
  All5mutants fail expected assertions: silent stale batch, cached admission,
  restamp, corrupt stored hash, wrong refusal paths. Production watcher restored
  byte-for-byte; final test unchanged after verification. Healthy poll permits
  only the existing documented rebuild-marker transient; unexpected errors fail.
  Existing one-shot commit-fault child probe fires/retries/recovers, then proves
  stale refusal; its temporary test edits restored. Final affected suites36pass
  (admission_ux5/mcp_registry18/watch_retry5/watcher8), full/perf Clippy/fmt clean.
  Production/API/dependency/workflow/default/budget/baseline diff empty.
  Fresh task/final review and native PR CI follow at close-out.
  Ruling: fresh C8 review — Spec PASS / Quality APPROVED; findings none.
  Final branch review and native PR CI remain pending for close-out.
