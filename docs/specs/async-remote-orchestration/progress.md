# Progress — async-remote-orchestration #81

Spec: [requirements.md](requirements.md), architecture: [design.md](design.md),
execution: [tasks.md](tasks.md). Architectural path, machine review coordinated
by root, author owns implementation/fix passes. Дата начала: 2026-11-14.

## 2026-11-14 — local architecture preparation, implementation locked

- Baseline verified `61effaf34dc570b0a3ea9c5c5afec4748587bf6f` on branch
  `feat/async-remote-orchestration-81`, worktree
  `/workspace/docsbase-agent-b-async81`. Initial `git status --porcelain=v1`
  empty. Actual issue81 body fetched with gh; source baseline is post #158/#159,
  not historical issue commit141509f or Rust1.88.
- Read AGENTS, project-design SKILL/integrity/execution/reviewer/documents/
  elicitation, constitution current toolchain delta, relevant hybrid-providers,
  component-configuration inventory/dependencies/CF13, rig-providers and
  website-search/search-concurrency contracts. Applied user-authorized autonomous
  scope: no repeated user permission gates; root fresh architectural challenge
  precedes production, as delegated for this task.
- Source verified: whole search CPU permit at `call_tool_in`; existing phased
  reads/OwnedPayload, SearchOperation/Cancellation/Activity/WorkerAdmission and
  Flights reused. Reqwest owns async client + fallback runtime; retry sleep sync;
  actual Rig codec already integrated and retained. Index embedding currently
  sits between markers and commits, Sync attribution captures own lexical result
  before backfill; watcher consumer has dedicated sync thread/retry loop.
- Hidden helper evidence: pinned reqwest0.12.28 GAI delegates to hyper-util0.1.21
  `spawn_blocking(getaddrinfo)`, GaiFuture Drop abort cannot retire running DNS.
  Plan includes actual bounded resolver with closure-held helper permit/Activity,
  finite computed pending cap even for surviving dependency connector work.
- Produced local requirements/design/tasks/progress bundle only. No production
  source, Cargo/Node dependency, lockfile, baseline or remote application changes.
  No Cargo/full baseline/live provider/model download invoked for this phase.

Raw untracked receipts: `/workspace/scratch/agent-b/81/plan/issue81.json`,
`base.txt`, `baseline-status.txt`, `source-scan.txt`, `http-helper-scan.txt`.
Final local document consistency checks/source links recorded below when run.

2026-11-14: local document validation ran: four documents,13 distinct FR and
7 SC, six ordered implementation tasks, all relative Markdown links/fences
valid, no unresolved placeholders. `git diff --check` has no findings; untracked
files additionally checked with `git diff --no-index --check /dev/null FILE`
(exit1 denotes added-file diff, empty output means no whitespace findings).
No tracked production diff. Receipt: `plan/document-validation.json`.
Source-name audit corrected proposed extension to actual Provider method family
`build_stages_with_execution` and documentation target to existing ARCHITECTURE.md.

Self-challenge corrections: helper pending capacity is now a real finite
semaphore cap even if dependency connector outlives HTTP; owned FlightInterest
explicitly narrows immutable hard-budget operation; local flight followers also
wait asynchronously. Independent approval remains pending, no runtime PASS claim.

Ruling: deliver maximum two integrated functional PRs — first real interactive
async search, then all remaining background/direct/lifecycle/metrics consumers.
No docs-only/flags-only/scaffolding PR. #81 remains open after first PR.

Ruling: global operational RemoteSpec settings only, existing v1 hybrid semantics
preserved — narrow CF13 I38–I40/I66 adoption is necessary before new consumer;
full provisioning/v2 config executor/publication work stays out of scope.

Ruling: no parallel query flight/token/executor/reader-pool replacement — extend
existing Flights/Cancellation/SearchOperation/WorkerAdmission/Activity/ReadPool
owners. Completed query pure recipe identity unchanged, active work additionally
fenced by retained runtime revision; stale result cannot publish new config.

