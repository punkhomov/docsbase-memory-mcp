# План исполнения #81 — два функциональных PR

Дата: 2026-11-14. **Goal:** завершить async remote orchestration #81 за максимум
два substantive functional PR. **Architecture:** async orchestration + existing
bounded blocking owners + retained snapshots/flights, [design.md](design.md).
**Spec:** [requirements.md](requirements.md). **Review focus:** physical ownership,
original budgets, stale publication, singleflight and all production consumers.

Baseline main `61effaf34dc570b0a3ea9c5c5afec4748587bf6f`; branch
`feat/async-remote-orchestration-81`, worktree `/workspace/docsbase-agent-b-async81`.
MODIFIED 2026-11-14: fix1 architecture challenge PASS, root unlocked A1.
A1 implementation checkpoint готов к полным gates и fresh task review;
A2/B production до соответствующего unlock не начинается.
Пользовательская автономная scope уже разрешает routine implementation decisions;
новый вопрос пользователю не является implementation dependency.

## Общие ограничения и workflow

- Exact Rust1.100.0, Node24.22.3, pnpm12.16.1; 14-day deny без обхода cooldown.
- New crates: **none**, runtime npm dependencies: **none**. Existing Tokio/
  reqwest/Rig/fd-lock/ouroboros достаточны; actual exception требует ADR до кода.
- Default lean offline; official cli-full содержит оба providers + Web.
- No production unwrap/expect/panic, no paid/live calls/downloads, baseline/golden/
  ignores/budget changes, environment reapply/publish, npm release, #128/#155 work.
- Один code writer и local Cargo owner, `CARGO_BUILD_JOBS=4`, exclusive target
  `/workspace/.tooling/target-agent-b`; literal cold-start setup ниже.
- Один task at a time; save BASE до task, actual RED до production, focused GREEN,
  owner fixes/review receipts. Root вызывает fresh reviewers и координирует PR/CI;
  author исправляет findings. Не создавать отдельные PR для A1 scaffolding/
  settings/docs: PR1 открывается только с real consumer A2 и acceptance A3.
- Receipts **untracked** `/workspace/scratch/agent-b/81/{plan,task-a1,...}`;
  progress ledger — dated append с командами/result/commit range/review findings.
  Не записывать guessed PASS и не делать metadata-only CI rerun commit.
- MODIFIED 2026-11-14, fix1/M2: root уже подготовил frozen Web/SDK/HTTP assets;
  logs `/workspace/scratch/agent-b/81/prep/{node-version,pnpm-version,install,
  web-build,sdk-build,http-build}.log`, все exit0. Повторно prepare не запускать
  без изменения package inputs или фактической утраты assets. Private Docker
  source `/workspace/scratch/agent-b/79/s3/container-check.sh`; execution copy
  `/workspace/scratch/agent-b/81/container-check.sh` меняет только workdir на
  `/workspace/docsbase-agent-b-async81`. Network none, private /tmp и /var/tmp,
  no ancestor fixture .git/host TMP/secrets; env allowlist source recipe неизменен.
- Shared target reused normal rlib может быть stale: inspect depinfo/mtime, при
  доказанной необходимости один mtime-only touch `src/lib.rs` до freeze;
  не content workaround, не full clean. Tests/gates после freeze на том же tree.

## A1 — Async provider, owner policy и bounded DNS execution

**PR:** 1 (вместе с A2/A3). **Depends:** fresh architecture challenge PASS.
**Design:** D1–D3,D6,D8. **FR:** 01,02,03,05,07,08,10,11,13.
**New crates:** none.

**Create:** `src/hybrid/remote_settings.rs`, `src/hybrid/remote.rs`,
`src/daemon/remote.rs`, `tests/provider_settings.rs`, `tests/hybrid_async.rs`.
**Modify:** `src/hybrid/{mod,provider,wire,openrouter,rig_codec}.rs`,
`src/vector_cache.rs` (async capability only), `src/daemon/{mod,settings,
worker_admission,request,lifecycle,server,launch}.rs`,
`src/config/{mod,resolve,view,source,migrate}.rs`, `src/cli/{connect,search,index,
mod}.rs` (explicit runtime composition only; actual direct async phases B3),
`tests/{hybrid_openrouter,config_resolution,config_view,daemon_settings}.rs`,
hybrid-providers requirements/design dated delta, component inventory/dependencies.

