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

Task S3.6a: design ready, before implementation (base4a5c28f). Add one
SearchSpec-owned request timeout and immutable original-start operation context
before the pooled phase consumer. Existing quick-call30s supplies the initial
policy; no hidden per-acquire fallback or per-stage budget reset. Actual runtime
adoption/transport cancellation remain future tasks in the same draft PR.

Task S3.6a: complete (base4a5c28f..this task commit; deadline primitive only).
SearchSpec owns global request_timeout_ms30000 with Daemon/Milliseconds
provenance and positive validation; partial config preserves other owner defaults
and project overrides are rejected. SearchOperation consumes the complete policy,
original start and explicit token, retains one checked deadline across clones,
counts queue/phase time and cancels this token on expiry. It starts no timer and
does not claim sync-provider preemption. Nine injected-time regressions and three
new file-resolution tests cover the contract without waits or timing budgets.
Fresh task review Spec PASS / Quality APPROVED, no findings or deferred minors.
Final13 stages exit0 and match all157 current Rust inputs: cli-full1074/0/13
across66targets, operation9/policy16/config68, lean operation9/config74/consumers10,
full/lean all-target and opt-in Clippy/fmt/diff all0. Own test-only lint correction
preceded final sequence; original RED/preflight preserved, no format exception.
Ruling: draft PR112 native preflight at previous reviewed4a5c28f passed all four
required checks; Linux1062+2artifact/macOS1058/Windows1043, 0failed13ignored.
Inventory adds66 per platform against merged110 with none removed. This evidence
does not cover this newer task commit or replace final-head acceptance.
Actual core/pool adoption and S4 timer/transport/dispatcher/timing plus final
whole-feature review/native gates remain pending; #66 remains open.

Task S3.6b: design ready, before implementation (basebf450f1). Actual pooled
core uses BM25+original G before embedding and validates G/config at owned
payload capture; no double query parse or SQL/lease through provider calls.
Shared cold factory has fail-fast prepared admission, no leader personal budget.
Existing fail-fast writer policy is explicit; no hidden priority queue. Persist
real indexed fixture state rather than trusting an in-memory status shortcut.

Task S3.6b: Ruling: focused cli_read exposed an old read-success assertion while
the exclusive project writer lease is held. Keep committed-index reads under a
Tantivy writer handle, require Project/retry under actual project WRITE, retain
list/status availability and prove content recovery after lease release. Raw
pre-correction failure retained in core/preflight. Native prior-head bf450f1
macOS installer SIGKILL is separately tracked in #113 after all112-record
duplicate check; no installer change or blind rerun in this core task.

Task S3.6b: Ruling: full suite exposed19 benchmark/external failures from
build_env's in-memory-only status. Persist and re-read real indexed registry
state; frozen input hashes captured, no score/recording/baseline change. Full
failed evidence retained before-bench-status; no successful full run claimed.

Task S3.6b: complete (basebf450f1..this task commit; actual pooled search core).
Daemon routes search before global Db and captures its original operation before
spawn_blocking. Binding/metadata use prepared readers with explicit deadlines;
SQL drops before Tantivy/vector CPU, providers and flight follower waits.
BM25 captures original G, query embedding owns no preparation resources, cold
leader uses guarded lazy metadata, final guarded payload validates G/config.
Owned finish checks cancellation/deadline and preserves provider/ranking/window/
response contracts; explain total includes original time even for limit0.
Cap1 gates prove status/list/same-project lexical complete during paused rerank;
embedding releases SQL/config/file/global Db, sync/reload cause explicit retry.
Fresh task review Spec PASS / Quality APPROVED, no Critical/Important findings.
Minor M1 was a scratch report count typo; dedicated correction changed only
its final1088 entry to1089, preserving historical disclosure and all source/raw
evidence. No deferred findings; no functional rerun for that report-only fix.
Final23 stages exit0 and match all160 Rust inputs: cli-full1089passed/0failed/
13ignored across66targets; admission4/phases8/pool6+unit4/consumers10/ranking60/
visibility37/benchmark88; lean8/4/6/74/10/28/37/88, all0failed. Full/lean
all-target and opt-in Clippy/fmt/diff pass; all113 frozen inputs unchanged.
Ruling: documented writer-lease refusal/recovery replaces unsafe old read-success
assertion; fixtures persist actual Indexed state. Prior full19 benchmark failures
and zero-limit timing RED retained honestly, no baseline/recording relaxation.
A final extra cleanup-result quarantine test covers both pool admission paths;
its predecessor1088-pass package remains historical, final1089 is authoritative.
S3.6c refresh prepared-pool adoption, S4 transport/timer/group cancellation/
actual-job accounting/queue telemetry and whole-feature final/native gates remain
mandatory. PR112 stays draft and #66 remains open.

