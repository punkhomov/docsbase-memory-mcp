# CI testing design

## Delta 2026-11-08 — C1

Use a Python standard-library command observer, not a replacement Cargo runner.
Each current test selection has three explicit workflow steps. They share the
same Cargo parameters and target directory; `--no-run --timings` prepares test
binaries, `-- --list` inventories them, and the original command executes them.
Doctests may compile in inventory/execution. Tests may still launch nested builds.

`target/ci-observations/<suite>/<phase>.log` retains merged stdout/stderr;
the adjacent JSON contains provenance, command status and parsed observations.
A periodic heartbeat records wall time and the last output line. Until a command
finishes its state remains `running`, with no invented exit status. Parser output
is diagnostic; it never determines whether a test succeeded. The subprocess exit
code does. Missing executables fail with 127.

Stable Cargo/libtest text supplies target identity, test inventory names and
target-level result counts/durations. Individual-test durations and fixture time
are unavailable from this output. Keep original logs so parsing is auditable.
Do not dump environment variables: only named public build/Actions settings.

Reset diagnostic directories after cache restore, so stale reports cannot be
published as current observations. Artifacts upload with `always()` from each
Rust job, with job/run-attempt-specific names. Logs are flushed while the command runs; cancellation or job timeout can
still prevent upload. Cache metadata describes the cache action's result, not
proof that all compilation was warm or cold.

No new crate or Python package. Keep existing job names, triggers, suite selectors,
timeouts, release checks and search-quality policy. Future deltas cover test
correctness, routing, nextest and measured build reuse separately.
