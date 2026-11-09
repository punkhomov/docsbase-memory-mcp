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

## Delta 2026-11-09 — C6

Use one shared project helper for both corpora. Each fixed-size unique document
creates one chunk; verify document/chunk counts and distinct hashes rather than
assuming a file count is a vector count. Main search retains 600 distinct paths
and dimension64; the 20-document project remains below the fetch clamp. Keep
roots alive through searches and declare the cache before DB handles.

Keep the existing real build gate and persisted ANN read-path counter. An
independent DenseCache with AnnPolicy::Disabled supplies the exact path oracle
for gated search and insufficient-candidate fallback; approximate ANN output
is not required to equal exact output. Existing large recall tests own those
quality thresholds. Check ten published results and repeatability as before,
plus explicit insufficient-candidate diagnostics and exact fallback results.

Temporary timing probes are local verification evidence, not permanent test
instrumentation or CI performance gates. Controlled mutations validate the
oracles, then restore source byte-for-byte. No workflow or production edits.

## Delta 2026-11-09 — C7

Thread a per-call `FnMut(Duration)` wait function through the config-lock
implementation and private model-write boundary. Production passes
`std::thread::sleep`, retaining the current retry policy and real fd-lock guard.
This mirrors the existing watcher retry seam without fake process-global time.
The lock component owns retry values; the model test only observes contention.

A holds the real lock before publishing. B's first failed lock attempt calls
its local wait observer, acknowledges contention and waits on a channel. A
publishes while retaining the lock, exits the critical section, then releases
B's wait gate. B must acquire and read the new state. Independent literal TOML
expectations check both complete stages plus unrelated fields/ranking; the
project write test also exercises the ordinary entry with its real default wait.
The shared environment guard protects ambient data-dir lookup without adding
data-dir overrides. Gate sender drop unblocks a failing child; watchdogs and
completion observation bound waits before joining.

Lock-policy tests supply an explicit fixture lock directory, keep real held
OS locks and record delays. They test exhaustion without invoking the operation,
release/reacquire, and release during a retry; no real backoff is consumed.
The original sleep test writes A before delaying B, so a pre-lock read may
already see A and incorrectly pass. Record that blind spot as test weakness,
not a newly demonstrated production defect. Watcher/backoff work is deferred.

## Delta 2026-11-09 — C8

Keep watcher production code untouched. The existing consumer records terminal
failed batches in sync_jobs with project id, finished state, error and per-path
warnings. The integration test reads those rows after a captured job-id cursor,
canonicalizes warning paths, and accumulates acknowledgements across batches.
A fresh post-downgrade Markdown path disambiguates new work from pre-existing
README events; the original README edit remains part of the scenario.

A healthy edit first changes committed chunk text/generation and the client
search result. Snapshot document ids/hashes, chunks and generation only after
that progress is observed. With the schema downgraded, wait for correlated
rebuild refusals and check invariants during the wait and after completion.
A fresh read-only index view verifies old healthy content remains searchable
and the stale-edit term is absent. Public search must still give the rebuild
hint. Polling is passive and watchdog-bounded, preserving actual OS notify,
debounce and schema admission semantics; no fixed quiet period or global clock.

C8 healthy readiness permits the documented temporary rebuild marker during
commit recovery, as existing MCP watcher tests do; unexpected errors still fail,
and successful committed data plus search remain mandatory.
