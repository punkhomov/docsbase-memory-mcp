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

S2a delivery: PR108 merged as2498711; #107 complete. Reviewed2f8d32b native
CI42813054338 passed all four required checks: Linux976 +2artifact/macOS972/
Windows957, zero failures/13ignored, +18 each OS/no removed tests; squash tree
equals tested tree. #66 remains open.
Ruling: GitHub interpreted a negated closing-keyword phrase in PR text as a
closing reference; #66 was immediately reopened and text corrected. Future
milestone descriptions use the unambiguous phrase "remains open".
Task S2b: design ready, before implementation. Generation publication ordering
and owned SQL snapshot/lazy cold coalescing are scoped here; config revision
authority remains S3 before parallel dispatch. No new runtime numeric default.
Ruling: the existing ANN retry fixture used a different ANN constructor root
from its Db root solely to force a dump failure. Under the explicit one-root
contract this correctly fails admission before the build. Keep the same dump
failure/recovery assertion by blocking the actual project ANN directory with a
file inside its real cache root; retry backoff and quality thresholds are unchanged.
Task S2b: complete (base2498711..this task commit; lazy cold initialization and
owned metadata). Fresh task review: Spec compliance PASS / Quality APPROVED,
no findings or deferred minors. Final local cli-full996/0/13ignored across65
targets; focused dense/metadata68 and ANN16; lean hybrid107 +integrations42;
full/lean all-target and opt-in Clippy, fmt/diff all exit0 on frozen source.
Ruling: identical-work coalescing and generation ordering are delivered here;
config-revision authority, coherent cross-store payload ownership and daemon
concurrency remain S3/S4. Latest-head native CI and whole-slice final review
are still delivery gates. #66 remains open.
Task S3.1: design ready, before implementation (base d7bb9dc, S2b PR110 CI
pending). Local tasks compose one eventual S3 PR; rebase onto the S2b squash
before delivery. No S3.1 implementation or coherent response claim yet.
S2b delivery: PR110 squash feb8df2, tree equal to reviewed d7bb9dc. Native
CI42823301698 passed docs/linux/macOS/Windows: 996+2artifact/992/977,
zero failures/13ignored, +20 tests each OS/no removed. Post-merge main
CI42825623847 also passed. #66 remains open.
Task S3.1: complete (basefeb8df2..this task commit; shared project lease and
pending-doc predicate only). Fresh review PASS / APPROVED, no findings/minors.
Final cli-full1004/0/13ignored across65targets; focused readers6/pending2/
writer integrations23; lean6+2+11. Full/lean all-target+opt-in Clippy and fmt/diff
passed, all13 stage hashes match. Ruling: test harness now always supplies the
isolated fixture root; /tmp cloud .git markers must not merge test projects.
Owned payload, byte policies, config authority and actual search/warm adoption
remain future S3 tasks. One whole-S3 final review/native CI gate before its PR.
Task S3.2: design ready, before implementation (basef2e762a). Новые budgets
принадлежат SearchSpec и global parser; отдельный S3.2 PR не создаётся, до
whole-S3 delivery должны появиться реальные consumers. Legacy config JSON
сохраняется, versioned effective UI остаётся #78.
Task S3.2: complete (basef2e762a..this task commit; owner/parser/byte permits).
Fresh review PASS / APPROVED, no Critical/Important findings. Minor M1
(duplicate inventory ID) fixed by separate implementer: payload rows I70/I71,
all83 inventory IDs unique, earlier lexical I68/reference preserved; Rust
unchanged. No outstanding minors. Full1017/0/13ignored across66targets,
focused13/config68, lean71; full/lean all-target+opt-in Clippy/fmt/diff pass.
Ruling: one final source documentation-line correction has exact full-file
equivalence proof; functional tests were not repeated for a comment-only edit,
final lint/format pin current source. SearchSpec impact and project inheritance
also verified by source review using the existing common resolver contract.
Actual SQL capture, data-before-permit drop order, production ownership and
config authority remain future tasks; S3.2 is not independently shippable.
Task S3.3: design ready, before implementation (base56cd4a1). SQL byte preflight
опирается на проверенный bundled SQLite/direct-column octet_length, UTF-16
upper bound и checked charge; data освобождается раньше финального permit.
Код capture ещё отсутствует, production adoption/S4 не объявлены выполненными.
Task S3.3: complete (base56cd4a1..this task commit; owned SQL capture only).
Fresh task review PASS / APPROVED, no findings or deferred minors. Final
cli-full1035/0/13ignored across66targets; capture18/policy13/metadata4;
lean capture18/config71; full/lean all-target+opt-in Clippy/fmt/diff exit0,
all12 manifests match current153 Rust inputs. Strong UTF-16 CJK-heavy cases
exercise the byte upper bound; actual data-drop gates cover success/error/
unwind before permit release, real SQLite decode failure preserves txn ownership.
Ruling: preflight uses direct octet_length columns in the pinned bundled SQLite,
not CAST materialization; i64 SQL lengths convert with checked unsigned range.
Production consumers/config authority, coherent cross-store preparation and
S4 remain outstanding. No partial delivery of unapplied search keys.