Ruling: current in-place indexing may refuse same-project searches while an owned
writer lease spans remote work — preserves existing mutation/recovery contract;
no promise of serving old generation or shadow-generation implementation.

## MODIFIED 2026-11-14 — fresh challenge и author blueprint fix1

Fresh independent review `/workspace/scratch/agent-b/81/plan/review.md` вернул
**CONDITIONAL**, C1 Critical и I1–I4 Important; M1/M2 Minor. Оригинальная bundle
с manifest сохранена root в `plan/before-fix1/`. Production по-прежнему locked;
не было source/Cargo/lockfile/PR changes. Root уже подготовил frozen Node assets,
`81/prep/{node-version,pnpm-version,install,web-build,sdk-build,http-build}.log`
exit0; author их не повторял.

Ruling C1: unique transfer-only JobOwner и existing Activity, distinct logical
L=J vs physical J, pre-reserved bounded terminal tickets и fixed T1 lane внутри
WorkerAdmission. Native IndexState может destroyed только в charged blocking
retirement после terminal SQL/captured Sync attribution; index/Db/data/permit
перед writer lease, physical CPU permit перед final Activity completion. Root
execution owner вне work future сохраняет terminal receiver до drain, в том
числе при result-delivery failure/unwind/ordinary admission shutdown. Untouched
budget refusal не меняет project status/attempt/outcome. D6/D7a, B1/B2/B3.

Ruling I1: operational ReqwestTransport runtime-free; explicit command/daemon
runtime root construction и destruction вне work block_on, with drain и ambient
flavor rejection before effects. Sync compatibility adapter owns Handle/liveness,
не Runtime. Actual last-Arc/stale/cancel/error/nesting/destruction tests A1/A2/B3.

Ruling I2: shared fixed HTTP timer — min(attempt setting, hard-cap remaining),
dynamic live-interest — outer resettable wait retaining same pinned HTTP/acquire/
backoff future. Early A/later B/attempts1 causal test, reverse shortening/last
departure/hard expiry; no resend/jitter reset/FIFO restart. D3/D5, A2.

Ruling I3: actual captured-document launch resolves recognized RemoteSpec before
legacy decode, maps failures to LaunchFailure::Remote and fails legacy Shared
before effects; unrelated old fallback remains. Global provenance/v1/source/
connect write roundtrips and project exclusion tested at real boundary. D2,A1.

Ruling I4: existing PayloadPermit grow_to/shrink ownership and CaptureBudget
reserve before all retained copies; scalar SQL preflight, actual bounded current
file, chunk/parser/path/hash/map costs, peak input/Rig/encoded clones reserved
before project effects. Instrumented next-allocation refusal/peak/cancel/unwind
tests. D7b,B1. Logical charge не является allocator/native RSS guarantee.

M1: permanent state/register-before-check and selection/join/publication/retiring
causal gates specified D5/A2. M2: literal env/container/receipt paths и existing
prepared assets, source unit vs integration targets corrected tasks.md.

Fix1 report/diff/manifest: `/workspace/scratch/agent-b/81/plan/fix1/`.
Author fixes are **proposed addressed**, authoritative scoped re-review pending;
no production unlock, runtime acceptance or task-complete claim.

## Gate status

| Gate | Status | Evidence / remaining work |
|---|---|---|
| Source discovery / initial local blueprint | Approved after fix1 | Local four-document bundle included with functional A1 checkpoint |
| Fresh root architecture challenge | PASS; root unlocked A1 only | plan/fix1/rereview.md: C1/I1–I4/M1/M2 addressed |
| A1 / PR1 | Implementation checkpoint; full gates/review pending | Actual causal RED and focused GREEN below; no task-complete claim |
| A2..A3 / PR1 | Not started | Real search consumer, fresh reviews and native gates required |
| B1..B3 / PR2 | Not started | Completes remaining accepted requirements and issue closure |
| Runtime convergence / performance | Not claimed | No tests/builds/live measurements run in architecture phase |

