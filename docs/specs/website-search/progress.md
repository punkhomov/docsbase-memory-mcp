# #79 progress

2026-11-13: agent B starts client stage C from4c9ff9f in isolated worktree.
User authorized design/implementation/PR/merge continuously. No need to ask gates
again; independent machine reviews remain mandatory. #79 not complete after C.
Spec/design/tasks challenge: exactbuild required; read-only binding avoids index/
backfill; u64 losslessness; no replay; no arbitrary roots/tools; bounded queues;
sameuser/PID deployment; full corpus acceptance deferred explicitly, not waived.
A/B interface recorded in design.md. No storage/vector/index/settings migration
code changed. Server composition ownership to be settled per integration PR.

2026-11-13: plan review fix pass (до реализации, independent re-review pending).
Ruling: ingress timestamp/absolute deadline захватываются до validation и
serialization — preprocessing должен расходовать общий budget; C2 требует clock
tests на elapsed/remaining и отсутствие admission/I/O при expiry. Цена ошибки —
операция превышает заявленный timeout на время preprocessing.
Ruling: expectedSchemaVersion обязателен и сверяется с positive safe integer
Hello.schema_version до bind — build_id() содержит только Rust package version,
не source fingerprint; matching build не доказывает совпадение schema. C1 требует
config/Hello invalid/mismatch tests, включая matching build. Цена ошибки —
несовместимый daemon допускается до read-only binding/search.
Minor 3 исправлен в плане: chunk_id raw token ограничен unsigned integer syntax и
0..2^64-1, output canonical decimal string; C1 требует 0/MAX/MAX+1/negative/-0/
fraction/exponent tests. Production parser ещё не реализован.
Requirements/design/tasks изменены с dated delta markers. Production code/tests
не добавлялись; фиксируется план тестов, а не их выполнение. Fix diff и notes:
/workspace/scratch/agent-b/79/plan-fix.{diff,md}; independent review делает controller.

2026-11-13 C1 verification (review pending): actual RED c1-red.log shows missing
connection.ts; additional schema-range RED c1-schema-red.log. GREEN: Node24.22.3,
17 passed /0 failed; package tsc build/check exit0, diff check exit0. Evidence:
/workspace/scratch/agent-b/79/c1-{red,schema-red,green}.log. Complete mandatory policy,
read-only bind, exact schema/build/capabilities, capped contiguous framing buffer,
lossless u64 and runtime citation checks. Dev-only @types/node24.19.2 and existing
TS5.9.3; pnpm normal 14-day policy accepted. Generated dist/node_modules ignored.
C2/C3 and whole #79 remain incomplete.

Task C1: complete (2026-11-13): transport + checked DTOs/settings, actual RED→GREEN,
final27 passed/0 failed; tsc build/check, package-local Biome format check and
source diff check exit0. Web check0errors/0warnings; pnpm audit moderate gate0
(existing Web dev deepmerge low advisory unchanged). Fresh review initially found
3 Important: Windows UNC endpoint, unsafe score integer exemption, missing schema/
u64 boundary tests. Separate implementer fixed all with actual RED23pass/4fail
thenGREEN27/0; original reviewer scoped re-review PASS/APPROVED, no new findings.
Evidence /workspace/scratch/agent-b/79/c1/{review.md,fix-rereview.md,fix/}.
Ruling: native schema/version fields have the Rust u32 representability bound;
platform is a captured capability, not runtime configurable policy. Biome sibling
path invocation hit upstream ignore-root panic; package-local VCS-disabled scratch
config works without changing repo settings. No deferred C1 minors.

2026-11-13 C2 verification (review pending): RED missing index.ts, additional
RED pre-aborted work prepared args; RED custom tags.toJSON invoked twice.
Implemented ingress operation across preparation/queue/connect/Hello/bind/reply,
separate bounded FIFO pools, owned bounded search args, no replay/autostart/admin,
explicit close and typed errors. GREEN44/0 (17client+27transport), tsc build/check0,
diffcheck0. Deterministic clocks/timers cover preprocessing, Hello, queued expiry
and context elapsed/remaining/queue, without sleeps or latency assertions.
Evidence /workspace/scratch/agent-b/79/c2-{red,aborted-red,owned-red,green,build,check}.log.