Task S3.6c: design ready, before implementation (base437242b). Background
refresh shares prepared capacity and checked lazy metadata; scalar lease drops
before warm, no per-worker Db/default. Busy-pool RED and capacity1 actual warm
verify resource ownership while preserving cleanup and prior refresh contracts.

Task S3.6c: complete (base437242b..this task commit). Shared and background
refresh share the startup-prepared Arc<ReadPool>; short checked scalar capture
releases its reader/config/file guards before guarded lazy warm. Real leader
metadata uses the same prepared-source helper; no unpooled open or personal
query budget. Cleanup still runs on skipped/stale/busy/failed warm. Actual
pool-exhaustion RED0/1 in0.06s and capacity1 seeded warm regressions retained.
Fresh review Spec PASS / Quality APPROVED, no findings. All20 final stages
exit0 and match all160 Rust inputs: full1091passed/0failed/13ignored/66targets;
focused server14/state11/phases8/dense74/ranking60/visibility37; lean14/11/74/
37/28/42/74. Full/lean all-target+opt-in Clippy/fmt/diff pass, all113 frozen
inputs unchanged. Ruling: runtime accessor still returns &ReadPool; const
qualifier removed explicitly because Rust1.88 Arc deref is nonconst.
S4 and whole-feature final review/current-head native gates remain pending;
PR112 stays draft and #66 remains open.

Task S4.1: complete (base84c4134..this task commit). Private checked Flights
work uses all registered live participants, with typed ordinary failure versus
abandonment. Dense checks before/after revision admission, after metadata and
around vector/ANN phases; no leader personal budget enters shared work. Explicit
abandonment retires/wakes with retry for late live joins/retained selections;
ordinary Admission remains shared failure without replay. Already-running sync
phases are not forcibly interrupted or rolled back. Public APIs/run adapter and
background participation remain unchanged. Real all-cancelled metadata RED0/1
in0.01s proves old unnecessary resident publication; final regression leaves no
entries/flight/listeners and a real live retry initializes. Two additional
deterministic retirement races preserve one live work and cleanup.
Fresh review Spec PASS / Quality APPROVED, no findings. All20 final stages0
match160 Rust inputs: full1094passed/0failed/13ignored/66targets; flight9/dense
admission5/query9/dense75/core8/server14/ranking60; lean9/9/75/28/42/74. Full/
lean all-target+opt-in Clippy/fmt/diff pass, frozen113 unchanged. No deferred
findings. Prior C84c4134 native42899966719 passed all4 checks, +2 tests per OS
versus B437 with none removed; does not replace this/final-head native gates.
S4 transport/timer/EOF/original retry budget/actual-job+reply+connection accounting,
queue diagnostics/product docs and whole-feature final review/native remain;
PR112 stays draft and #66 remains open.

Task S4.2a: design ready, before implementation (basea2b03c5). Persistent private
bounded frame state is required before selecting cancellation against reads;
manual-poll/drop regression must demonstrate actual old partial-byte loss.
Wire behavior and protocol bound remain, remaining S4/final/native mandatory.

