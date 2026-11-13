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
