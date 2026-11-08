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
