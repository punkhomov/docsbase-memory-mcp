# CI testing

## Delta 2026-11-08 — C1: observe the current suites

This delta establishes measurements before changing test semantics or routing.

- **FR-1:** Every existing functional test invocation in `ci.yml` retains its
  features, target selection, assertions, ignored-test policy and required-check
  status. Release/nightly workflows and timing budgets are unchanged.
- **FR-2:** Each invocation gets separately measured preparation (`--no-run
  --timings`), inventory (`-- --list`) and execution phases. Cargo remains the
  test runner. Preparation does not claim to precompile doctests or release
  builds launched by tests.
- **FR-3:** Record the exact command, revision/run/attempt/job, platform, reported
  cache-hit value, selected non-secret build settings, elapsed wall time and
  exit code. Stream UTF-8 execution output even with a legacy stdout encoding,
  and periodically report progress during
  quiet periods. Preserve the child failure as a failing CI step.
- **FR-4:** Save flushed logs and phase state from the beginning of a command;
  collect stable libtest target durations/counts and inventory names without
  claiming individual-test durations. Publish a job summary and diagnostic
  artifacts on success or failure. Interrupted state and artifact upload after
  runner termination are best effort, never a guarantee.
- **FR-5:** Document current test routing and measurement limits, including
  hidden artifact builds, compile/fixture/execution overlap, cache-hit ambiguity
  and the distinction between this first delta and the planned CI structure.

## Success criteria

- **SC-1:** Automated checks prove nonzero exit propagation, retained output,
  inventory and target-summary parsing, and missing-executable diagnostics.
- **SC-2:** A real MSRV `cli-full` prepare/inventory/execution sequence succeeds;
  before/after suite selection is identical. Native PR CI validates all three OS.
- **SC-3:** No production code, Cargo dependencies, test baselines or test
  assertions change. No tests move to scheduled-only execution in C1.

## Delta 2026-11-09 — C2/C3: small fixtures and isolated performance checks

- **FR-6 (C2):** Empty-query and dimension-mismatch ANN checks use a small,
  valid index with nonzero vectors. A valid-query control must succeed; empty,
  shorter and longer queries must retain their error assertions. Keep test names.
- **FR-7 (C2):** Preserve the 5,000-vector/64-dimension recall and persistence
  scenarios, query/fetch counts and recall thresholds. Exact/approximate ID
  intersection may use a set without changing which hits count.
- **FR-8 (C2):** No production/dependency/baseline/budget change. Demonstrate
  that accepting invalid queries makes the validation tests fail. Measurements
  compare test execution separately from compilation, without a CI speedup claim.

### C3 selection revision — 2026-11-09

Cargo-native opt-in selection replaces the initially planned metadata selector.

- **FR-9 (C3):** Nightly runs the entire `perf_budget` target exactly once in
  release `cli-full`, with `--test-threads=1`. It must not run again in the
  remaining release suite. Preserve all performance assertions and budgets.
- **FR-10 (C3, revised):** Mark only `perf_budget` as opt-in in Cargo.toml.
  Keep the normal Cargo release invocation for every other default target,
  including future integration/bin/example targets and doctests. Preserve
  Cargo failure handling; no custom selector or runner.
- **FR-11 (C3, revised):** Keep PR jobs/triggers/features/search gate and
  schedules unchanged. The six performance tests no longer appear as ignored
  entries in default Linux debug suites; nightly release still executes them.
  Linux PR CI and nightly explicitly lint `--test perf_budget`, retaining
  pre-merge checking of that Linux-only code. Preserve test bodies, ignore
  attributes and all functional executions. Document this selection change.

### Success criteria for this delta

- **SC-4:** C2's invalid-acceptance mutation fails both named tests; restored
  small-fixture tests and the complete `ann_recall` target pass on Rust 1.88.
  Native PR CI, full clippy and fmt pass.