**Consumes:** existing SettingsSpec/ConfigSpec/LaunchSnapshot, Cancellation,
WorkerAdmission/request::spawn/Activity, reqwest custom Resolve, Rig codecs.
**Produces:** complete PR1 RemoteSpec subset; AsyncTransport/AsyncEmbedder/
AsyncReranker capability + RemoteCall; shared client/execution; actual bounded
DNS helper with same physical tracker. Background-only fields deferred to B1/B2.

**TDD / expected evidence:**

1. Add minimal gated async transport test and owner default/override consumer
   test before production. Record actual RED: missing async path or observed
   legacy blocking/retry behavior, not a typo/fixture setup failure.
2. Async attempt and retry tests cover every preserved status class, transport
   error, exactly4 default total attempts, base250ms, multiplier2, cap4000ms,
   429 Retry-After clamp30000ms and jitter[0,50). Gate waits without real sleeps.
3. Queue/network/backoff/jitter cancel/expiry gives zero additional attempts,
   capped per-attempt remaining timeout, no final extra sleep. Cancellation is
   terminal, not another retryable transport failure.
4. Owner overrides reach real request timeout, retry calculation, batch splitter,
   active/pending semaphores and response reader. Invalid settings fail before
   provider/client/listener effects; global-only key rejected in project source;
   capability/effective/provenance/restart impact has no artifact rebuild.
5. Controlled resolver closure starts, its HTTP waiter drops, helper slot remains
   occupied and Activity nonzero until release/unwind. Actual bounded helper
   pending queue tested even if caller/connector retains the resolver future.
   No default GAI helper bypass remains on operational async path.
6. Actual-byte response cap rejects growing/misleading Content-Length source
   before excess retention; existing OpenRouter fixtures/finite f32/index/roles/
   L2/rerank/headers/redaction remain green using shared codecs.
7. MODIFIED 2026-11-14, fix1/I1/I3: runtime-free operational transport last Arc
   dropped in async task, construction failure/cancel and old client cleanup
   do not panic/block Runtime destruction. Command wrapper tests all ambient
   flavors, construction **and destruction**, explicit sync/probe bridge and
   current-thread rejection before effects. Actual Shared legacy launch tests
   malformed remote types/unknown keys/zero/overflow timeout/attempt/count and
   cross-owner executor sum, including an unrelated bad legacy field together.
   Client/DB/state/listener sentinels untouched; unrelated legacy fallback stays.
   Provenance/effective/feature-off, v1 roundtrip and connect provider write
   preserve the remote section.

**Verify (focused):**

```sh
cargo test --locked --features hybrid-remote --test hybrid_async
cargo test --locked --features hybrid-remote --test provider_settings
cargo test --locked --features hybrid-remote --test hybrid_openrouter
cargo test --locked --features cli-full --lib daemon::remote
cargo test --locked --features cli-full --lib launch
cargo test --locked --features cli-full --lib connection_tests
cargo test --locked --test config_resolution --test config_view --test daemon_settings
cargo test --locked --test config --test config_migration --test connect
cargo fmt --check
```

Completion: tests actually run/read, no adopted field without consumer, source
scan of operational Reqwest constructor shows injected bounded resolver; fresh
task review PASS/APPROVED. Это internal task, не independently shippable PR.

## A2 — End-to-end async interactive search и independent flight waiters

**PR:** 1. **Depends:** A1. **Design:** D3–D6,D8. **FR:** 01,02,04,05,06,07,10,11,13.
**New crates:** none.

**Create:** `src/daemon/search_async.rs`, `src/hybrid/async_flight_tests.rs`,
`src/daemon/async_search_tests.rs`.
**Modify:** `src/daemon/{mod,server,tools,request,scoped}.rs`,
`src/hybrid/{dense,flight,work}.rs`, `src/search/operation.rs` (explicit deadline
constructor shared with B1 if needed), `src/daemon/{search_phase_tests,
search_payload_tests,server_worker_tests}.rs`, `src/hybrid/query_flight_tests.rs`.

**Consumes:** A1 async adapters/execution, current lexical/dense/scoped/refill
algorithms, ReadPool, PayloadBudget/OwnedPayload, revision/generation admission.
**Produces:** SearchSession-owned async dispatch; start/prepare-with-vector/
finish-with-rerank phases; async membership in existing Flights; retained revision
key; independent waiter and bounded shared-work budget; request core counters.

**TDD / expected evidence:**