Task S3.4: Ruling: owned response consumers first, before revision/pool adoption
— a bounded SQL-free tail can be tested without prematurely enabling parallel
search. Remaining S3/S4 ship together only after resource/deadline authority and
native acceptance; #66 remains open until all requirements are met. Pre-code
contract recorded on base71a3746; no tail/context implementation exists yet.

Task S3.4: Ruling: config_runtime polling waits for an actual successful search
through temporary Project state instead of asserting success during a pending
watcher update — old helper failed in the full suite after safe capture adoption.
Never convert an error to empty hits; preserve other error failures and existing
watchdog. This test contract change is explicit and gets a new wire regression.

Task S3.4: Ruling (2026-11-10, review I1): readiness retries share the enclosing
watcher poll's absolute deadline and reject success at/after it; the seed-file
assertion uses its recorded in-budget result. Standalone searches retain the
existing watchdog, known Project-only retries and actual-success requirement.
Injected-clock regressions cover deadline exhaustion, late success, recovery
and immediate fatal errors without sleeps or elapsed wall-time assertions.

Task S3.4: complete (base71a3746..this task commit; owned response consumers).
SearchContext/search_docs_in explicitly receive admission/cancellation; daemon
owns one global Config.search allowance, CLI passes its resolved policy, old
signature is a pure owner-resolved standalone adapter. PreparedSearch rerank,
filters/rendering and explain generation consume captured data without SQL.
Ten gated/SQLite ownership tests plus3CLI/IPC regressions; five deterministic
clock tests fix I1 without actual waiting or extending the watcher watchdog.
Fresh task review I1 fixed by a separate implementer; scoped re-review ADDRESSED,
consolidated Spec PASS / Quality APPROVED, no remaining findings/minors.
Final cargo test --locked --features cli-full --no-fail-fast:1053passed/0failed/
13ignored across66targets; deadline5/runtime14, lean runtime14/visibility37/
consumers10/integrations42/routing27/config71, all0failed. Full/lean all-target
and opt-in Clippy/fmt/diff exit0; all15 final manifests match155 current Rust
inputs. Original pre-I1 evidence remains unchanged; final I1 evidence is the
current authority. Earlier existing10-second soak smoke1pass; no nightly settings,
ignore markers, quality fixtures, provider windows or latency thresholds changed.
Ruling: S3.5 config/preparation authority and actual short pool/cross-store phases,
S4 caller deadlines/dispatch/cancellation/timing and whole-feature native gates
remain mandatory; this task does not complete #66. No partial delivery claim.

Task S3.5: Ruling: separate reload serialiser and preparation revision gate,
with model construction outside state/preparation locks — preserve ordered
atomic reopen while allowing old coherent reads. Runtime memory invalidation
clears entry/digest only, preserving last-query outcomes and persisted artifacts;
no destructive invalidation. Pre-code contract recorded on base22019f7.

Task S3.5: complete (base22019f7..this task commit; revision authority only).
Stable ProjectCell separates reload serialisation, preparation gate and state;
model construction holds neither state nor preparation. Semantic config change
clears resident entry/digest under publication WRITE; query cache/outcomes,
persisted ANN and actual worker reservation remain intact. Delayed old warm
validates its captured revision before work, preserving cleanup on rejection.
Fresh task review Spec PASS / Quality APPROVED; minor M1 fixed in-task by a
separate implementer, scoped re-review ADDRESSED with no remaining findings.
Ruling: the publication test observes actual preparation try_write contention,
probes both old config/stages Arcs without blocking, then releases/joins before
assertions. Controlled state-before-preparation mutation fails in0.01s; restored
GREEN passes. No ordering sleeps or watchdog changes.
Final functional cli-full1062passed/0failed/13ignored across66targets; focused
state11/memory2/server12/dense70/ranking59/visibility37, lean11/12/70/42/27/37/71.
All full/lean all-target and opt-in Clippy/fmt/diff checks pass. Explicit evidence
exception: the full/lean functional run precedes one test-only whitespace reflow;
saved rustfmt output equals current bytes, other154 Rust files (all production)
are identical. Raw pre-format manifests retain their original hashes. Current
targeted GREEN/scopes and lint/format checks were rerun; no redundant full run.
Actual search pool/cross-store phases and S4 deadlines/dispatch/cancellation/
timing plus whole-feature review/native gates remain mandatory before delivery.
