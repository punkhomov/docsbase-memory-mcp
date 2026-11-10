# Search concurrency progress — #66

2026-11-10: S1 design ready; S2–S4 explicitly require detailed dated design gates.
Ruling: user authorized autonomous current-queue work including PR/merge; machine
mode retains fresh task/final reviews and latest-head four required checks.
Ruling: split #66 into safe increments. S1 prepares bounded readers but leaves
search routing unchanged; singleflight precedes concurrency. #66 stays open.
Design challenge: default acquire timeout would violate CF0; acquire therefore
requires caller absolute deadline and owns no timeout default. Startup eagerly
prepares all readers and fails before serving if preparation fails.

Task S1: implementation verified (base6e14280; cli-full958 passed/0failed/13ignored
across65targets; lean integrations30/0, pool units3/0, startup3/0; full/lean
all-target Clippy, opt-in lint, fmt and diff checks clean). Fresh task review pending.
Ruling: ReadLease field order unlocks its slot before ReturnSlot advertises
availability; no second unbounded mutex wait after deadline-aware admission.
Ruling: Shared reader preparation occurs before listener/state advertisement.
Validation ruling: an intermediate local full run is excluded: concurrent lean
build replaced target/debug/docsbase during full CLI tests (missing local-model
commands). Profiles were rerun sequentially; final sole cli-full run passed.

Task S1: complete (base6e14280; verification above; fresh task review
Spec compliance PASS / Quality APPROVED, no findings).
Ruling: cleanup-failure quarantine is source-verified defensive behavior; real
SQLite tests cover successful rollback/unwind cleanup. No driver-failure injection
or native result is claimed. Exact command/exit observations retained in session
evidence; final-head native CI remains a delivery gate.

2026-11-10: S1 PR106 ready, final source review clean; native CI42803600478
pending. S2a starts in isolated worktree from reviewed8bd6e96 while primary S1
checkout stays immutable. S2a design/task delta settles query flights before code.
Ruling: split S2 query/cold work for independent verification; no deadline default
is invented by the cancellation primitive. S4 owns actual request policy wiring.

S1 delivery: PR106 merged as b568508; required CI42803600478 on tested8bd6e96
passed docs/Linux/macOS/Windows. Native results Linux958 +2artifact, macOS954,
Windows939; 0failed/13ignored, all parent tests retained (+11 each OS). Squash
tree equals tested tree. S2 worktree rebased onto b568508 before implementation.
Task S2a: in progress. TDD confirmed #107: provider unwind leaks blocking marker;
RAII repair passes original regression plus nested-context test (2/0). Query API
RED missing methods/modules, then gated four-request tests found a duplicate
cache/result Arc; owner insertion now shares the same Arc, query suite8/0.
Source-chain/delayed-miss tests added before final lint/full verification.
Ruling: query work uses explicit cancellation state without inventing a request
timeout. Active synchronous work may finish and cache a valid pure query vector
after cancellation; caller sees cancellation, others receive the shared result.
Daemon cancellation/admission ownership/deadline wiring remains S4; dispatch
is still serial. No whole-#66 or cold dense convergence is claimed.

Task S2a: implementation verified (baseb568508; final cli-full970 passed/0failed/
13ignored across65targets; focused query9/0, delayed-miss1/0, blocking-context2/0;
lean hybrid81/0 and integrations42/0; full/lean all-target Clippy, opt-in lint,
fmt/diff clean). Fresh task review pending.
Validation ruling: a local command named nonexistent target search_cost after
all preceding checks passed. Corrected actual target list ran42/0; no failed
product test was rerun or relaxed. Feature builds remained sequential.

2026-11-10: S2a fresh review found Important I1: live admission could inherit
all-cancelled failure before provider work. Scoped fix verified; re-review pending.
RED: three gated schedules fail with the original shared cancellation error:
late admission, selection before wake registration, registration after retirement.
GREEN: flight7/0, query9/0, blocking-context2/0; final cli-full976/0/13ignored
across65targets, lean hybrid87/0 and integrations42/0. Full/lean all-target and
opt-in Clippy, fmt/diff clean; profiles sequential. Evidence and fix-only diff:
`/workspace/scratch/issue-66/s2/i1/` (before snapshots match original review hashes).
Ruling: no-active pre-provider decision publishes internal `Completion::Abandoned`;
live callers repeat cancellation-checked admission and completed-cache recheck.
Provider failures, including `Error::Admission`, are terminal shared results and
are not automatically retried. Internal `run` borrows its key; cache callback is
`FnMut`, expensive work remains `FnOnce`. This preserves short lock phases and
handles callers retaining a flight Arc before wake registration; a wrong ruling
would duplicate paid work or reject live callers, covered by gated regressions.
No task completion, fresh re-review, native S2a checks or whole-#66 convergence
is claimed by this fix entry. No commits/push, real-provider runs or baseline
regeneration performed.

Task S2a: complete (baseb568508; final976/0/13 across65targets; lean87/0+42/0,
focused flight7/query9/blocking2; full/lean all-target and opt-in Clippy,
fmt/diff clean). Fresh task review I1 ADDRESSED with no new Critical/Important
breakage; consolidated Spec compliance PASS / Quality APPROVED, no deferred minors.
Ruling: pre-provider abandonment is an internal retry signal for active callers,
not a provider error. Paid work remains one-shot; actual provider Admission is
terminal. Final whole-slice review and required latest-head native CI remain gates.