1. Before search production edits, drive real `call_tool_in`/native search with
   gated fake remote transport. With search_active1 network-held first query
   must allow a second lexical phase to acquire and complete; legacy whole-call
   worker fails this assertion. Gate event ordering is the proof, watchdog only
   catches hanging test. No task/worker-per-query facade-only assertion.
2. Prove all metadata/config/read leases released while query embedding and
   rerank gates are held. Actual concurrent writer/reload succeeds; generation
   change before capture returns explicit retry. Complete OwnedPayload finishes
   consistently after later mutation/reload, no fresh SQL finalization.
3. Four admitted identical misses produce exactly one provider call and shared
   Arc vector. Different model/revision separate; old-revision success cannot
   publish into new-runtime LRU. Preserve pure completed cache sharing semantics.
4. Cancel/drop initiator and one follower independently; peers continue. Last
   waiter departure cancels async work/removes flight. Failure, panic/unwind,
   abandon/late join and retry leave no stale flight/error cache.
5. Caller earlier/later deadlines, live membership changes and hard shared cap
   tested. Local query flight followers use async waiting; only leader occupies
   CPU while fake local model blocks. Physical local charge persists after drop.
6. Same original deadline across CPU re-admission, embedding+retry+rerank+reply;
   queued/expired follower starts no later phase/provider. Scoped EOF/session
   close and legacy half-close semantics preserved.
7. Existing hybrid RRF/rerank/scoped search, filters/cap refill/snippets/metadata
   outputs match unchanged fixtures; no ranking/baseline regeneration.
8. MODIFIED 2026-11-14, fix1/I2/M1: A starts an attempt near personal deadline,
   B joins with later budget, A expires; release **same** attempt after A but
   before B/hard deadline. With retry_attempts1 B succeeds, provider_attempts1.
   Test reverse membership-shortening, last departure and hard expiry; no resend,
   new jitter or new FIFO acquire on membership wake. Permanent state/Notify
   seams cover selection/join, check/registration, publication/removal and
   retiring/late-join; all live waiters wake/re-admit. Reload old flight as last
   client Arc owner also tests async-safe destruction.

**Verify (focused):**

```sh
cargo test --locked --features cli-full --lib async_search
cargo test --locked --features cli-full --lib async_flight
cargo test --locked --features cli-full --lib query_flight
cargo test --locked --features cli-full --lib search_phase
cargo test --locked --features cli-full --lib scoped
cargo test --locked --features cli-full --test hybrid_ranking --test hybrid_rank_ann --test hybrid_ann
cargo test --locked --features cli-full --test dedupe_refill --test search_payload_budget --test server_workers
```

Completion: actual interactive remote consumer through IPC route, no blocking
network/backoff in its source trace, full requirement map updated, fresh review
PASS/APPROVED. A1+A2 form PR1 functional delta; A3 required before handoff.

## A3 — PR1 acceptance, feature matrix и legacy-boundary audit

**PR:** 1. **Depends:** A2. **Design:** D0–D6,D8,D9. **FR/SC:** PR1 portions of
all FR; SC-81-01..05 and preservation portion07. **New crates:** none.

**Modify/test:** existing `tests/{ipc_server,mcp_frontend,mcp_registry,
server_workers,server_profile,server_readiness,literal_clients,search_golden,
search_bench,offline,node_client,website_http}.rs`, focused new tests as needed;
`docs/{CONFIG,ARCHITECTURE}.md`, this spec progress/coverage and component inventory/
state-dependencies. Do not add mirror tests or unrelated code cleanups.

**Acceptance:** deterministic actual request queue/HTTP/retry/cancel/reload/
helper/local matrix completed; official cli-full functional/fmt/Clippy/native
required checks green. Lean dependency tree remains remote-free. PR description
states direct/index/watch/background remain legacy until PR2; no full Shared
reserve/process-wide capacity or #81 completion claim. Fresh whole-diff review
maps every delivered FR/SC to code/test, findings author-fixed and scoped-reviewed.

**Verify:** run the common delivery gates below once frozen. Narrow failure
fixes rerun affected focused tests then required project gates; no blind repeat
or serial docs-only PR. Root creates/reviews/pushes/merges the one functional PR.

## B1 — Resumable index, async vector batches и attributable Sync/backfill

**PR:** 2. **Depends:** PR1 accepted; fresh challenge of any implementation
delta discovered while A shipped. **Design:** D2,D3,D6,D7,D8.
**FR:** 02,04,05,07,08,09,10,11,13. **New crates:** none.

