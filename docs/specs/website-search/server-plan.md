# #79 S — fixed-project lexical website service

Draft baseline 2026-11-13, base main0c6d707 after client PR141.
**MODIFIED 2026-11-13:** architectural challenge I1..I5 and three minors resolved
in this plan amendment; original reviewer's scoped rereview is still required.
Goal: FR-S1..S3 and detailed FR-S1a..S3b / SC-S2. No capacity claim from synthetic
fixtures; real ~20k FR-S4 was explicitly deferred by the user.

## Constraints, ownership and dependency order

Rust1.100.0 / Node24.22.3 / pnpm12.16.1. No new Rust crates/runtime npm deps,
paid APIs/model downloads, retrieval/baseline changes, v2 activation or migration
executor. B owns daemon/lifecycle/IPC/watch/CLI composition; A owns
store/vector/dense/index and CF5/CF11. Reuse SearchOperation, scoped Activity,
ReadPool, PayloadBudget, existing index_generation, project read/writer admission,
sync deduplication and singleflight. Full CF12 I41..I58 and #81 remote async remain
separate; the narrow A outcome dependency below is not full CF11.

Land the independent #143 fix first, then the separately reviewed narrow
[#147 CLI sync selector fix](https://github.com/punkhomov/docsbase-memory-mcp/issues/147),
then S1. #147 fixes the supplied sync path currently ignored in the daemon branch;
it does not own the website-profile policy added in S1b. S1a/S1b/S1c form one
integration PR with separate fresh task reviews. Merge only after every accepted
owner field reaches its consumer, with no ignored setting or consumer fallback.
S2 then S3 can ship as separate PRs after S1; S3's runnable readiness integration
depends on S2. S4 audits the combined implementation.
S2 additionally requires A's specific all-writer outcome projection described
below. If A is unready, park S2 and continue independent #81 work in the user's
queue; do not block S1 or implement a B-owned storage substitute.

## Owner composition and launch snapshot (FR-S1b)

Create `src/daemon/settings.rs::DaemonSpec`: owner Patch/Settings/Defaults, field
scope, units/provenance, validation, view_fields and pure runtime assessment.
Global ConfigSpec composes it using the existing HybridSpec/SearchSpec pattern;
the aggregate knows the owner type but owns no daemon operational defaults.
ProjectFileConfig has no daemon section. SourceLayer/preview/migration retain
the global owner data and effect; a second global-file parser is forbidden.

Capture bytes and revision once at the launch boundary, resolve a complete
immutable policy, and pass it to Shared/lifecycle/session/admission. Validate
structural shapes, cross-field limits, existing project identity and lexical
policy before cache/admission-lock writes, runtime construction, listener or
daemon-state publication. Do not obtain the revision through a second read.
Document existing constructors as adapters to this owner resolver; explicitly
test legacy invalid-global warning/fallback behavior. Website startup fails
closed. The existing global-change restart diagnostic remains.

Owner settings: profile; Shared idle grace5000ms; connection64/event1024/session256
limits; search4active/32pending and control2active/16pending; control admission
timeout2000ms; **max_blocking_threads512**; lifecycle poll50ms, janitor500ms,
reply-drain5000ms and drain-poll20ms. Preserve operational defaults already present.
Zero pending means immediate acquisition or refusal, with no retained waiter.
Positive count fields must fit their actual consumers, including
`tokio::sync::Semaphore::MAX_PERMITS` for active, pending and connection gates and
Tokio bounded-channel semaphore limits for events; validate session allocation
and all checked sums before constructors that can panic. Grace accepts0 in
Shared. Other intervals/timeouts must be positive and fit checked Duration and
Instant arithmetic; overflow is a typed config error. Do not import unrelated
START/STOP/bootstrap/install constants into this slice. Wire sizes/version and
serialization remain compatibility invariants.

All adopted settings are immutable until restart, have owner defaults once,
and appear with provenance in effective views/source migration. Assessment is
runtime reconfiguration requiring restart/unsupported live executor, not index
invalidation or implicit strict-v2 activation; CF5 consent/apply is A's work.

### Profile shape and overlay (FR-S1a,b,d)

The global TOML shape is `[daemon]` with a complete tagged `profile` value:
`profile = { mode = "shared" }` or
`profile = { mode = "website", project_root = "/absolute/existing/root" }`.
Missing profile defaults to Shared with default provenance; explicit Shared
has global/CLI provenance even when its effective behavior is identical.
Unknown modes/keys, Shared with a root, Website without a root, and relative
roots are invalid explicit shapes, even if another layer would mask them.

CLI `serve --project ABS` and `serve --profile website --project ABS` each supply
one complete Website profile. `serve --profile shared` supplies complete Shared.
`--profile website` without its CLI root and `--profile shared --project ...`
are invalid; they never borrow a missing selector field from the file. CLI has
highest precedence for the **whole profile**, rather than merging mode and root.
Resolve absolute roots through canonical filesystem identity and the platform's
path-comparison rules, matching the existing registered root/project ID; missing
or unregistered roots fail without registration. Aliases of the same existing
root compare equal. An unrelated cwd never changes a selected Website root.

| Global profile | CLI profile selector | Result |
|---|---|---|
| omitted / explicit Shared | omitted | Shared; preserve distinct provenance |
| any valid profile | `--project A` | complete Website(A), CLI provenance |
| Website(A) | `--project A` / canonical alias | Website(A), CLI provenance |
| Website(A) | `--project B` | Website(B), if B independently passes admission |
| Website(A) | `--profile shared` | Shared, root removed by whole-profile overlay |
| any | invalid explicit mode/root shape | typed error before effects |

Lexical-only, watcher-off and no idle shutdown are Website invariants. They
cannot be turned on/off as conflicting resource overrides. `--detached` is a
Shared autostart marker only; reject it with Website. Change CLI `--grace-ms`
parsing to an optional patch: omission retains file value/provenance, otherwise
the owner default; explicit Shared0 remains0. Explicit shared idle grace in a
resolved Website policy (file or CLI) is rejected as inapplicable, rather than
silently ignored. The owner constructs no idle-grace timer for Website.

## Backend profile gates and explicit CLI routes (FR-S1a,d / FR-S2b)

Before RegisterSession/provider/backfill/index behavior, admit only the existing
fixed project and reject incompatible global/project hybrid policy. Gate every
watcher start/ensure path and all project bindings at backend entry points.
No reader-triggered registration/bootstrap index. Filter status/panel project
inputs **before** runtime/coverage callbacks; current registry-wide iteration
must not initialize another project's hybrid runtime. Website reader binding
uses existing BindReadOnly; its Project ID must match the fixed ID. Mutating or
auto-index registration cannot be used to bypass this gate.

`docsbase sync ABS` selects ABS even from an unrelated cwd (#147). With a live
Website daemon it resolves the existing fixed root read-only, binds using
BindReadOnly, and invokes the existing explicit sync route without registration.
With an omitted path, sync retains cwd selection: it must resolve to the fixed
project, otherwise it is refused. A different explicit root is a typed
profile/admission denial before ensure_project/provider/index work. The backend
checks sync_start's project again even if a caller supplies an ID argument.
Website permits only this fixed-project explicit mutation, watchers remain0,
and the existing one-project sync deduplication/ProjectLease is reused.

In S2 add `docsbase status --project ABS` for fixed-project `server_status` presentation:
read-only binding, selected root independent of cwd, no registration/providers.
Existing `docsbase status` remains registry-wide in ordinary Shared/direct mode;
when connected to Website its payload contains only the fixed project.

Once a live daemon returns a typed profile/admission denial, propagate its stable
error code through CLI/IPC/SDK; never encode it as the ordinary Project error
that CLI currently interprets as direct-index fallback. No retry, autostart or
direct fallback follows denial or a protocol/build/schema error. If the daemon
disconnects after connect/Hello/bind/sync dispatch, report interruption, do not
replay directly; an enqueued job may still be running. A later explicit call can
inspect its job/status. Before any daemon was contacted, ordinary `sync ABS`
with no live daemon retains direct indexing of ABS (including #147's path), and
direct registry status remains read-only. A globally selected Website profile
instead requires its daemon: unavailable is a typed error with an explicit
foreground-start instruction, with no direct index/autostart. Explicit
`status --project ABS` requires the live daemon, regardless of global profile.
Direct `docsbase index ABS` remains the deliberate direct-mode command; its
all-writer outcome must invalidate stale readiness evidence as specified below.

## Async admission and physical ownership (FR-S1c,d)

Create `src/daemon/worker_admission.rs` for search/control active+pending lanes.
Keep existing `src/daemon/admission.rs::Lease` and its daemon-lifetime
build/schema/lock admission callers intact. Reserve one bounded pending slot
before awaiting the fair active semaphore; pending guards leave on admission,
timeout, cancellation, future drop or shutdown. A full pending budget refuses
immediately. Search uses its original SearchOperation and absolute deadline.
After an active permit wins, recheck cancellation/deadline before spawning.
Move the permit and existing physical Activity ownership into the actual
spawn_blocking closure; caller-future cancellation cannot return that permit.
Release only after closure finish/unwind, using existing panic/error handling.
Control admission uses its finite owner timeout and daemon shutdown signal.
Hello/StopDaemon/signal handling stays async and requires no blocking lane.

### Same-token async cancellation (I1)

Add `Cancellation::cancelled().await` in `src/hybrid/work.rs` using Tokio Notify
on the **same shared Cancellation state** exposed by SearchOperation. Retain
existing blocking listeners and their Drop unsubscription. `cancel()` publishes
the permanent cancelled bit under its existing synchronization before notifying
all async/blocking listeners. The async wait creates/registers/enables its
Notified future before rechecking that bit; if already cancelled it returns
immediately, otherwise it awaits notification. This covers cancellation before
registration and between registration/check/await without a lost wake. Dropping
the async future removes its Notify waiter; no persistent parallel subscription.
Admission selects this wait, original deadline, shutdown and semaphore permit.
No blocking listener bridge, worker per waiter, periodic polling, parallel token
or second SearchOperation. Notify is additive in the existing Tokio dependency.

### Nonblocking lifecycle janitor (I2)

The accept loop owns exactly one optional tracked janitor future/state:
Idle -> AwaitingControl -> Running -> Idle. A tick starts it only from Idle;
ticks while awaiting/running are skipped/coalesced, without another task or
pending slot. The tick branch only sets state. Poll control admission and worker
completion as select branches alongside accepts, StopDaemon/shutdown changes,
signals and timers; never await busy admission or completion inside the tick
branch. A timed-out admission retires the attempt until a later tick. Ensure
ready accepts cannot starve shutdown or this tracked state (remove/reorder the
current biased select as needed). A queued janitor leaves cleanly on shutdown.
On spawn it transfers its control permit plus physical Activity into the closure
through prune_dead completion, even if the loop's waiter is dropped. Apply prune
results/grace bookkeeping only when completion is observed; Website never arms
idle grace. Graceful drain counts it, all running request closures and the
fixed-project sync through actual finish. Signals/escalation remain observable
during admission, execution and drain; abandoning a waiter does not claim to
interrupt its blocking closure or release its charge.

### Explicit blocking executor contract and closed inventory (I3)

Lifecycle calls `.max_blocking_threads(resolved.max_blocking_threads)` explicitly;
512 is DaemonSpec's default, matching pinned Tokio1.54.1's prior implicit default.
For Website validate with checked arithmetic **S + C + 1 + 1 <= B**, where S/C
are search/control active limits and B is blocking executor capacity. The first
1 is the fixed-project sync slot; the second is nominal helper headroom. Bounds
also meet Tokio's actual semaphore/channel limits, not merely usize conversion.
No blocking class outside the following inventory is supported in Website.

| Daemon-runtime work / source | Charged maximum in Website | Ownership |
|---|---|---|
| search `server.rs::call_tool_in` | S | search permit + physical Activity to finish |
| metadata/tools/status/panel including S2 | within C | control permit + physical Activity |
| BindReadOnly and RegisterSession workers `server.rs` | within C | same; reject incompatible bindings before effects |
| lifecycle prune_dead janitor | within C, at most1 | tracked coalesced state above |
| `server.rs::spawn_sync_job` fixed-project sync | 1, separate from C | dedup before spawn, singleton guard + Activity to finish |
| nominal filesystem/DNS/stdio/platform helper headroom | 1 reserved, unused on current native lexical paths | no unbounded helper may consume it; any adopted helper must be singleton and tracked before spawn |
| watcher, backfill, ANN refresh, dense/provider workers | 0 in Website | backend profile gates prevent initialization/scheduling |

Source audit currently finds native daemon spawn_blocking sites in server.rs
(bind, register, tool route, sync) and lifecycle.rs (janitor). Metadata is routed
inside those closures; synchronous file/SQLite/index calls do not spawn nested
Tokio workers. Native IPC uses async sockets/pipes, not tokio::fs/DNS/stdio;
the nominal helper slot is conservative unused headroom, never permission for an
untracked queue. Web frontend startup and MCP stdin belong separate processes/
runtimes; direct CLI indexing is also a separate process. They remain outside
this daemon executor guarantee and still contend for machine resources. Repeat
the source scan in S1 and gate any newly found daemon helper before claiming
the closed inventory; do not increase assumed headroom to hide unknown work.

Independent lane limits alone do not reserve threads. This inventory/sum makes
the lexical Website guarantee concrete: full search and one sync still leave C
physical executor slots for admitted control work. Several control consumers
share C fairly; no per-tool latency guarantee. Shared-mode request lane limits
still apply and constructors are validated, but legacy multi-project background
sync/backfill and provider helper/runtime behavior remain outside this physical
reserve proof until #81's separately reviewed orchestration. Do not claim shared
hybrid control reserve from S1. No dense/provider/index runtime refactor here.
Connection/session caps, PayloadBudget bytes and ReadPool handles stay separate;
worker reserve cannot free a connection when all fd slots are occupied.

## Readiness evidence and A-owned producer gate (FR-S2a,b)

Expose a versioned read-only `server_status` tool/DTO, CLI presentation and SDK
projection: profile/project, index health/current generation, attributable index
outcome, current/latest sync state/result, stable not-ready reason and restart
diagnostic. No mutating SDK API. Done alone is insufficient; strict uncapped
errors, not warnings length, governs quality. skipped means unchanged content.

Existing index_generation changes only with chunk-set changes. Direct indexing
has no SyncJob today, and a partial/no-op attempt can retain the same generation.
Therefore equal generations, latest Done or timestamps cannot prove completeness.
B must not infer an association by reading generation after a writer lease ends.

**Specific A dependency, required before S2 adoption:** A supplies a narrow
durable all-writer index-outcome projection for direct CLI full index, explicit
Sync, incremental/watcher, rebuild/recovery and every other existing writer.
The coordination request is recorded in
[#78 comment-6855914803](https://github.com/punkhomov/docsbase-memory-mcp/issues/78#issuecomment-6855914803);
that request is not evidence the producer has landed.
While holding the existing ProjectLease, every attempt marks prior proof no
longer current before it can mutate, and records terminal quality (including
errors in an otherwise Ok/Indexed pass), strict actual counts, producer/scope,
opaque index-attempt identity and the associated **existing index_generation**.
A publishes final Indexed/error status and this terminal outcome coherently
before releasing that same lease; nonterminal/crash/failed persistence must
remain visibly unknown/interrupted, never expose old proof as current. Full-pass
completeness must be distinguishable from a partial/incremental scope. Sync
outcome association must identify its job/attempt, including a successful no-op.
For this first adoption, only a verified full survey can establish Ready:
unchanged files counted as skipped are valid full-survey evidence, but a
zero-error subset/incremental batch cannot promote an Unknown/Partial corpus or
carry an earlier completeness claim forward. Supporting verified baseline-plus-
subset chains is A-owned future work, not an assumption in B's readiness check.
Exact storage/schema/API is A-owned. Attempt identity orders/attributes indexing
outcomes only: it is not another search generation, publication lease or CF11
availability protocol. B consumes this projection read-only and tests its
promised semantics; B does not edit store/index or synthesize timestamp proof.

The projection must make an older Done unable to mask a later partial direct
attempt **even when no chunks/generation changed**. A previously failed Sync
is conservatively blocking even after a later successful direct index: only a
later successful, attributable full explicit Sync clears that sync failure for
Website readiness. A later failed/partial direct attempt again makes it not
ready; any later successful full Sync (including unchanged/skipped-only) can
replace failed proof. Legacy indexes without the new producer's proof require
an explicit Sync after that producer is available; never backfill success from
legacy stats or filesystem times. This dependency gates S2, not S1/full CF11.

| Observation | Readiness / recovery |
|---|---|
| legacy/direct-only index without trustworthy current full-pass outcome; missing/malformed required stats/association | UnknownNotReady; explicit Sync after new producer lands |
| current queued/running attempt or Sync; restart-interrupted attempt/job | NotReady (`indexing` / `interrupted`); explicit Sync after work resolves |
| current failed/partial attempt or errors>0, even state Done/Indexed and unchanged generation | NotReady (`failed` / `partial`); repair sources and explicit Sync |
| latest Sync failed/partial, later direct outcome succeeds | NotReady (`sync_failed`); successful later full Sync required |
| zero-error partial/incremental scope without a current verified full survey | UnknownNotReady (`incomplete_scope`); explicit full Sync required |
| trustworthy successful current full pass, no active work/latest failed Sync, errors=0 (including skipped>0 or no changed chunks) | Ready only if remaining health/status/association checks pass |
| absent/unreadable/corrupt/rebuild-needed index, non-Indexed status | NotReady (`index_unavailable` / `rebuild_required`); explicit index/Sync repair, never status-triggered repair |
| metadata tuple changes during observation, including job/attempt change without generation change | NotReady (`state_changed`); no internal retry |

Apply state_changed first when a tuple moved; otherwise explicit active/failed/
interrupted/index-health reasons take precedence over unknown proof, then Ready.
A known failure is NotReady even if its counts are unavailable; absent counts
never convert a failure into success.

An admitted status observation with readable metadata makes **exactly two
bounded metadata observations** around a read-only ReadIndex health check,
under the existing project read/mutation admission and ReadPool/control budgets.
O1 captures fixed project/status,
index_generation, current attempt/outcome identity and quality, active/latest
Sync IDs/states/attribution and strict stats. Open/check `ReadIndex` read-only
with existing rebuild compatibility checks; never `open_or_create`/Repair,
registration, provider construction or a writer lease. End O1's short metadata
read transaction before the health check; O2 uses a fresh authoritative metadata
snapshot, not the old SQLite snapshot. O2 captures the same tuple from the
authoritative projection. Any tuple change returns state_changed;
no retry loop or hidden sync. Stable invalid evidence returns the table's reason;
only stable valid evidence plus healthy ReadIndex can be Ready. Admission/index
errors return typed unavailable/not-ready errors without retry; failed admission
or unreadable O1 need not perform the remaining reads/check. Bounded means two metadata
reads and one health check per admitted call, not a whole-corpus rescan. Release
existing read admission normally; result is a current observation, not a pinned
snapshot/search lease or old-generation availability during in-place mutation.

## External HTTP integration (FR-S3a)

The Node example uses the official SDK. Complete operator configuration owns
HTTP connection/active/header/body/query/result/deadline budgets and client
configuration. Bound admission before buffering JSON; validate a fixed route,
propagate abort/disconnect and map typed errors without query/document logs.
Reject browser-selected project/root/tool/provider controls. Authentication and
deployment belong to the embedding website. Rust remains native IPC only for
this service; no new public embedded HTTP server.

Add optional `CallOptions.budget {startedAtMs, deadlineAtMs}` in Node's
performance.now clock domain. Copy/validate finite values at ingress
(0<=start<=now, deadline>=start); a past deadline fails without I/O. Effective
deadline is min(caller deadline, caller start + client requestTimeoutMs).
Elapsed includes upstream preparation; queue_ms remains SDK lane wait. Omission
preserves existing ingress behavior. Retain OperationGuard/write checkpoints.
HTTP derives its budget before reading the body; browser input cannot select it.
An AbortSignal timer alone cannot extend time across synchronous preparation.
Challenge this additive API at S3 entry with actual tests.

## Ordered tasks, named targets and verification

Commands below are **implementation completion gates, not executed plan evidence**.
Run at repository root with pinned tools and writable CARGO_HOME/RUSTUP_HOME,
without changing HOME. Prepare Node/generated assets before relevant Cargo tests:

```sh
corepack enable
corepack install
pnpm install --frozen-lockfile
pnpm --dir web build
pnpm --dir packages/client build
```

### S1a — owner settings / launch snapshot (FR-S1b)

Create src/daemon/settings.rs and tests/daemon_settings.rs. Modify config/mod.rs,
source/resolve/view integration and daemon/CLI composition interfaces. RED real
session/connection override, omitted vs explicit0 grace/provenance, complete
profile overlay matrix, invalid shape/project scope/capacity before writes,
view/runtime-only assessment, single captured snapshot despite file change.
GREEN complete policy propagation; no hidden consumer defaults.

S1 owns the same-PR source scan/classification update in
docs/specs/component-configuration/{settings-inventory,state-dependencies}.md:
map I44 watcher profile invariant, adopted I45 grace subset, I46 caps, I47
timers/drain, new search/control active/pending/timeout and explicit Tokio
blocking capacity to DaemonSpec/consumers/units/provenance/restart effects.
Record still-deferred I41..I58 subsets and classify nominal helper headroom and
closed inventory as implementation invariants; do not claim full CF12 adoption.
Repeat this scan after S1b/c wiring and reconcile the final matrix before merge:

```sh
rg -n 'const |DEFAULT_|Semaphore::new|mpsc::channel|spawn_blocking|block_in_place|tokio::fs|thread::spawn|thread::Builder' src/daemon src/platform src/hybrid src/watch src/web src/mcp
RUSTUP_TOOLCHAIN=1.100.0 cargo test --locked --features cli-full --test daemon_settings --test config --test config_resolution --test config_runtime --test config_view
RUSTUP_TOOLCHAIN=1.100.0 cargo test --locked --test daemon_settings --test config_resolution
```

Fresh task review. S1a cannot merge without S1b/c connecting every accepted field.

### S1b — fixed lexical lifecycle / CLI policy (FR-S1a,d / FR-S2b)

Depends on #147 and S1a. Modify src/cli/{mod,status}.rs, daemon lifecycle/server/
session registration, src/error.rs/IPC error projection and watcher entry points.
Create tests/server_profile.rs; extend tests/cli_routing.rs, lifecycle.rs,
watcher.rs and error_codes.rs. RED session-less Website stays live; Shared grace
including0 still exits; real file change causes no watcher update; alternate
project/read-only/mutating binding denied before effects; hybrid rejected with
no model/provider/vector initialization; profile overlays and CLI fixed-root
sync matrix above; supplied path from unrelated cwd works and is never ignored;
typed denial cannot direct-fallback; no-daemon ordinary direct vs Website policy;
interruption after Hello/bind/enqueue has no replay. GREEN explicit fixed sync
updates a real index with watchers0. Cover stop/restart, unknown/unindexed root,
malformed config and existing status/panel callbacks never touching unrelated
hybrid projects. The new readiness DTO/status --project route remains S2 work.

```sh
RUSTUP_TOOLCHAIN=1.100.0 cargo test --locked --features cli-full --test server_profile --test cli_routing --test lifecycle --test watcher --test error_codes
RUSTUP_TOOLCHAIN=1.100.0 cargo test --locked --test server_profile --test cli_routing
```

Fresh task review; preserve ordinary CLI/MCP Shared compatibility.

### S1c — physical worker admission / janitor (FR-S1c,d)

Create src/daemon/worker_admission.rs and tests/server_workers.rs; modify daemon
mod.rs/lifecycle.rs/server.rs/request.rs/scoped.rs and src/hybrid/work.rs.
Preserve existing daemon/admission.rs::Lease and tests/admission.rs. Wire every
inventory path, actual Activity/sync lifetime, explicit runtime builder capacity
and the coalesced janitor. RED active/pending caps and FIFO; cancellation before
async registration, during wait and racing permit release; deadline unchanged;
waiter drop leaves no Notify subscriber/pending slot/worker; expired waiter
never spawns; running closure keeps permit after caller cancellation. A small
deterministic runtime with B=S+C+1+1 fills search and sync, then gates control
execution to prove it enters before those workers are released. Invalid sums,
overflow and >MAX_PERMITS fail before writes/runtime creation. Saturate C and
gate janitor: accepts, StopDaemon and signals still advance, only one janitor is
retained, and its Activity/permit remain charged to finish. Graceful stop counts
all real work. Use barriers/channels/clock seams, not CI latency assertions.

```sh
RUSTUP_TOOLCHAIN=1.100.0 cargo test --locked --features cli-full --lib hybrid::work
RUSTUP_TOOLCHAIN=1.100.0 cargo test --locked --features cli-full --test server_workers --test ipc_server --test lifecycle --test admission --test admission_ux
RUSTUP_TOOLCHAIN=1.100.0 cargo test --locked --test server_workers --test lifecycle
RUSTUP_TOOLCHAIN=1.100.0 cargo clippy --locked --features cli-full --all-targets -- -D warnings
cargo fmt --check
git diff --check
```

Fresh task review; final S1 owner/consumer/inventory audit before the one PR ships.

### S2 — explicit sync/readiness (FR-S2a,b)

Entry gate: A's specific all-writer producer/projection above is landed and
behaviorally reviewed, including no-generation-change partial direct indexing
and coherent final status/outcome under ProjectLease. Otherwise park S2 and
continue independent #81; no unsafe B approximation. Add protocol/panel DTO and
fixed-project server_status route in src/ipc/{protocol,panel}.rs and daemon
server/tools, CLI status presentation, packages/client/src/{types,protocol,index}.ts
and read-only SDK method. Intentionally update Rust-owned wire fixture in
tests/node_client.rs/packages/client/test/fixtures and its Node guards/tests.
Create tests/server_readiness.rs; extend server_profile and client tests.

RED every truth-table row: direct-only legacy including partial/no-op; missing/
invalid strict stats; stale Done then later direct partial with unchanged
generation; restart-interrupted; failed/partial Sync then later direct success
stays not-ready; successful no-op Sync clears previous failure; errors>0 vs
skipped>0 in a full survey; zero-error incremental scope cannot promote unknown/
partial evidence; absent/corrupt/rebuild-needed index; generation/attempt/job changes
between O1/O2. Assert exactly two metadata observations, one read-only health
check, no retries/repair/side effects. Real add/edit/remove explicit Sync while
watchers0 and ordinary current-mutation refusal remain covered. B adopts only
A's readonly projection; no new search generation or publication protocol.

```sh
pnpm --dir packages/client check
pnpm --dir packages/client build
pnpm --dir packages/client test
pnpm --dir packages/client format:check
RUSTUP_TOOLCHAIN=1.100.0 cargo test --locked --features cli-full --test server_readiness --test server_profile --test ipc_protocol --test node_client
RUSTUP_TOOLCHAIN=1.100.0 cargo test --locked --test server_readiness --test ipc_protocol
RUSTUP_TOOLCHAIN=1.100.0 cargo clippy --locked --features cli-full --all-targets -- -D warnings
cargo fmt --check
git diff --check
```

Fresh task review; build the modified client before node_client execution.

### S3 — Node HTTP example/original HTTP budget (FR-S3a)

Depends on S2's read-only readiness API. Create
examples/website-search/{package.json,tsconfig.json,config.ts,server.ts,
test/server.test.ts,README.md} with build/check/test/format:check scripts using
existing pinned workspace dev tools; include in workspace. Modify client
CallOptions/OperationGuard plumbing and packages/client/test/client.test.ts for
the additive budget above. No runtime dependency. RED body/admission limits,
invalid method/options, aborted body/search, deadline spent before/during SDK
preparation, typed overload/denial/daemon error mapping, no tool/root controls,
reconnect with no replay. A completed req.close is not disconnect: use
req.aborted/res.close and remove listeners. GREEN example with actual daemon and
explicit fixed-project readiness; create tests/website_http.rs to launch the
example against the real daemon using the already built SDK/example artifacts.

```sh
pnpm --dir packages/client check
pnpm --dir packages/client build
pnpm --dir packages/client test
pnpm --dir packages/client format:check
pnpm --dir examples/website-search check
pnpm --dir examples/website-search build
pnpm --dir examples/website-search test
pnpm --dir examples/website-search format:check
RUSTUP_TOOLCHAIN=1.100.0 cargo test --locked --features cli-full --test website_http --test node_client
git diff --check
```

Wire generated example preparation into every workflow/agent command that runs
website_http, preserving current client/Web preparation; fresh task review.

### S4 — combined native acceptance/docs (FR-S3b, SC-S2)

Extend existing native jobs without new required check names. Prepare generated
Web/client/example assets before full suites. Cover corpus startup/update/failure,
bounded overload/control, cancellation, disconnect/restart, real cleanup and
documented current-generation refusal. Update docs/IPC-CLIENT.md, WEB/CLI usage,
CI instructions/changelog and final settings inventory/dependency matrix.

```sh
RUSTUP_TOOLCHAIN=1.100.0 cargo test --locked --features cli-full
RUSTUP_TOOLCHAIN=1.100.0 cargo test --locked
RUSTUP_TOOLCHAIN=1.100.0 cargo clippy --locked --features cli-full --all-targets -- -D warnings
RUSTUP_TOOLCHAIN=1.100.0 cargo clippy --locked --features cli-full --test perf_budget --test artifact -- -D warnings
cargo fmt --check
pnpm --dir packages/client check
pnpm --dir packages/client test
pnpm --dir examples/website-search check
pnpm --dir examples/website-search test
git diff --check
```

Exact-head docs/linux/macos/windows green plus existing test-inventory/artifact
audit and independent strongest-model whole-diff final review are required.
Prepare normal squash delivery subject to the current user/repository authority.
Close #79 only when FR-S1..S3 are satisfied; explicitly retain FR-S4's user
deferral. Queue #81 / remaining CF12-CF13 separately, subject to S2 park rule.

## Challenge resolutions / limits

- A blocking bridge per waiter defeats admission; same-state Notify supplies the
  missing wake without workers. Cancellation never changes the original deadline.
- Waiting inside janitor's select body stalls accept/stop; one coalesced tracked
  future supplies bounded work and independently polled progress.
- Semaphores alone cannot reserve physical executor threads; explicit capacity
  plus the lexical closed inventory does. Shared hybrid behavior needs #81.
- Normal CLI-highest profile replacement is permitted; malformed explicit
  selectors and inapplicable idle flags are refused rather than ignored.
- Same generation/stale Done cannot prove current completeness; A attributes
  every writer outcome under the existing lease, and B returns stable not-ready
  for missing/changing evidence. Legacy requires new explicit Sync.
- Readiness remains an observation; in-place mutation may refuse search. There
  is no old-generation availability or new publication protocol.
- Worker slots do not imply free connection fds, machine-wide resources, real
  capacity or latency. New defaults and HTTP caller budgets require actual task
  tests/reviews. Real ~20k evidence remains unavailable and unverified.

## Delta 2026-11-13 — ADDED: S1a bounded capture / #152 (FR-S1b)

Include [#152](https://github.com/punkhomov/docsbase-memory-mcp/issues/152) in the
existing S1 integration PR, with S1a's implementation/review gates; no standalone
PR is planned. This addendum awaits the original reviewer's scoped PASS before
S1a runtime coding and leaves the closed I1..I5/M1..M3 contracts unchanged.

Source evidence: config/mod.rs::read_capped currently checks metadata.len then
uses unbounded read_to_string, so growth/unknown length bypasses the cap.
daemon/server.rs::global_config_stamp separately uses unbounded fs::read and
maps missing and failed reads to the same None. config/observe/source.rs already
has a bounded take(cap+1) reader and Captured/SourceState observation machinery.
Reuse its bounded byte-I/O primitive by extracting a crate-internal helper in
src/config/capture.rs for that observer, legacy global/project read_capped,
launch capture and restart hashing. Preserve the observer's existing no-follow,
regular-file, race checks and CF5 semantics in its adapter; do not invoke migration
planning/SourceLayer-v2 activation just to capture a daemon launch source.

The trusted current bootstrap budget is crate::limits::MAX_STATE_BYTES (1MiB),
passed explicitly to the shared helper before reading. Check budget+1 arithmetic
and actual consumed/retained bytes: at most budget+1 may be read, regardless of
metadata size, source growth or unknown stream length. Reject >budget instead of
parsing/hashing a truncated prefix as a valid source. Metadata may reject early
but cannot authorize an unbounded read or set allocation capacity; buffers are
bounded from the trusted budget, never metadata.len. No daemon-private cap/default
and no config key read from the file whose initial read it would govern.

Startup parses/resolves and computes its diagnostic stamp from the **same one
bounded capture** (including legacy launch adapters), with no second file read.
Post-startup restart checks use the same bounded I/O helper and trusted budget.
Model capture/stamp state explicitly as Missing, Loaded(diagnostic digest), or
Unavailable(reason); raw bytes are Loaded only if the byte read was complete
within budget, while parse errors retain existing diagnostics/fail-closed policy.
Missing is a confirmed not-found outcome, not any I/O failure. Current Unavailable
always reports unavailable/restart-required, even if a prior unavailable reason
compares equal. Missing -> Missing and equal Loaded digests can report unchanged;
Missing -> Loaded/Unavailable, Loaded -> Missing/Unavailable or a changed digest
must report a diagnostic change/unavailability. Startup fallback errors remain
visible under existing Shared policy; Website still fails closed. This stamp
is restart diagnostics only, distinct from CF5 authoritative source identity,
prepare/apply revalidation or publication; it adds no consistency/ABA guarantee.

S1a RED cases: CountingRead with a tiny trusted budget proves actual bytes read
stop at budget+1 for a longer/unknown-size stream; a deterministic growth seam
after metadata admission proves growth cannot bypass that bound; misleading
large metadata cannot drive allocation. Changing the file after capture proves
launch settings and baseline stamp still derive from captured bytes. Restart
checks cover unchanged Loaded/Missing, changed/deleted files and initial Missing
becoming oversized/unreadable: these must produce a notice, never None equality.
Use explicit I/O seams/counters and a bounded oversized fixture, no huge-allocation,
RSS or elapsed-time threshold test. Extend tests/daemon_settings.rs, tests/config.rs,
tests/config_observation.rs and config/capture.rs unit cases. Additional S1a gates:

```sh
RUSTUP_TOOLCHAIN=1.100.0 cargo test --locked --features cli-full --lib config::capture
RUSTUP_TOOLCHAIN=1.100.0 cargo test --locked --features cli-full --test daemon_settings --test config --test config_observation
RUSTUP_TOOLCHAIN=1.100.0 cargo test --locked --lib config::capture
```

S1's same-PR inventory/state-dependency update must classify I52 as the existing
shared 1MiB bootstrap/read boundary now actually enforced on config/stamp reads;
full configurable bootstrap budgets remain deferred. Classify launch capture and
restart stamp as runtime/diagnostic state only, with no metadata/lexical/vector/
ANN invalidation or CF5 publication edge. No source writes, CF5/v2 executor or
strict activation are added. These are future runtime acceptance gates, not
executed tests or a claim that #152 is fixed already.
