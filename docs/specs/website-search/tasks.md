# #79 client implementation plan

**Goal:** typed bounded Node IPC connector, without closing whole #79.
**Architecture:** native sequential IPC connection; isolated bounded search/control
pools; caller-owned operation across admission/handshake/delivery.
**Spec:** docs/specs/website-search/requirements.md
**Global constraints:** Rust1.100.0, Node24.22.3/pnpm12.16.1/TS5.9.3; zero new Rust
crates/runtime npm dependencies/live inference. Preserve native checks/baselines.
A owns storage/index/migration; B IPC/lifecycle/providers; no storage edits here.
**Review focus:** interrupted slot reuse, deadline reset, queue/byte growth,
unsafe u64 coercion, hidden indexing/provider calls; tests owned below.

## C1 — typed native transport

Create packages/client/{package.json,tsconfig.json,src/{types,settings,protocol,
connection}.ts,test/transport.test.ts}. Modify pnpm workspace/lock.
Consumes protocol.rs Hello/BindReadOnly/Search shapes, required caller-supplied
expectedBuildId/expectedSchemaVersion/projectID. Build string checks Rust package
version equality; source commit identity не обещается. expectedSchemaVersion и
Hello.schema_version должны быть positive safe integers и точно совпадать до bind.
Produces checked Connection.exchange(request, signal) and runtime DTO guards.
RED tests first: fragmented/multibyte frames, invalid/incomplete/oversized/extra
responses; chunk_id 0/MAX(u64) → canonical decimal strings, MAX+1/negative/-0/
fraction/exponent → protocol rejection по raw-token syntax/range; wrong
hello/capability/build; omitted/invalid config schema
без I/O, missing/invalid/mismatched Hello schema при совпадающем build закрывает
socket до bind/search/status, matching schema допускает bind; no mutating bind. Confirm
failure before implementation, implement minimal checked transport; compile/test.
Acceptance FR-C1..C4/C8 partial; NFR-C1..C4; no orphan source scaffolding.
Verify package check/build and transport tests; fresh task review; task commit.

## C2 — bounded public client

Create src/{pool,index}.ts, test/client.test.ts, README package usage.
Consumes C1 transport, complete ClientSettings; produces DocsbaseClient public
search/status/close typed API, separate bounded pools, original operation.
RED gated peer tests: overload, FIFO cancelled waiters, timeout in Hello/queue,
search cancellation, control independence, no replay, later reconnect, close.
Deterministic clock tests захватывают ingress до validation/serialization для
search/status: preprocessing уменьшает remaining и увеличивает elapsed исходной
operation; исчерпанный им budget запрещает pool admission/connect/write. Сохранять
один deadline через preprocessing/queue/Hello; queue_ms — только ожидание slot.
Implement; verify compile/build + all Node tests, no real provider. Acceptance
FR-C4..C7, SC-C2. Fresh task review; commit.

## C3 — native integration and operational contract

Create Rust test tests/node_client.rs and protocol fixture under
packages/client/test/fixtures; Node native test entrypoint. Modify existing
ci.yml native setup steps, docs/IPC-CLIENT.md, docs/ci.md and changelog.
RED actualdaemon search/fixture test; produce fixture from Rust serde actual
Request/Response/SearchHit/error codes (not copied static assertions). Native Node
script searches real index, snippets/filter/dedupe, unknownproject, closes/restarts
and next operation reconnects. Verify client doesn't autostart/index/provider.
Protocol fixture must be generated and checked via Rust test, explicit env update
like web_contract. Node tests consume same fixture for codec validation.
Native CI package check/build/tests in existing native jobs, functional Rust suite
runs E2E using existing built binary; generated JS/type package pack smoke.
Acceptance remaining FR-C8/C9, SC-C1/C3; docs define lifecycle/namespace/generation
and server follow-up FR-S*. Full relevant suites/fmt/Clippy + native4 CI, fresh task
and final reviews; merge client PR, leave #79 open and reassess server queue.

## Follow-up phases