**Create:** `src/index/async_job.rs`, `src/hybrid/async_jobs.rs`,
`src/daemon/job_service.rs`, `tests/async_index.rs`, `tests/async_backfill.rs`.
**Modify:** `src/index/{mod,job,outcome,chunk,frontmatter}.rs`, `src/store/repo.rs`,
`src/daemon/{mod,lifecycle,tools,
server,worker_admission,settings}.rs`, `src/hybrid/{remote_settings,jobs}.rs`,
`src/vector_cache.rs`, `src/search/{operation,budget}.rs`,
`tests/{index_job,index_recovery,index_outcome,index_vector_source,
vector_sync_context,hybrid_jobs}.rs`; source unit module
`src/vector_cache_batch_tests.rs`, verified with `--lib vector_cache_batch`.

**Consumes:** A1 providers/settings; existing writer lease/IndexOutcome/Sync
capture/VectorCacheSource/pin semantics. **Produces:** owned same-lock writer
lease, PreparedIndexJob state machine; bounded lookup/network/write batches;
whole job operation and background limits with real consumers; terminal cleanup.

**TDD / expected evidence:**

1. Gated actual full index/Sync embedding shows no background CPU permit or
   metadata transaction through network. Own Db/IndexHandle and exclusive writer
   lease remain bounded; same-project read refusal and unrelated status/lexical
   queries preserved. Owned lease Send/cross-process read/write behavior tested.
2. Full/incremental/recreated-index recovery share planner/apply; remote failure
   after markers keeps retry evidence. Next explicit index re-plans instead of
   skipping. No-op/partial/full surveys and exact counters unchanged.
3. Count/bytes bounded backfill pages/batches and retained plans; oversized
   allocation rejected before markers/mutation. Cache connection dropped before
   provider gate. Whole invalid batch produces no partial writes, earlier valid
   batches reused and duplicate hashes/input order preserved.
4. Pin lease renewed using actual DenseSpec lifetime, released on failure/cancel,
   no stale config/generation ANN publication. Physical vector owner retained.
5. Sync association is captured from its own lexical attempt before writer lease
   release; later direct writer/no-op/partial work cannot relabel that Sync.
   Failure/backfill/cancel cannot promote readiness to Ready.
6. Whole background timeout includes pending/prepare/network/write, cancellation
   before admission starts no mutation; terminal marker/error publication runs
   despite closed new admission and leaves non-success recovery state.
7. MODIFIED 2026-11-14, fix1/C1: JobService issues separate logical L slot and
   pre-reserved terminal ticket before capture; physical J is phase-owned.
   At J=L=T=1 cancel/abort while remote gated, while prepare/resume runs, during
   result-delivery failure, and force unwind. Close all ordinary admission
   before handoff. Fake/native destructor and terminal SQL gates show unrelated
   Tokio control progresses, native writer refused, lease/bytes/Activity charged.
   Release proves exactly one T finalizer publishes captured Sync/non-success
   outcome then retires index→Db→payload→lease→logical→CPU permit→Activity.
8. MODIFIED 2026-11-14, fix1/I4: instrument PayloadPermit grow/capture with many
   individually legal files and low configured cap. Failed next reservation has
   **no following allocate/copy event**; peak logical charge/live capture <=cap,
   failed grow leaves aggregate unchanged, status/attempt/markers/index unchanged.
   Include paths/maps/hashes/chunks/headings, actual current-file growth, JSON
   escaping and overlapping Rig/input clones, cancellation/unwind release, and
   direct/daemon/watcher shared ownership. No final-error-only substitute.

**Verify (focused):**

```sh
cargo test --locked --features cli-full --test async_index --test async_backfill
cargo test --locked --features cli-full --test index_job --test index_recovery --test index_outcome
cargo test --locked --features cli-full --test index_vector_source --test vector_sync_context --test hybrid_jobs
cargo test --locked --features cli-full --lib vector_cache_batch
cargo test --locked --features cli-full --lib job_service
cargo test --locked --features cli-full --lib search::budget
cargo test --locked --features cli-full --lib sync_readiness
cargo test --locked --features cli-full --test server_readiness --test website_http
```

Completion: full/Sync/backfill actual consumers, legacy sync wrappers not used
by these production routes, every adopted background setting real; fresh review.

## B2 — Watcher async consumer, bounded background scheduling и physical drain

**PR:** 2. **Depends:** B1. **Design:** D5–D7,D8. **FR:** 05,07,09,10,11,13.
**New crates:** none.