Deferred minors: none recorded. Open user blockers: none found.

## MODIFIED 2026-11-14 — A1 functional implementation checkpoint

Authoritative fix1 re-review returned PASS for C1/I1–I4/M1/M2; root approved
the revised bundle and unlocked A1 only. Earlier locked/pending statements above
are historical architecture-phase receipts. A2/A3 and B1/B2/B3 remain pending.

Task A1: implementation checkpoint (base `61effaf3`; commit containing this
entry). Global-only RemoteSpec is composed before legacy launch fallback; actual
Shared rejects recognized invalid policy before cache/state effects. Async
provider execution shares Rig codecs and an explicit policy owner; bounded DNS
and local CPU closures retain existing physical Activity/admission until exit.
Operational ReqwestTransport owns no Runtime. Explicit command runtime scope
drains physical work and retires Handle-only compatibility bridges before drop.

Actual RED receipts in `/workspace/scratch/agent-b/81/task-a1/`: existing Runtime
drop panicked inside Tokio **1.54.1** (`red-runtime-drop.log`); missing owner/async
capability failed (`red-owner-policy.log`, `red-async-path.log`); actual Shared
legacy fallback accepted invalid recognized policy (`red-shared-launch.log`);
dropped queued CPU work still executed (`red-queued-cpu-drop.log`). Each causal
failure was observed before its production fix, then the focused contract passed.

Focused GREEN: `focused-cli-full.log`, 180 passed/0 failed across 12 integration
targets (including async13, OpenRouter26 and provider policy3);
`focused-remote-unit.log`, 9 passed/0 failed (actual command runtime, DNS physical
ownership, retry timing, Shared launch and connect-write boundaries). Current
cli-full and default all-target Clippy with `-D warnings`, and `cargo fmt --check`
pass (`clippy-cli-full-2.log`, `clippy-default.log`, `fmt-check.log`). These are
focused checks, not full project acceptance. Full gates and fresh A1 review follow
the functional checkpoint; no public A1-only PR or issue-closure claim.

Ruling: async provider inputs use owned Arc payloads so bounded CPU preparation
does not borrow across detached physical work. Existing factory signatures use a
construction-scoped compatibility Handle context, never a thread-local guard
across await. Background-only RemoteSpec fields await their B1/B2 consumers.
Shared hard attempt budgets and dynamic interest are implemented separately;
actual Flights integration and end-to-end async search remain A2.

Ruling: current locked/compiled Tokio is 1.54.1; prior challenge's cached 1.53.1
reference does not describe the causal RED runtime. No dependency files changed.
Prepared frozen Web/SDK/HTTP assets were reused. Fresh normal rlib depinfo contains
the new remote module and is newer than source (`depinfo-source-check.json`);
no mtime touch, source workaround or clean was required.

## MODIFIED 2026-11-14 — A1 review fix1 (I1/I2/I3/M1)

Fresh A1 task review returned FAIL/CHANGES at `af08ca0`; author owns this scoped
fix. Actual gated REDs (`task-a1/fix1/red-queued-cpu-shutdown.log` and
`red-resolver-shutdown.log`, each exit101) reproduced queued CPU work running
after execution shutdown and retained real-factory DNS admission remaining live.

I1: error exits keep CancelOnDrop armed; actual CPU closure checks the execution
shutdown token before work. I2: real reqwest factory gives its resolver the same
execution token; helper admission observes both execution and root shutdown,
and a queued helper closure checks retirement before native lookup. Retiring one
execution leaves shared search/control lanes available. No global worker kill.

I3: causal gates cover original deadline expiry during provider queue/network/
backoff+jitter; actual transport observes the min of policy and partially consumed
ordinary budget. The tests await the semantic deadline (one second), with held
work and a five-second hang watchdog, without elapsed-time performance claims.
Actual Shared table adds unknown owner key and a valid-TOML numeric value beyond
the u64 timeout domain; the latter fails owner decoding, preserving the existing
whole-document syntax/fallback boundary. Direct assessment and real Shared
restart notice prove runtime-only remote policy change, with no artifact effect.
M1 runtime-ownership comments now describe the retirable Handle bridge.