S1 CF12 owners watcher/lifecycle/fixed-project settings + server mode.
S2 explicit sync/readiness/errors + Node HTTP example/end-to-end updates.
S3 real targetcorpus ~20k acceptance (selected corpus/load/budgets required).
Then #81 remote async and CF13, separately reviewed without billing.

## Delta 2026-11-13 — MODIFIED: review fixes before C1/C2

C1 расширен явной config/Hello schema проверкой независимо от build string;
C2 расширен deterministic preprocessing deadline tests до admission. Реализация
ещё не начата; эти RED cases обязательны при выполнении соответствующих задач.
C1 также фиксирует chunk_id unsigned integer raw syntax, u64 range и tests на
0/MAX/MAX+1/negative/fraction/exponent без числового округления.

### Delta 2026-11-13 — C3 concrete files / user ruling

C3 also modifies AGENTS/README links and package-local Biome configuration, reuses
pinned existing Biome dev dependency and LICENSE, creates check-node-package.py
and fixture.test.ts/native.mjs. Check fixture under lean and cli-full (canonical
JSON order, exact numeric tokens), native real-daemon run, packed external TS/JS
consumer. FR-S4 real corpus is user-deferred, not a blocking acceptance criterion;
FR-S1..S3 remain follow-ups before #79 closes.

### Delta 2026-11-13 — C3 prerequisite review fix

C3 also audits all workflow and agent/local test entrypoints for the generated
client export. Build `packages/client` before nightly's full release `cli-full`
suite and include it in the documented Web/full-test preparation. Required native
jobs already build it; preserve CLI-only builds, compile checks and selected test
targets that never execute the Node E2E. Verify an isolated source copy fails its
public package self-reference without `dist`, then succeeds after the actual
package build; validate workflow syntax without repeating unrelated Cargo suites.

## Delta 2026-11-13 — S tasks

The coarse follow-up list is refined by server-plan.md: S1a owner settings and
launch snapshot; S1b fixed lexical profile and watcher/lifecycle gates; S1c bounded
physical worker admission; S2 readiness/explicit sync; S3 Node HTTP example and
original deadline integration; S4 native acceptance/docs. Review each task
independently. Real-corpus FR-S4 remains deferred, not renamed into synthetic
acceptance. No S implementation has begun at this baseline.

## Delta 2026-11-13 — MODIFIED: S challenge completion gates

Use the amended [server-plan.md](server-plan.md) task sections for exact files,
interfaces, RED cases and copyable verification commands. These commands are
future implementation gates; no Cargo/build/test execution is claimed here.