Task C2: complete (2026-11-13): public typed client, search/control FIFO pools,
original operation and cancellation, no replay, explicit close, bounded owned args,
package API/docs. Fresh review Important: timer-only transport could write after
synchronous encoding/Hello decode crossed deadline. Separate implementer added
mandatory caller-owned OperationGuard and checks through connection/open/exchange;
4 actual late-frame regressions RED0/4→GREEN48/0, build/check/format/diff0. Original
reviewer scoped re-review ADDRESSED, PASS/APPROVED, no new findings. Evidence:
/workspace/scratch/agent-b/79/c2/{review.md,fix-rereview.md,fix/}. No deferred minors.
C3/native/package acceptance and FR-S* still pending; #79 remains open.

2026-11-13 user ruling: real corpus/load/machine пока предоставить не может,
«можно скипнуть наверно». FR-S4 measurement deferred, not verified and no longer
blocks shipping/queue. No capacity promises or synthetic replacement. Runtime
server FR-S1..S3 remains required before #79 implementation completion.

2026-11-13 C3 local resource ruling: source-exact Node50/0, native2/0 + lean
fixture1/0, packed JS/external TS consumer and full+opt-in Clippy passed. Full
Rust suite failed during linking, before executing tests: ENOSPC; agent B target
reached32GiB, filesystem100%. Evidence c3-full-suite.log/c3-verification.log.
Only own disposable target-agent-b removed (source/evidence/A target preserved).
Repeat local Rust verification with CARGO_PROFILE_DEV_DEBUG=0,
CARGO_PROFILE_TEST_DEBUG=0, CARGO_INCREMENTAL=0 to fit disk; debug assertions,
features, test expectations and CI profiles unchanged. No bypass/skip/rebaseline.

2026-11-13 C3 local verification (fresh review pending): Rust-generated fixture
RED0/1 + Node fixture RED0/2, canonical generated GREEN; full/lean preserve_order
ordering discrepancy reproduced then canonicalized without numeric coercion.
Native root alias/junction fixture exposed path-string assumption RED, corrected
with realpath; no SDK production change. Final Node50/0 + tsc build/check0, real
packed ESM/external TS consumer0, native full2/0 + lean fixture1/0, fmt/diff0,
full all-target and opt-in Clippy0. Local full cli-full (debug symbols off only)
1341 passed/0 failed/18 ignored across83targets; original374 source/input hashes
unchanged before/after. ~3GiB target versus32GiB failing debug-symbol target.
Evidence c3-full-counts.json, c3-source-sha256.json and c3-*.log in scratch79.
Native CI remains pending until PR exact head; Linux checks do not prove other OS.
Real20k run user-deferred; serverFR-S1..S3 remain next phases. #79 not closed.

2026-11-13 C3 Important review fix (scoped re-review pending): nightly's full
release cli-full suite now explicitly builds packages/client in its existing
preparation step. Audited all workflows and agent/local entrypoints; required
native CI and AGENTS already prepare the client. Corrected docs/WEB full-test
sequence and documented generated-export prerequisite in IPC/CI/current specs.
CLI-only builds, compile-only checks, lean and selected non-Node suites unchanged.
Actual isolated source-copy RED: public package import fails ERR_MODULE_NOT_FOUND
for absent dist/index.js (exit1). GREEN: actual pnpm package build exit0, same
unchanged import succeeds exit0. All-workflow actionlint, static prerequisite/
preserved-Cargo-command/check-name audit and source diff check exit0. Fix changes
only YAML/docs; no Cargo rerun or Node suite rerun needed after unchanged runtime
and tests. Evidence /workspace/scratch/agent-b/79/c3/fix/. Exact-head native CI
and original reviewer's scoped re-review remain pending; no new acceptance claim.