**Modify:** `src/watch/mod.rs`, `src/watch/runtime_tests.rs`,
`src/daemon/{server,lifecycle,request}.rs`, `src/hybrid/dense.rs`,
`tests/{watcher,watch_retry,lifecycle,server_workers}.rs`; add focused
`src/daemon/async_shutdown_tests.rs`, `src/watch/async_runtime_tests.rs`.

**Consumes:** B1 resumable job; existing WatchRuntime config/stages/source,
event channels/coalescing/retries, Activity counter and dense build reservation.
**Produces:** production async watcher consumer, shared job scheduler, complete
activity retention through DNS/local/flight/job/ANN-refresh closure finish.

**TDD / expected evidence:**

1. Actual watcher remote gate does not park a consumer blocking worker/thread;
   producer retains bounded event backpressure and coalescing. Success, repeated
   changes, ignore/config reload, lease contention and full recovery unchanged.
2. Multiple projects/watch/explicit Sync contend for finite job admission and
   background provider lane; interactive remote and control reserve independent.
   Actual physical `S+C+J+H+T<=B`, T1, checked before writes; no production None-permit
   bypass. Event producer threads counted separately, no process-wide claim.
3. Drop/cancel queued, HTTP/backoff, local inference, DNS and flight leader, then
   graceful stop. Pending retires, started physical work stays charged through
   finish/unwind and terminal state. Existing second-signal semantics preserved.
4. Post-sync refresh and detached ANN build retain same Activity until actual
   work completes; existing byte reservation/pin lifetime remains honest after
   invalidation/reload/caller drop. No network wait in these blocking tasks.
5. MODIFIED 2026-11-14, fix1/C1: close-new does not close pre-admitted terminal
   lane/receiver; existing tickets survive shutdown, pending/new jobs get no new
   ones. Queue+reserved+active <=L at J1 and multiple jobs; dispatcher cannot drop
   before all JobOwners/tickets and native teardown finish. Terminal captured
   Sync attribution/#159 readiness is observed before Activity drain completes.

**Verify (focused):**

```sh
cargo test --locked --features cli-full --lib async_shutdown
cargo test --locked --features cli-full --lib watch
cargo test --locked --features cli-full --test watcher --test watch_retry --test lifecycle --test server_workers
cargo test --locked --features cli-full --test ann_lifetimes --test vector_store_owner
```

Completion: production background source inventory has no sync-over-async
provider path; physical pending/active/drain tests PASS; fresh task review.

## B3 — Direct consumers, metrics и полное закрытие #81

**PR:** 2. **Depends:** B2. **Design:** D0–D9. **FR/SC:** all accepted requirements
and SC-81-01..07, no partial issue closure. **New crates:** none.

**Modify:** `src/cli/{search,index,mod}.rs`, `src/hybrid/meta.rs`,
`src/search/operation.rs`, `src/daemon/{tools,server}.rs`,
`tests/{cli_read,cli_index,cli_routing,mcp_frontend,mcp_registry,error_surface,
hybrid_openrouter,provider_settings}.rs`, `tests/hybrid_async.rs`,
`docs/{CONFIG,ARCHITECTURE,IPC-CLIENT}.md`, component inventory/state-dependencies,
this spec requirement/SC/source-consumer coverage and progress ledger.

**TDD / expected evidence:**

1. Direct search/index/sync uses one explicit runtime composition for a command,
   no per-request/attempt runtime and no current-thread nesting panic. Existing
   daemon initial-connect-only direct fallback/no replay and Website denial stay.
   MODIFIED 2026-11-14, fix1/I1/C1: runtime/execution roots remain outside work
   future; work Err/unwind calls close/drain/join before Runtime Drop. Gate
   terminal/native teardown during direct command failure and daemon serve
   unwind; async last-Arc drop never becomes Runtime/IndexState destructor.
2. Controlled queue/CPU/HTTP/backoff/multistage run produces expected additive
   metrics and attempts. Shared calls counted once globally; cancelled waiter
   attribution does not inflate provider calls; no raw key/query/doc/body labels.
   Retained/saturated counters unwind to correct physical values, status uses
   independent admitted control work while network pending.
3. Final source scan maps every operational remote consumer to async path;
   compatibility/probe exceptions named and not called by daemon/watch/direct
   indexing flows. Existing provider fixture/feature-off/admission/local adapter
   contracts retained. No new ignored tests or retrieval fixture changes.