1. **Prerequisites:** reviewed #143 then narrow
   [#147](https://github.com/punkhomov/docsbase-memory-mcp/issues/147) explicit
   sync path fix before S1 integration. #147 does not implement Website policy.
2. **S1a / FR-S1b:** DaemonSpec owns explicit max_blocking_threads512, Tokio
   MAX_PERMITS/channel bounds and checked lexical executor sums. Pin tagged
   profile whole-value CLI overlay, canonical roots, omitted/explicit provenance
   and Shared grace0. Create daemon_settings target; use config/config_resolution/
   config_runtime/config_view plus lean checks. Source-scan and update component
   settings inventory/dependency matrix in this same PR, including adopted vs
   deferred I44..I47 subsets, worker limits, helper invariants and restart effects.
3. **S1b / FR-S1a,d/S2b:** server_profile/cli_routing/lifecycle/watcher/error_codes
   targets cover typed denial without direct fallback, fixed-path bind from
   unrelated cwd, no-daemon ordinary direct vs Website cases, interrupted daemon
   with no replay, backend watcher/provider gates and Shared compatibility.
4. **S1c / FR-S1c,d:** create daemon/worker_admission.rs and server_workers target;
   preserve existing daemon/admission.rs::Lease and admission/admission_ux targets.
   Extend hybrid/work.rs with same-state Notify async cancellation and keep
   blocking subscribers; test lost-wake races and drop cleanup with no bridge or
   polling. Wire all inventory paths, actual sync/Activity completion and explicit
   executor capacity. One coalesced janitor state polls control admission/finish
   beside accept/stop/signals. Gate workers to prove physical control entry while
   search/sync are full and loop progress under busy janitor/control. ipc_server/
   lifecycle plus full Clippy/fmt complete the named gates. Shared hybrid legacy
   background/provider behavior is outside the lexical physical reserve proof.
5. **S1 integration:** each subtask has a separate fresh review; all owner fields
   are wired and final inventory/matrix reconciled before the single S1 PR merges.
6. **S2 / FR-S2a,b:** gated only on A's specific all-writer outcome projection
   under existing ProjectLease, with coherent status/outcome, attempt/scope/job
   attribution, strict actual counts and existing generation. No B store/index
   implementation or equal-generation/timestamp substitute. Full survey with
   skipped>0 may qualify; zero-error subset/incremental never promotes Unknown/
   Partial. Failed/partial latest Sync requires later successful full Sync even
   after direct success. server_readiness target covers every truth-table row,
   unchanged-generation partial direct index, successful no-op Sync recovery,
   missing/malformed/legacy proof, restart, index health and changed metadata.
   Enforce two observations + one read-only health check, no retries/Repair.
   Update ipc_protocol/node_client Rust fixture and client guards/tests; build
   client before native execution. If A is unready, park S2 and continue independent
   #81; S1 does not depend on this or full CF11.
7. **S3 / FR-S3a:** website_http target runs the bounded example with the real
   daemon. Named client/example Node scripts check/build/test/format:check; wire
   generated artifacts into all full/native test entrypoints. Preserve original
   caller budget/abort/deadline through synchronous work, no replay/tools/roots.
8. **S4 / FR-S3b/SC-S2:** full cli-full + lean suites, opt-in target Clippy,
   fmt/diff, client/example scripts, exact-head docs/linux/macos/windows and final
   test-inventory/artifact audit. Fresh whole-diff review; real ~20k remains
   user-deferred/unverified, with no synthetic capacity claim.

## Delta 2026-11-13 — ADDED: S1a #152 bounded-read slice

Before S1a runtime coding, obtain the original reviewer's scoped addendum PASS.
Include #152 in the same integrated S1 PR; do not reopen closed S plan findings
or create a standalone PR unless implementation proves it necessary.

Extend S1a files with src/config/capture.rs (crate-internal shared bounded byte
reader), src/config/mod.rs, src/config/observe/source.rs and daemon/server.rs
launch/restart stamp integration. Reuse/extract the observer's existing bounded
take(cap+1) primitive while preserving its no-follow/race/CF5 adapter behavior.
Use explicit current MAX_STATE_BYTES=1MiB, no new private default/configurable
bootstrap field. Parse startup settings and stamp from one capture; bounded
post-startup hashing distinguishes Missing/Loaded/Unavailable and always reports
current Unavailable. Diagnostic identity remains separate from CF5 authority.

RED then GREEN in config/capture unit cases plus daemon_settings/config/
config_observation targets: actual CountingRead/unknown-length consumption at
most budget+1; source growth after metadata admission; misleading metadata length
does not determine allocation; startup parse+stamp share bytes despite later
file mutation; Missing -> oversized/unreadable restart notice; unchanged Loaded/
Missing and changed/deleted file behavior. Use tiny trusted budgets, explicit
seams/counters and bounded fixtures, no huge-allocation/RSS/timing thresholds.
Copyable extra verification commands are in server-plan.md's #152 addendum;
its existing S1a/config gates still apply. Commands are planned, not executed.

Update S1's inventory/dependency matrix classification: current I52 bootstrap1MiB
is shared and actually enforced; configurable bootstrap I52 remains deferred.
Launch capture/restart diagnostics have runtime/diagnostic-only effects, no
metadata/index/vector/ANN invalidation or CF5 publication dependency. No CF5/v2
executor, source write or strict activation is in this task.