Task C3: local implementation complete (2026-11-13): source-native integration,
Rust-owned canonical fixture, packed JS/TS check, CI/docs. Fresh task review found
one Important: nightly full release tests lacked SDK build. Separate implementer
reproduced clean missingdist RED1→actualbuild/import GREEN0; fixed nightly + current
Web instructions, audited other selected-target workflows and actionlint0. Original
reviewer scoped re-review ADDRESSED/PASS/APPROVED, no new findings. Evidence
/workspace/scratch/agent-b/79/c3/{review.md,fix-rereview.md,fix/}. No deferred minors.
PR exact-head native4 and most-capable whole-stage review remain required before
merge; no other-OS or full #79 completion claim. Next rebase onto A's CF7 main254af73
and verify combined source; no changes to A-owned storage implementation here.

2026-11-13 nativeCI43422791379 head570d704: docs/linux/macos passed, Windows failed
new native fixture's project lookup before daemon startup/search. Original log
has no actual compared path spellings, so exact spelling cause not established.
Separate implementer replaced fixture-only string equality with followed bigint
filesystem dev/ino identity and exactly-one matching registered row; same-name
wrong root/ambiguity cannot select a project. Three test files only, no SDK/Rust
production changes. Actual Windows RED retained; extra lookup RED before fix;
Node55/0, local native2/0, build/check/format/diff0. Original C3 task scoped review
and most-capable final scoped review PASS/APPROVED, no new findings/minors.
Evidence /workspace/scratch/agent-b/79/windows-fix/. Fresh exact-head native4 still
required, no blind rerun or Windows skip. User-deferred corpus remains unverified.

2026-11-13 nativeCI43426206657 headf03027d: docs/linux/macos passed; Windows lookup
now passed and real daemon started, then fixture connected to an absent pipe.
Actual Windows ENOENT log retained, actual daemon pipe hash not printed. Before
any fix, standalone proof linked the production platform::pipe_name and showed
before/after cache creation via canonical alias changes its result; stale-parent
endpoint actualdaemon RED confirms dependency. Separate implementer changed only
three testfiles: matching-PID daemon.json.socket is authoritative before first
realclient; Windows bare pipe name qualified per serialization; restart asserts
stable recorded endpoint and existingclient reconnects. Initial absent/noCache
negative retained. Staleendpoint RED→GREEN, Node58/0, native2/0, build/check/format/
diff0. Original task and most-capable final scoped reviewers PASS/APPROVED, no new
findings. Evidence /workspace/scratch/agent-b/79/windows-endpoint-fix/.
No production SDK/Rust behavior change, no retry/skip; fresh exactheadnative4 pending.

2026-11-13 Task 155-static-draft: Ruling: temporary characterization draft only,
base92c3b61 (main after #157), experiment/windows-transport-retirement-155. Windows-only four-case
owned-thread FlushFileBuffers probe, supervisor-owned CandidateFrozen/shared
records and finite external containment drafted; dispatch-only Windows workflow
must never merge into main. Explicit existing windows-sys IO/Security/Memory
features are probe-only and controller-authorized; no src/lock/product feature
changes. Direct Rust1.100 rustfmt/static verification only; local Cargo slot
belongs to S1a. No production TDD/compile/native pass or task-complete claim.
Fresh static whole-diff review/controller permission required before push or
one exact-ref Windows dispatch. #155 and production owner/E2E obligations open.
Protocol: experiments/transport155-native-probe.md. Draft review/evidence package:
/workspace/scratch/agent-b/155/. Any failed/missing candidate, forced termination,
or unreaped child is failure/inconclusive; no rerun to obtain favorable evidence.

Task 155-static-draft: Ruling: clean rebase onto main92c3b61 before fresh draft
review — CLI147/#157 is inherited intact; sourceguard compares the draft to92,
with all src/** and Cargo.lock unchanged. Approved plan's original1da8 baseline
is retained as provenance; no local Cargo/native claim or remote action added.