- **SC-5 (C3, revised 2026-11-09):** Verify real default inventories before/after:
  perf exactly once in its explicit group, every other target/name retained by the
  ordinary suite. Check the Cargo target-metadata delta (profile-independent),
  execute all isolated release performance tests, and run the full functional
  suite in native PR CI. Validate explicit compile/lint coverage and workflow
  syntax. A controlled MSRV release Cargo probe must confirm new targets
  still auto-discover and compile errors in the opt-in target are caught by explicit checking. No metadata helper or
  implementation-mirroring configuration unit tests are required.

## Delta 2026-11-09 — C4: install subprocess failure diagnostics

- **FR-12:** When the install fixture's launcher exits unsuccessfully, include
  its complete `Output` (exit status, stdout and stderr). Keep the command,
  success assertion, copy behavior and existing ETXTBSY-only retry unchanged.
  An empty-stderr macOS failure is an observation, not a diagnosed product bug;
  neither diagnostics nor a passing rerun may be described as fixing its cause.
- **SC-6:** Existing install suite, fmt and Clippy pass; fresh task review and
  final-head native CI pass. No production change or new retry/timeout.

## Delta 2026-11-09 — C5: observable daemon ANN refresh

- **FR-13:** Preserve refresh policy and behavior: a configured embedding
  warms only when hybrid is enabled and ranking is RRF; use the resolved model
  and project. Configuration errors/missing embedding still skip the worker.
  Warm and project-cleanup failures must not suppress subsequent cleanup.
  Existing callers keep detaching the background worker; defaults and config
  resolution remain unchanged.
- **FR-14:** Give the private refresh boundary a completion handle and an
  injectable warm operation. Tests observe the actual worker/config/cleanup
  path without global hooks. A gated positive control proves the worker runs
  asynchronously and cleanup follows warm. Negative lexical/default-lexical
  and disabled-hybrid checks prove zero warm calls after confirmed completion,
  instead of waiting a fixed interval for absence of READY.
- **FR-15:** Verify real stale-temp cleanup and disk-budget cleanup, including
  continuation after injected warm failure and real project-cleanup failure.
  Use a small per-fixture disk budget, not a public test-only config setting.
- **FR-16:** Retain real default ANN admission coverage: below ANN_MIN_VECTORS
  remains exact; at the threshold refresh builds/publishes an ANN. Produce the
  corpus in one Markdown file and verify actual stored chunk counts. Observe
  completion passively so the test cannot start the build it claims to detect.
  Keep existing large recall/scale tests and numeric thresholds unchanged.
- **FR-17:** No new dependencies, jobs, public API/config knobs or global test
  state. Any remaining timeouts are real watchdogs that finish on observed
  progress; no performance-policy or numeric budget relaxation.
- **SC-7:** MSRV cli-full policy/default-boundary tests pass. Controlled policy
  and cleanup mutations fail their named tests; restore source before final
  verification. Record baseline/after execution separately from compilation.
  Full/explicit-performance Clippy, fmt and native CI pass.
- **SC-8:** Fresh C5 task and final branch reviews have no unresolved
  Critical/Important findings. Prepare a green PR; merging stays human-only.

## Delta 2026-11-09 — C6: compact ANN search-path fixture

- **FR-18:** Replace growing repeated-text documents in `hybrid_rank_ann` with
  short unique documents. Retain 600 main documents, 20 small documents and
  64-dimensional normalized spread embeddings; assert one stored chunk per
  document and distinct content hashes. Retain real indexing, backfill,
  background build, persistence/load and search through `search_docs_with`.
- **FR-19:** Preserve the build gate, RAII release, real watchdog and ANN-served
  counter controls. While the build is gated, search must return the same ten
  paths as an ANN-disabled exact control, without increasing the ANN counter.
  Published ANN must serve repeatable ten-result searches. A small published
  graph below `MAX_HITS` must report insufficient candidates, return the exact
  fallback result, and not increase the ANN-served counter.
- **FR-20:** Keep the single debug test target (global gate isolation), query,
  fetch/limit, per-fixture admission setting and all numeric production defaults,
  recall thresholds, large scale/default-boundary suites and CI routing intact.
  No production changes, new dependencies or global hooks. Retain project roots
  through searches and release DB handles before cache directories.