Task S4.2a: complete (basea2b03c5..this task commit). Private FrameReader owns
bounded pending bytes across dropped read futures; daemon uses it in the idle
janitor select. Copy and consume have no cancellation point, existing protocol
cap/line/EOF/incomplete/error/close behavior preserved. Real duplex manual-poll/
drop RED0/1in0.00s demonstrates old prefix loss; moved regression retains exact
expected bytes/cancellation sequence with only reader API adaptations.
Fresh review Spec PASS / Quality APPROVED, no Critical/Important findings.
Minor M1 corrected by a dedicated agent in design wording only: actual existing
tests cover exact-limit/oversize/frontend behavior, while partial-EOF semantic
preservation is established by inspection. All161 Rust hashes and113 frozen
inputs unchanged by this correction; no functional rerun for a docs-only fix.
No deferred findings. All17 same-source stages exit0: full1095passed/0failed/
13ignored across66targets; framing1/wire28/core8/server14, lean1/28/8/14/42/74;
full/lean all-target+opt-in Clippy/fmt/diff pass, all113 frozen inputs unchanged.
Already-passing focused stages reused across two sequential same-source runners.
Prior a2b03c5 native42902721351 passed all4 checks, +3 tests per OS with none
removed versus C84; Linux1094+2artifact/macOS1090/Windows1075,0failed13ignored.
That prior-head evidence does not replace this/final-head native acceptance.
Remaining S4 active-call timer/EOF/scoped capabilities/original frontend budget/
retry/actual-job+reply+connection accounting/queue/docs/wholefinal/native remain;
PR112 stays draft and #66 remains open.

Task S4.1: design ready, before implementation (base84c4134). Checked shared
phase interest uses all participants and typed abandonment. Cold work stops at
the next boundary after all cancel; late live arrivals retry normal admission.
No transport/timer delivery claim; remaining S4/final/native gates mandatory.

Task S4.2b: design ready, before implementation (base76b0c0b). Original absolute
deadline driver/cancellation-on-drop plus shared actual-job/reply/connection
activity. Gated actual dispatcher abandonment RED precedes production edits;
injected expiry tests avoid clock/sleep assumptions. Remaining S4.3/final/native
mandatory, PR112 remains draft and #66 remains open.

Task S4.2b: complete (base76b0c0b..this task commit). Async dispatcher retains
one original SearchOperation deadline; expiry or abandoned waiter cancels that
caller and prevents its next stage, without preempting an executing provider.
Normal completion disarms cancellation and rechecks the original budget even
when a completed worker wins the ready race. Activity increments once and is
shared through actual blocking work and wire reply; actual admitted connection
capacity survives caller timeout until both owners finish. Last drop releases
its capacity reference before counter/event notification. Expired searches
close after their error reply; legacy hello/session/half-close policy preserved.
Actual gated dispatcher RED0/1in0.07s proved old post-abandonment rerank; expired
completed-worker RED0/1in0.00s proved the ready-race budget hole before correction.
Fresh task review Spec PASS / Quality APPROVED, no Critical/Important findings.
Minor M1 safety watchdogs corrected by a dedicated agent only in request.rs
cfg(test); production prefix byte-identical and all other161 Rust files unchanged.
Final post-fix9 commands exit0 matching all162 current hashes: full cli-full1099
passed/0failed/13ignored across66targets, full/lean driver3, full/lean all-target
and opt-in Clippy/fmt/diff pass; all113 frozen inputs unchanged. Prior19-stage
evidence retained honestly for pre-watchdog source, including core9/server14/
wire28/lifecycle25 and lean9/14/28/25/42/74. No deferred findings or baseline/
provider/window/default changes. Prior76 native42905527539 all4green/+1eachOS/
none removed is prior-head evidence, not final acceptance.
Remaining S4.3 scoped active EOF/capabilities/reply deadline/original frontend
queue+retry budget/rmcp cancellation/queue diagnostics/docs/wholefinal/native
remain mandatory. PR112 stays draft and #66 remains open.

Task S4.3a: design ready, before implementation (base507f223). Scoped capability/
context, active transport cancellation, original reply deadline and explicit
queue diagnostics. Actual Hello RED precedes production code. Legacy framing/
half-close, physical job ownership and old public metadata signatures preserved;
S4.3b/final/native remain mandatory, PR112 draft and #66 open.

Task S4.3a: Ruling: place scoped call/reply helpers in daemon/scoped.rs as a
private child of server, with its focused tests. This keeps the connection loop
and legacy dispatcher readable without expanding public APIs; it consumes the
same planned Scope/Activity/operation/framing contracts. No scope reduction.