4. Full frozen project checks/native CI and fresh whole-diff convergence review
   pass. Author fixes Critical/Important, original reviewer scoped re-review.
   Each FR/SC maps exact source/test receipts; no deferred accepted requirement.

**Verify:** focused CLI/metrics tests, then common delivery gates below. Root
handles final PR/push/ordinary squash and #81 closure under prior authorization;
no npm release/environment publication and no new approval question.

## Общие delivery gates для PR1 и PR2

MODIFIED 2026-11-14, fix1/M2: literal cold-start composition (no inference from
chat/delegated context). Run each Cargo command through
`/workspace/scratch/agent-b/81/container-check.sh` once that exact isolated copy
has been prepared from the source recipe above; command output/exit saved in
`/workspace/scratch/agent-b/81/task-a1/` etc. Environment inside that recipe:

```sh
cd /workspace/docsbase-agent-b-async81
source /workspace/.tooling/docsbase-env.sh
source /workspace/.tooling/docsbase-node-env.sh
export CARGO_TARGET_DIR=/workspace/.tooling/target-agent-b
export CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 CARGO_INCREMENTAL=0
export TMPDIR=/var/tmp/db-b XDG_DATA_HOME=/workspace/.tooling/test-xdg-data
export DOCSBASE_BENCH_LATENCY_SLACK=3
mkdir -p /var/tmp/db-b
```

Version/setup evidence retained raw. Reuse already successful asset preparation
logs in `/workspace/scratch/agent-b/81/prep/`; **do not repeat install/builds**
unless inputs changed/assets missing. If regeneration is necessary, commands are
`pnpm install --frozen-lockfile`, `pnpm --dir web build`,
`pnpm --dir packages/client build`, `pnpm --dir examples/website-search build`,
each with raw log/exit receipt before Cargo. Frozen verification commands:

```sh
node --version
pnpm --version
rustc --version
cargo --version
cargo test --locked --features cli-full
cargo clippy --locked --features cli-full --all-targets -- -D warnings
cargo clippy --locked --features cli-full --test perf_budget -- -D warnings
cargo clippy --locked --features cli-full --test artifact -- -D warnings
cargo fmt --check
cargo test --locked --test offline
cargo test --locked --test search_golden --test search_bench
cargo check --locked --all-targets
cargo check --locked --features hybrid-remote --all-targets
cargo check --locked --features hybrid-local --all-targets
cargo tree --locked -e normal
git diff --check
```

Do not run INSTA_UPDATE or live ignored tests. Current private isolated
container recipe avoids ancestor git fixture pollution; no host workaround
alters retrieval results. Official native required `docs/linux/macos/windows`
CI on final source is distinct from local Linux green; release-static/artifact
and source compatibility gates follow actual project CI/AGENTS requirements.
No broad baseline run for architecture-only local documents.

## Traceability, review package и release criterion

| Requirements | Design | Task/source verification |
|---|---|---|
| 01,03 | D1,D3 | A1 transport/codec + A2 interactive consumer + A3 whole route |
| 02 | D3,D3a,D4,D5,D7 | A1 retry, A2 same-attempt dynamic-interest test, B1 job budget, B3 direct |
| 04,06 | D4,D5 | A2 WAL/config/generation/flight matrix; A3 whole diff |
| 05,07,11 | D2,D3a,D6,D7a,D7b | A1 helper/runtime; A2 CPU/leader; B1/B2 terminal/destructor/budget drain |
| 08 | D2,D7b | A1 actual Shared owner-fatal launch + roundtrip; B1/B2 PR2 fields/budget; B3 audit |
| 09 | D7 | B1 full/Sync/backfill; B2 watcher; B3 direct |
| 10 | D3,D4,D7 | A2 IPC/MCP; A3 native; B3 runtime/fallback matrix |
| 12 | D8 | A2 core counters, B2 physical counts, B3 projection + redaction |
| 13,NFR | D0,D1,D8,D9 | Every task preservation + A3/B3 official gates/review |

Author task package includes task brief paths, BASE..HEAD diff/log/stat,
RED/GREEN/gates raw receipts, requirement/SC mapping and stated legacy gaps.
Reviewer does not rerun established tests; cannot-verify items resolved by
coordinator/source receipts, never silently marked PASS. Final issue convergence
requires both functional PRs, exact implementation/source inventory and no
unfulfilled accepted FR/SC; docs alone do not satisfy anything runtime-related.