- **SC-9:** Record old/new execution separately from compilation, including
  temporary fixture/build phase probes. New fixture assertions fail with the
  old corpus; controlled read-path/fallback mutations fail their expected
  assertions. Restore source before final target/related tests, full and
  explicit performance Clippy/fmt and native CI. Fresh task/final reviews have
  no unresolved Critical/Important findings; prepare a green PR, human-only merge.
  Shared-runner timings alone establish no overall CI speedup.

## Delta 2026-11-09 — C7: deterministic config-writer and lock tests

- **FR-21:** Preserve config-lock behavior: real fd-lock exclusion, data-dir
  lock location/path naming, bounded retry count/delay, error propagation and
  guard lifetime. A private/internal per-call wait seam uses `thread::sleep`
  in production; tests may observe retries and advance by releasing a held
  lock without waiting real time. No public configuration knob/global hook.
- **FR-22:** Replace model-writer sleep ordering with an observed interleaving:
  A owns the real lock and has not published its embedding; B attempts that
  same lock and confirms contention; only then A publishes and releases; B
  resumes, re-reads under the lock and preserves both stages. Verify complete
  persisted values, unrelated sections and ranking, not mere stage presence.
  Keep project/global write behavior and exercise the default production entry.
  Channels/joins are per-fixture; errors/panics release gates, with real watchdogs.
- **FR-23:** Lock-policy tests keep real OS locks but record requested waits.
  Exhaustion must not run the write; every failed attempt requests the existing
  delay; releasing the held lock during a retry permits the write exactly once.
  Assert lock placement remains outside the project root. No fixed test sleep
  or elapsed-time assertion establishes ordering or retry correctness.
- **FR-24:** Keep read/merge/write/fsync/rename, provider validation and actual
  CLI behavior unchanged. No dependencies, numeric defaults, performance-budget,
  workflow or baseline changes. Watcher/HTTP-backoff audits remain separate.
- **SC-10:** Reproduce the old test's blind spot with a pre-lock-read mutation;
  the replacement detects stale-read, missing exclusion and retry-policy
  violations by expected assertions. Restore source before final models/config
  lock suites, relevant CLI integrations, full/perf Clippy and fmt. Native CI
  and fresh task/final reviews pass; prepare a green PR, merge human-only.

## Delta 2026-11-09 — C8: observed stale-schema watcher refusal

- **FR-25:** Replace the stale-schema watcher's fixed2.5s absence check with
  observed progress. Retain a real daemon, native notify backend, existing
  README edit and actual debounce. First prove a healthy edit updates committed
  SQLite chunks/generation and searchable content. After downgrading the stamp,
  add a fresh Markdown witness path and observe new terminal failed watcher
  jobs for the project/changed paths with the rebuild instruction. The fresh
  path must not exist before the stale edit; do not assume two paths share one
  batch or identify a raw OS event beyond the consumer evidence available.
- **FR-26:** After confirmed refusals, search remains refused with the rebuild
  hint; schema stays stale. Generation, document identities/hashes and chunk
  texts remain unchanged, and a newly opened physical index still serves the
  healthy control but not stale edits. Do not let observers trigger indexing.
- **FR-27:** Use existing persisted failed-batch diagnostics, no production
  code, public API, test hooks, dependencies, workflow/default/budget/baseline
  changes. Normalize diagnostic paths through the filesystem on all native OSes.
  Poll conditions under a real watchdog; no mandatory sleep proves completion.
- **SC-11:** Reproduce an old-test false negative with silently ignored stale
  batches. New test detects ignored work, missing/corrupt path diagnostics,
  stale admission bypass/restamping and index-data changes via named assertions.
  Restore mutations before final affected suites/full+perf Clippy/fmt, native
  CI and fresh task/final reviews. Prepare a green PR; merge human-only.
  Improved sensitivity is separate from any whole-CI timing claim.