Focused GREEN: remote unit14/0 plus CLI integration44/0; affected lean config51/0,
owner unit1/0, final settings4/0. Current cli-full/default all-target Clippy and
fmt pass. One earlier Clippy exit101 for new test assertion style is retained in
raw receipts and fixed. Full frozen CLI check follows this checkpoint; fix1
report/commands/source hashes live untracked under `task-a1/fix1/`. Fresh scoped
re-review remains pending; A2 still locked. No task-complete/PR/closure claim.

Ruling: use a construction-only resolver factory seam to retain the exact
resolver injected into real reqwest for the shutdown test. The normal factory
and test share the complete owner/token/client composition. Existing local
CPU/Activity/WorkerAdmission owners and same-pinned-future policy are retained.

## MODIFIED 2026-11-14 — A1 accepted; A2 functional checkpoint

Task A1: complete (`61effaf3..6a80d7d`; original fix1 scoped re-review
PASS/APPROVED, I1/I2/I3/M1 addressed, no new Critical/Important findings).
Root unlocked A2. A1 final CLI1586/0/18 and source-hash receipts were accepted.

Task A2: functional implementation checkpoint (base `6a80d7d`, commit containing
this entry; fresh task review pending). Actual native search now runs admitted
lexical preparation, async shared embedding, admitted dense/fusion/refill/payload
capture, async remote rerank and admitted finalization. Only local inference,
codec and metadata/CPU phases hold search permits. The interactive factory uses
Shared RemoteExecution; execution-only real Rig stages pass the native route
without a synchronous bridge. Direct/index/backfill/watch remain legacy PR2 work.

Existing Flights owns async memberships and one independent leader; keys retain
both config and stage Arcs, including abandonment/unwind. Personal deadlines and
last-departure cancellation do not alias the initiator's token. Later peers keep
the same fixed HTTP attempt; hard cap, reverse shortening, late join and Notify
publication boundaries pass controlled tests. Config/stage/generation admission
ends before await; completed OwnedPayload finishes after later writer/reload.
The leader and its actual CPU closure retain the original Activity/connection
permit; DNS retains its existing separate physical helper owner.

Actual causal RED receipts in untracked `task-a2/`: held HTTP starved lexical
dispatch (`red-dispatch-worker.log`); rerank selected sync entry
(`red-rerank-dispatch.log`); abandoned CPU output outlived its payload charge
(`red-cpu-output-retention-2.log`); empty document scope made an extra provider
call (`red-empty-scope.log`). Each was observed and fixed. Reservations now follow
actual returned CPU data and encoded rerank input through failed delivery/drop;
retained/encoded copies are admitted before allocation under existing limits.

Ruling: approved D5 intentionally removes obsolete in-flight LRU publication.
The existing reload test initially failed8/1 expecting that warming; its name
and exact retry error remain, with a pre-existing completed vector proven to
survive by Arc identity and the obsolete query explicitly absent. No blanket
cache clear, test removal, fixture/baseline/ignore or budget-value change.

Ruling: existing scoped fixtures stopped polling the phased request after one
poll, then blocked on provider entry. The exact stalled test was stopped with
SIGTERM (receipt retained); fixtures now select provider entry while driving
that same future. All EOF/half-close/partial-frame/physical permit assertions
and timeouts are unchanged. Scoped16/0 verifies the corrected driver/ownership.

Focused GREEN includes async/refactor36/0, scoped16/0, phase9/0, query-flight10/0,
worker-unit4/0 and129/0 preservation tests across eight integration targets.
Current cli-full/default all-target Clippy (`-D warnings`) and fmt pass; historical
lint/compiler failures remain in raw receipts. One frozen full CLI and full lean
run follow this checkpoint. No runtime convergence/native/release acceptance or
task-complete claim for A2; A3 and PR2 are still pending.