Task S4.3a: complete (base507f223, scoped protocol/server lifetime and queue
telemetry; tests:20 sequential commands all exit0, full cli-full1116passed/
0failed/13ignored across66targets,163tested Rust hashes; scoped7/operation12/
meta5/wire34 full+lean, phases9/flight9/lifecycle25, full+lean all-target and
opt-in Clippy/fmt/diff pass; frozen113 unchanged). Fresh review PASS/APPROVED,
no Critical/Important. Sole Minor M1 public error rustdoc fixed by dedicated
agent; exact comment-only diff verified, other162Rust/113frozen unchanged,
post-fix fmt/diff0. Functional evidence applies to unchanged executable/test
code; the one rustdoc hash differs from the20-stage map. No functional rerun
for a documentation-only correction, no deferred minors.
Task S4.3a: Ruling: original raw reviews/maps remain immutable; m1 addendum
records final163 source identity and precise comment-only equivalence. Prior
reviews/full regressions cover unchanged S1–S4.2b seams. Current native checks,
mandatory S4.3b canonical callers and whole-feature final acceptance remain;
PR112 stays draft, #66 remains open. No hard synchronous preemption promised.

Task S4.3b: Ruling: implement canonical caller adoption in b1 async IPC/driver,
b2 MCP cancellation/lease/retry, b3 CLI/docs. This keeps native transport tests
and compatibility review bounded without weakening S4 acceptance; all stages
stay in draft PR112, then whole-feature final review/latest-head4native. Dated
requirements/design/tasks recorded before b1 source implementation, base4c63610.

Task S4.3a: native preflight4c63610/run42927420839 complete docs/linux/macos/
windows SUCCESS. Captured diagnostic archives independently add17tests perOS
against507f223, none removed; native scoped EOF case present on allOS.
Linux1116+2artifact/macOS1112/Windows1097,0failed13ignored. This is prior-task
server evidence, not acceptance for S4.3b/final head.

Task S4.3b1: complete (base4c63610, asynchronous native IPC and explicit scoped
search operation; tests:11 sequential commands all exit0, full cli-full1124/
0/13 across66targets; tested165 Rust snapshot, frozen113 unchanged; full+lean
client8/wire34, full+lean all-target+opt-in Clippy/fmt/diff0). After commands,
own inspection corrected one encoding-error rustdoc line only: final165 differs
from tested165 at exactly that comment, other164 and executable/test code
unchanged; exact before/diff/final map and separate fmt/diff0 recorded. No
functional rerun for a comment-only correction or identical-final-hash claim.
Fresh review Spec PASS; sole Minor M1 evidence-summary wording fixed by a
dedicated agent without source/spec/raw-manifest changes. Scoped re-review M1
ADDRESSED, no new Critical/Important, resulting Quality APPROVED; no deferred
minors. Root independently verified final165/raw11/frozen113 and exact comment.
Task S4.3b1: Ruling: generic async connect/call admission belongs to caller
composition; search itself uses the original absolute driver. This task supplies
the client, not canonical MCP/CLI adoption. Mandatory b2/b3 cancellation/lease/
retry/CLI/docs and whole-feature final review/final-head4native remain; PR112
stays draft and #66 remains open. No provider preemption or CappedRead bug claim.

Task S4.3b2: design ready, baseca2c6db. First prove the existing CappedRead
newline-boundary candidate against its actual implementation/actual frame cap;
no confirmed issue or fix claim yet. Probe files/test expectations recorded
before source changes. Then canonical MCP adoption under the binding b2 design.

Task S4.3b1: native preflightca2c6db/run42930505074 complete docs/linux/macos/
windows SUCCESS. Diagnostic inventories independently add8tests perOS versus
4c63610, none removed; Linux1124+2artifact/macOS1120/Windows1105,0failed13ignored.
This is prior-client-task evidence, not acceptance for B2/final head.

