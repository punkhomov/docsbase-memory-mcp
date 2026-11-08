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
- **FR-9 (C3):** Nightly runs the entire `perf_budget` target exactly once in
  release `cli-full`, with `--test-threads=1`. It must not run again in the
  remaining release suite. Preserve all performance assertions and budgets.
- **FR-10 (C3):** Preserve all other default release test targets, including
  library/bin tests and doctests. Discover integration targets from Cargo
  metadata, so new targets are included without a maintained allowlist.
  Fail explicitly on unsupported target layouts or missing exclusions rather
  than silently dropping coverage. Preserve Cargo failure status.
- **FR-11 (C3):** Keep PR jobs, triggers, required search gate, toolchain,
  features, scheduling and existing ignore policy unchanged. Document the new
  nightly selection and verify its inventory against the original full suite.

### Success criteria for this delta

- **SC-4:** C2's invalid-acceptance mutation fails both named tests; restored
  small-fixture tests and the complete `ann_recall` target pass on Rust 1.88.
  Native PR CI, full clippy and fmt pass.
- **SC-5:** C3's automated checks cover discovery of a newly added target,
  exclusion/retention, unsupported layouts and subprocess failure propagation.
  Actual release inventory partitions into perf exactly once and every other
  original target exactly once, including doctests. Run isolated perf and the
  remaining release suite; validate workflow syntax and native PR CI.
