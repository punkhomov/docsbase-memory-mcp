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

## Delta 2026-11-09 — C2/C3

C2 separates input validation from recall/scale fixtures. Both invalid-query
tests build two nonzero vectors in two dimensions and verify a valid query
before the invalid case. The mismatch test covers shorter and longer inputs.
Large recall/persistence fixtures remain unchanged. Exact-scan ID membership
uses a `HashSet`; approximate result length, corpus, thresholds and sample counts
retain their current meaning.

### C3 selection revision — 2026-11-09

Use Cargo's `[[test]] name = "perf_budget", test = false`. Ordinary Cargo test
selection includes all other current/future default targets and doctests; the
existing nightly `--test perf_budget -- --test-threads=1` explicitly opts in.
This replaces the initially planned metadata selector and retains Cargo's own
default selection for every other current/future target kind.

MSRV probes showed `--all-targets` also omits this opt-in target, even when
combined with `--test`. Therefore Linux PR CI and nightly have a separate
`cargo clippy --features cli-full --test perf_budget -- -D warnings` command.
It reuses compilation outputs in the same job. The performance test file is
Linux-only; other native functional checks are unchanged. Six formerly ignored
debug entries become opt-in; no functional execution or performance budget is
removed. Real inventories, explicit performance execution and lint checks prove
the partition. Production code/dependencies remain unchanged.

## Delta 2026-11-09 — C4

The install fixture uses the standard `Output` Debug format already used by
its index helper. A failed process with empty stderr must still show its exit
status and stdout. This is diagnostic-only; preserve the real subprocess,
failure assertion and ETXTBSY-only retry. The observed macOS termination cause
is unknown because the old message discarded its status.

## Delta 2026-11-09 — C5

Return the existing refresh worker's JoinHandle from the private method; callers
explicitly drop it to preserve detached execution. Its private generic helper
accepts a FnOnce warm operation, with DenseCache::warm used in production.
Config resolution, database open, dense snapshot and both real cleanup calls
stay on the same path. No global spy or public test configuration.

Small fixtures open Shared before registering/indexing the project, avoiding
unrelated watcher activity. Policy tests keep real DB/config/vector/cleanup
work, observe warm through per-test channels, and wait for worker completion
under a real watchdog. A positive channel gate establishes ordering and releases
on parent drop. Error tests exercise real directory and disk cleanup with a
small per-instance disk limit; they do not allocate gigabytes or sleep for mtimes.

A separate real-build test indexes ANN_MIN_VECTORS-1 uniquely headed sections
in one Markdown file, verifies exact fallback, appends a section and refreshes
at ANN_MIN_VECTORS. Stored counts are asserted. READY and ann_snapshot are
passive observations, so the waiting path cannot create a false positive build.
Well-spread normalized fake vectors avoid incidental degenerate geometry.