Task S4.3b2: complete (baseca2c6db; async MCP original scope, actual rmcp
cancellation, owned connection lease/retry and #114 same-limit frame correction).
Original13-stage snapshot167Rust/full1137/0/13/66 remains historical evidence.
Fresh review found Important I1: post-index Admission/Protocol was swallowed,
restoring a rejected lease. Dedicated fix agent added actual native unbound→
index→rebind rejection regressions: RED1pass/2failin0.01 (only lease-discard
assertions fail; primary success/no replay already pass, Transport preserved),
then minimal helper propagation of Transport/Admission/Protocol gives3GREEN.
Final14 sequential fix stages all exit0 and match current167Rust: full cli-full
1140passed/0failed/13ignored across66targets, full+lean frontend12/frame4/wire34,
full+lean all-target+opt-in Clippy/fmt/diff0. Actual order full-feature focused/
lints→lean focused/lints→fmt/diff→final full, no concurrency. All113frozen and
421non-owned inputs unchanged; only two owned source/test files differ from the
reviewed original snapshot. Root independently verified current167/all14 maps,
exactRED source/same final tests, frozen inputs and last-summary full totals.
Scoped original-reviewer re-review I1 ADDRESSED/no new Critical or Important;
resulting Spec PASS/Quality APPROVED, no deferred minors. No index replay or
primary-result loss on failed post-index bind.
Task S4.3b2: Ruling: #114 was filed only after actual-cap wrapper RED and all31
open/closed issue duplicate check. Server creation00:23:02Z follows RED finish
00:22:07.207816; retrieved issue metadata matches exact POST input. Historical
POST response was tool output only, not an invented archived response. This is
read-boundary-dependent framing failure, not a claim of unlimited memory growth.
B3 CLI/original policy/direct fallback/product docs and whole-feature final
review/latest-head4native remain mandatory; PR112 stays draft, #66/#114 open
until actual delivery/merge. No environment changes or hard preemption promise.

Task S4.3b3: design ready before code, base8312ed6. CLI async scoped composition,
one global policy/operation across initial connect/direct fallback, safe sync
runtime bridge and product contract documentation. Explicit current-thread
runtime error avoids unsupported sync nesting; normal CLI remains a plain thread.
Actual old-capability CLI regression precedes production changes. B2 source has
clean scoped re-review and was committed; current B2 native gates pending.

Task S4.3b2: native preflight8312ed6/run42937123060 complete docs/linux/macos/
windows SUCCESS. Diagnostic inventories independently add16tests perOS versus
ca2c6db, none removed; Linux1140+2artifact/macOS1136/Windows1121,0failed13ignored.
This verifies MCP/frame/rebind changes on allOS, not the final B3/feature head.

Task S4.3b3: complete (base8312ed6; CLI scoped async bridge, original complete
owner policy/same direct snapshot, initial-connect-only fallback, product docs).
Actual legacyCLI on indexeddata RED0/1in0.07 unexpectedly succeeds; bridge API
compileRED follows. Four exact saved source/test snapshots verify raw inputs.
Five unit cases cover plain/multithread/current-thread runtime behavior and
expired/live original operation on missing endpoint. Two nativeCLI additions
preserve refusal/error rather than hiding it behind valid direct results.
Final16 sequential stages all exit0/current168Rust: full cli-full1147passed/
0failed/13ignored across66targets; full+lean CLIunit5/routing+read18/IPC+MCP34/
operation12; all-target+opt-in Clippy/fmt/diff0, docs hygiene/5newanchors/request
JSON0. All113 frozen inputs unchanged. Own first-green let-else lint preflight
retained separately. Fresh task review Spec PASS/Quality APPROVED, no findings
or deferred minors. Generic CLI helper/blocking Client/Options preserved.
Task S4.3b3: Ruling: commit the approved task locally, perform mandatory fresh
whole-feature review on that committed head, then push the final reviewed head
once for required native checks. No gate is skipped; this avoids starting an
intermediate native workflow before the final review ledger is committed.
Whole-feature convergence/latest-head4native remain pending; PR112 stays draft,
#66/#114 remain open until actual completion/merge. No environment changes.

Task S4.final.I1: complete (basea012723; second ANN load checks shared interest
before publication/fallback/build, preserving typed abandonment and live peers).
Dedicated finding-fix agent first observed meaningful RED: 2 live controls pass,
3 canceled cases fail on unwanted resident/build state. Actual SQLite, vector
cache and persisted graph IO are gated per instance; no global hook or timer.
Final24 sequential commands all exit0/current168Rust: cli-full1152passed/0failed/
13ignored across66targets; full/lean dense/flight/phases/scoped/client/wire and
all-target+opt-in Clippy, fmt/diff; full query/server/ANN also pass. All113frozen
inputs unchanged. Final tests differ from RED solely by a documented redundant
reference lint correction. Root independently checked raw maps, stage ordering,
full last-summary totals, exact correction diff and frozen inputs. Original
whole-feature reviewer scoped re-review: I1 ADDRESSED, no new Critical/Important;
no code gap remains from that review. Final-head native SC-4 is still pending.
Task S4.final.I1: Ruling: late read-only dependency audit separately identified
Windows busy-pipe connection paths using unbounded blocking waits. This is a
new bounded native-compatibility task, not a reopening of the closed ANN finding.
Confirm exact dependency behavior and meaningful native regression before its
production fix. Final convergence/PR readiness waits for that task review and
latest-head docs/linux/macos/windows; #66/#114 remain open.

Task S4.3c: design ready, baseb9d0355; source-confirmed locked dependency uses
unbounded Windows busy-pipe waits. Actual native regression before production
change is pending. This task closes the connection-admission hole in the
existing FR-5/FR-6/SC-4 flow; not an async rewrite or legacy-client change.
Ruling: the prior whole-feature code review passed after its ANN I1 correction,
but did not cover this late dependency finding. Add a bounded task and fresh
review, then supplemental whole-feature acceptance/latest-head native evidence.

Task S4.3c: Ruling: actual Windows TDD requires an intermediate draft push of
the test-only head; Linux cannot execute this native API. This supersedes the
previous single-final-push optimization, without weakening any gate. The new
Windows regression is expected RED; no production fix is included and this
intermediate head cannot be merged. Test formatting preflight was corrected
before this head (no behavior change).

Task S4.3c: native RED observed on3fa6ae7/run42948263954/job130286191228:
actual adapter Pending versus Ready(Some(TimedOut)),449other lib tests pass;
no production correction existed at that run. Runtime cleanup completed.
#115 filed after RED and all114records/32issues duplicate check. Minimal raw
zero-wait connection/probe correction and four real endpoint controls added;
validation/fresh review pending. This candidate stays draft, not mergeable.

Task S4.3c: native candidate266b089/run42949328740 docs/Linux/macOS pass.
Windows453pass/1fail lib: original busy-connect RED is GREEN; available byte IO,
absent/invalid and busy-probe presence controls pass. Sole failure is an extra
post-Drop immediate-absence assertion. Source evidence (Mio Arc ownership and
IOCP completion) proves that assertion assumes an unsupported teardown instant.
Remove it, retain actual busy presence, dedicated never-created absent endpoint
and existing joined-runtime removal regressions. No production change, timeout,
sleep, retry or gate weakening. Local11 stages on266 passed: cli-full1152/0/13/66,
full/lean relevant65/client8/lints/fmt/docs0/frozen113. Their exact old snapshot
is retained; only a cfgWindows test assertion/comment now differs, so do not
claim final-byte map equality. Final native CI reruns all platforms on new head;
local Linux functional code is unchanged and a duplicate local full run adds no
coverage. Fresh task review remains pending until final native evidence.

Task S4.3c: native9cfce9e/run42950674651 docs/Linux/macOS pass, all5newplatform
controls and454Windowslib pass. FullWindows later fails project_error_has_instruction
with connect-after-startup Transport instead of Project. No rerun/extra retry.
Source-backed candidate: connect-and-close liveness itself consumes the one
pipe instance before the real connection; add deterministic native no-consumption
RED plus NUL-live-prefix control BEFORE further production changes. Narrow
Win32 NOWAIT probe exception is designed above, not yet implemented. PR112
stays draft, fresh review/full acceptance pending.
