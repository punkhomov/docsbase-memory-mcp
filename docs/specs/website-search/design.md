# Дизайн #79, этап C: Node/TypeScript connector

Дата: 2026-11-13. Контракт: requirements.md FR-C1..C9, NFR-C1..C4.

## Общие ограничения

- Rust 1.100.0, Node 24.22.3, pnpm 12.16.1, TypeScript 5.9.3.
- No new Rust crates; no runtime npm dependencies; no live model calls/downloads.
- Preserve cli-full, four required native checks and frozen retrieval baselines.
- Runtime defaults have one owner; transport only consumes complete settings.
- A owns store/vector/dense/index/migration; B owns IPC/lifecycle/watch/providers.

## Границы и общий контракт A/B

Клиент использует Request::Hello / BindReadOnly / Search / CallTool(status) и
Response из src/ipc/protocol.rs; database/store API не расширяется на этом этапе.
Project ID, build/schema handshake и index_generation остаются backend authority.
SearchCallContext передаёт остаток original operation, elapsed и queue delay.
После deadline/abort socket закрывается, backend сохраняет учёт реально живой
работы по существующему scoped/SearchOperation. Никаких новых snapshot/singleflight
или generation revisions в Node. Новые lifecycle/settings integration PR согласуют
одного владельца для server.rs/CLI composition; текущий PR их не меняет.

## Компоненты и интерфейсы

`packages/client/src/settings.ts`: validate required `ClientSettings`, immutable
snapshot, integer/range validation. Конфиг также требует expectedBuildId и
expectedSchemaVersion (positive safe integer). Без fallback defaults.
`protocol.ts`: wire constants, bounded decoding + exact chunk IDs, responses,
validation helpers. Rust-generated protocol fixture является cross-language gate.
`connection.ts`: net socket, one pending exchange, bounded frame collector,
unexpected bytes poison; Hello и read-only bind; close interrupts exchange.
`pool.ts`: lazy slot reservation before await, FIFO bounded pending, cancelled
waiter removal, close; pool не управляет deadline/reties independently.
`index.ts`: public DocsbaseClient, operation ingress/deadline/AbortSignal,
search/control pools, search args validation, typed result/status/errors.
`types.ts`: typed public options/results; opaque explain meta сохраняется как
Record<string,unknown>, не объявляется стабильной полной telemetry schema.
`test/*.test.ts`: fake net peer cases и native real-daemon entrypoint.
`tests/node_client.rs`: Rust fixture contract + subprocess Node acceptance на
реальном daemon; Node доступен во всех native CI jobs после Web setup.

Public API:
```ts
const client = new DocsbaseClient({
  endpoint, projectId, expectedBuildId, expectedSchemaVersion,
  settings: { searchConnections, controlConnections, queueLimit, requestTimeoutMs }
});
await client.search(query, options, { signal });
await client.status({ signal });
client.close();
```
Constructor только валидирует конфиг, не выполняет I/O. Lazy first operation
выполняет Hello+BindReadOnly; status даёт конкретный registry project и generation,
не утверждает completeness/readiness до server этапа. Hello metadata — diagnostics,
не публичная возможность отправить raw message. expectedSchemaVersion обязателен,
immutable и проверяется конструктором как positive safe integer; missing/invalid
значение — local validation error без I/O. Hello.schema_version также должен быть
positive safe integer и точно равняться expectedSchemaVersion до BindReadOnly;
отсутствие/invalid/mismatch — protocol error и закрытие socket. Поле schema в
Hello request не добавляется: это локальная проверка существующего ответа.
expectedBuildId сравнивается с Hello.build_id как exact string. Его формат —
`docsbase <CARGO_PKG_VERSION>` из Rust protocol::build_id(): это равенство версий
Rust-пакета, не fingerprint source commit или доказательство идентичности кода.
Разные commits одной версии могут иметь одинаковый build string, поэтому schema
проверяется отдельно; версия npm не определяет ни ожидаемый build, ни schema.

## Transport/data flow (FR-C2..C7)

Call entry first captures performance.now and absolute deadline from immutable
settings → validates/serializes bounded args under that same operation → checks
original abort/expiry before admission → reserves/queues pool slot → connect
under same deadline → Hello requires protocol/build/schema/capabilities →
BindReadOnly(process.pid, projectId)
→ one Search scoped frame or status CallTool → decode/validate → return socket
only after complete response. At every boundary original abort/expiry wins.
Synchronous preprocessing cannot be interrupted while JS runs, but its elapsed
time consumes the original budget; expiry then rejects without acquiring a pool
slot or opening/writing a socket. Neither validation nor serialization resets time.

FIFO pending stores bounded validated args; each has exactly one cleanup path.
Search context uses floored integer elapsed/queue and remaining; zero remaining
rejects before dispatch. Elapsed starts at original ingress, including validation/
serialization; queue_ms measures only time waiting for a pool slot. Transport
frame cap mirrors Rust 8MiB. Separate pools
prevent a saturated search pool from consuming all control slots. Daemon has its
own connection/admission limits; client pool doesn't claim global resource policy.

Response parser uses Node JSON reviver source to inspect chunk_id raw number
token before Number rounding. Only `^(0|[1-9][0-9]*)$` and 0..2^64-1 are accepted;
sign (including -0), fraction and exponent syntax are rejected. App output is a
canonical decimal string, including "0" and "18446744073709551615" at the bounds;
all other unsafe integer numbers are rejected. Arrays/objects validated; required
citation path/heading/lines/score checked, safe integer bounds for status IDs/counts.
Unknown additive response properties retained; no text conversion/HTML rendering.

Native endpoint only: absolute Unix path, or canonical local Windows named-pipe
path. No tcp options. Owner supplies endpoint; same user/PID namespace only.
Socket errors, EOF/framing, unexpected extra messages permanently poison slot.
Next operation creates/negotiates a new connection; no replay within operation.

## Ошибки, ограничения и завершение

`DocsbaseError` carries kind, optional daemon code/instruction and cause. Daemon
messages originate backend redaction; connector doesn't log query/provider data.
Timeout/abort/close reject with distinct local kinds and destroy connection.
Validation/protocol errors don't expose peer frames. Local option limits use
representability checks, frame invariant; no invented policy maximum pool size
other than daemon-known capacity, which caller config must respect.

## Зависимости и ADR-C1

Node builtins net/buffer/perf_hooks/test/assert only at runtime. TypeScript5.9.3
already exists in workspace; add exact @types/node24.19.2 development dependency
(publication2026-10-09, verified registry2026-11-13; cooldown satisfied). Pnpm may
add transitive undici-types, no runtime HTTP use. Native TS tests use Node type
stripping; `tsc` emits JS + declarations for distributable package. Package added
to pnpm workspace; new dev deps lock via normal enforced policy, no overrides.

Alternatives: CLI subprocess rejected (fallback/startup per request); generic MCP
client rejected (wrong public scope); multiplexing rejected (wire is sequential);
second Rust HTTP server unnecessary for external Node integration. Manual .d.ts
without implementation typecheck rejected in favour of compiled TypeScript.

## Испытания и приёмка

C1 transport: split multibyte, oversized/EOF/invalid frame, extra bytes; chunk_id
0/MAX(u64) return canonical decimal strings, MAX+1/negative/fraction/exponent
(including -0 and 1e0) reject rather than round or coerce;
Hello wrong protocol/build/capability; schema missing, zero/negative/fractional/
unsafe/non-number or mismatched positive integer even with matching build string.
Accepted matching positive-safe-integer schema allows bind; every rejected Hello
closes before bind/search/status. Config tests reject omitted/invalid
expectedSchemaVersion without I/O. Mock asserts no RegisterSession/admin frames.
C2 orchestration: gated slow search + control, queue overload/abort/FIFO/close,
deadline includes preprocessing+queue+Hello. Deterministic monotonic-clock tests
advance time during validation/serialization and assert that emitted context
elapsed/remaining charges it; preprocessing exhausting budget rejects before
pool admission or any connect/write, for search and status. No automatic replay
after disconnect; restart next
call rehandshakes. Tests use explicit gates/events, mock timers where local policy
requires time; wall budgets only deadlock guards, not performance expectations.
C3 real daemon: indexed small corpus, filter/page dedupe/snippets, read-only unknown
project, no vector/provider state, restart and reconnect; Rust owned fixture check
prevents schema/constant drift. Native CI runs package build/check/test and Rust
E2E in existing jobs; no extra job, no repeated Rust compilation for Node suite.

## Risks / exit criteria

Idle broken connections can be discovered only on next read/write; operation fails,
not replayed. Conservative non-retry is intentional even for lexical. No claims
about continuous availability during sync; current backend generation rejection
is preserved. Live 20k acceptance belongs server phase and remains explicit open.
C-stage converges only with C1..C3 task reviews, final review, all4 green checks.

## Delta 2026-11-13 — MODIFIED: operation ingress и compatibility

Public config/Hello contract теперь явно требует expectedSchemaVersion и equality
до bind, независимо от package-version build string. Operation timestamp/deadline
создаются до любых argument preprocessing; C1/C2 acceptance tests покрывают оба
уточнения. Это исправления плана до реализации, не свидетельство пройденных тестов.
Chunk ID parsing также получает явные raw-token syntax/range и boundary tests.

## Delta 2026-11-13 — C3 artifact/fixture enforcement

Reuse the existing pinned Biome2.6.3 as client dev-only tooling, with package-local
configuration (VCS disabled to avoid sibling/worktree ignore-root panic). No new
runtime dependency/crate. Rust fixture canonicalizes map key order recursively,
including encoded frame JSON, preserving u64: cli-full's preserve_order changes
ordering only. Node consumes the same fixture. Native jobs pack/extract locally,
execute public ESM and typecheck an external consumer; no npm registry publishing.
Real ~20k measurement deferred by user ruling in requirements delta, not verified.

## Delta 2026-11-13 — ADDED: S-stage design baseline

See [server-plan.md](server-plan.md) for implementation boundaries, APIs, tasks
and challenge points. ConfigSpec already delegates to HybridSpec/SearchSpec;
DaemonSpec joins that pattern. The config aggregate knows the owner type, but
daemon defaults/validation/field views/impact remain in daemon/settings.rs.
A second independent global-file parser is not introduced.

The website profile is a typed launch policy for one explicit existing project.
Its lexical-only, watcher-off and persistent lifecycle behavior are profile
invariants. Operator resource settings remain configurable. A complete launch
snapshot is passed to Shared before listener/state publication; project and
hybrid admission precede registration/provider/index side effects. Legacy entry
points remain explicit adapters, with prior behavior covered by tests.

Worker admission is daemon-owned and independent from connection limits,
PayloadBudget bytes and ReadPool handles. Search/control lanes have finite active
and pending budgets; guards move into blocking closures and survive caller
cancellation. Existing SearchOperation/scoped Activity are reused. This is the
current blocking orchestration boundary; #81 will move remote waits outside it.

Readiness is a bounded current observation of index health/generation and sync
state/result. It is not a publication fence and promises no uninterrupted old
generation serving. HTTP remains an external Node integration; daemon serves
native IPC, not an embedded public HTTP search service.

## Delta 2026-11-13 — MODIFIED: S architectural challenge decisions

The amended [server-plan.md](server-plan.md) is the cold-start implementation
contract for FR-S1a..S3b. Keep #143 -> narrow reviewed #147 -> integrated S1
ordering. #147 supplies the explicit CLI sync selector; S1 supplies profile
admission/no-fallback semantics. S1a/b/c have separate fresh reviews and one PR;
accepted settings cannot merge before consumers. Same-PR source scan updates
component-configuration/settings-inventory.md and state-dependencies.md.

DaemonSpec owns explicit max_blocking_threads512 and actual Tokio count bounds,
in addition to all earlier owner settings. Lifecycle configures the builder
explicitly. For lexical Website, checked search-active + control-active + fixed
sync1 + nominal helper headroom1 must fit executor capacity before writes or
runtime construction. The closed inventory covers search, control metadata/
bind/register, coalesced janitor and one deduplicated sync through physical
completion. Native lexical IPC currently needs no filesystem/DNS/stdio helper;
headroom is not permission for an untracked queue. Watcher/backfill/ANN/provider
workers are gated off. Shared hybrid background/provider helper behavior remains
legacy and is outside this physical reserve guarantee until #81, without a
dense/index/provider implementation expansion. FD/connection, bytes and read
handles remain independent limits (FR-S1b/c/d).

Use new daemon/worker_admission.rs; preserve existing daemon/admission.rs::Lease
and daemon-lifetime build/schema/lock callers. Search admission selects fair
semaphore, original deadline and an additive Tokio Notify async wait on the
same hybrid/work::Cancellation state. Publish cancellation before notifications;
enable async registration before rechecking state so no cancellation window
loses a wake. Existing blocking subscribers remain. Drop cleans pending/Notify
ownership; a worker's permit/Activity stays in its closure. No polling or blocking
bridge per waiter. Lifecycle retains one janitor state and polls its admission/
completion alongside accept/shutdown/signals; busy ticks coalesce without await
inside the tick branch or another retained task. Graceful drain counts the
janitor and explicit sync to actual finish (FR-S1c/d).

Global TOML profile is an atomic tagged Shared(no root)/Website(absolute root)
value. CLI --project selects complete Website; --profile shared replaces the
file profile, and CLI has full-profile precedence. Reject malformed explicit
shapes and Website CLI mode without its CLI root; never fill it from a file.
Compare canonical registered identity, preserving omitted/explicit Shared
provenance. Optional CLI grace patch preserves file provenance on omission and
explicit Shared0. Website rejects explicit idle-grace/detached flags. Fixed
sync/status binding uses read-only existing-project lookup and BindReadOnly;
typed live-daemon profile/admission denials never fall into Project direct-index
fallback. Ordinary no-daemon Shared sync retains direct mode; globally selected
Website/no-daemon and interrupted-daemon cases fail without replay/autostart.
Backend rechecks every selected project before side effects (FR-S1a/b/d/S2b).

Readiness cannot associate latest Done with current content using equal
index_generation or timestamps: generation changes only for chunk-set changes,
and direct partial/no-op indexing has no SyncJob today. S2 therefore waits for
A's specific narrow all-writer outcome projection, not full CF11. Existing
ProjectLease covers invalidating stale proof before mutation and coherent final
status/outcome publication with existing generation, strict actual counts,
completeness scope and attempt/job association. Direct CLI, Sync, incremental
and recovery/rebuild writers all record quality. B consumes this read-only;
no new search generation/publication lease or post-lease generation inference.
If unavailable, park S2 and continue independent #81; S1 is unaffected.

Only trustworthy current full-survey outcomes can establish Ready in this first
adoption. Full surveys with skipped unchanged files can qualify; zero-error
incremental/subset work cannot promote Unknown/Partial or carry completeness
forward. Legacy/missing/malformed proof is UnknownNotReady and requires explicit
Sync after the producer lands. Partial/errors>0/failed/interrupted, unreadable/
absent/rebuild-needed index and active work are NotReady. Later direct success
does not clear latest failed/partial Sync; a successful later full no-op Sync
does. A later partial direct pass invalidates prior Done even at equal generation.
The plan's truth table fixes stable reason/recovery behavior (FR-S2a/b).

Observe authoritative project/status/generation/attempt/outcome/Sync metadata
twice around one read-only ReadIndex health check, under existing project read/
mutation admission and ReadPool/control budgets. End O1's short metadata read
transaction before the health check; O2 uses a fresh snapshot. Any tuple change returns
state_changed without retry. Never repair/open_or_create, initialize providers,
register, take a writer lease or trigger hidden Sync. This bounded observation
does not pin publication or old-generation availability during in-place writes.
All these are planned contracts awaiting implementation evidence, with real
~20k capacity/latency acceptance still user-deferred and unverified.

## Delta 2026-11-13 — ADDED: S1a bounded launch/restart capture (#152)

FR-S1b's single launch snapshot includes actual bounded I/O, not a metadata-only
length check. Reuse/extract the existing take(cap+1) primitive from
src/config/observe/source.rs into a crate-internal src/config/capture.rs helper.
Legacy config/mod.rs::read_capped (global/project), daemon launch capture and
restart checks use it with the explicit trusted MAX_STATE_BYTES=1MiB boundary.
The observer adapter retains its current no-follow/regular-file/race checks and
SourceState/CF5 behavior; sharing byte I/O does not activate its migration/v2 path.
No consumer-local cap or file-defined bootstrap recursion is introduced.

The helper checks cap+1 arithmetic, limits actual Read consumption/retained bytes
to that amount, rejects >cap and derives any buffer sizing from the trusted cap,
never metadata.len. Metadata is an optional early rejection, not proof of a
complete bounded read. Parse resolved launch policy and compute its diagnostic
stamp from the same captured bytes once, including legacy adapters. Post-startup
checks apply the same actual byte limit before hashing and cannot bless a
truncated over-budget prefix.

Keep explicit Missing/Loaded(diagnostic digest)/Unavailable(reason) states,
independent of parse-error diagnostics. Any current Unavailable reports a notice;
only Missing->Missing and equal Loaded digests may report unchanged. Missing to
oversized/unreadable is Unavailable and cannot disappear through None equality.
Existing Shared fallback diagnostics and Website failure behavior remain.
This diagnostic stamp is not CF5 authoritative source identity, publication or
revalidation, and does not strengthen concurrent-write/ABA guarantees.

S1a adds deterministic bounded-reader/growth/capture-sharing/restart-state tests
and preserves config_observation behavior. The same S1 PR classifies I52's
enforced current cap and diagnostic runtime state in settings-inventory and
state-dependencies, without artifact invalidation. Full configurable bootstrap
I52, source writes, CF5/v2 execution and strict activation stay deferred.
Plan addendum scope/commands are in server-plan.md; original reviewer scoped
PASS is required before runtime implementation. #152 closes with S1 integration.

## Delta 2026-11-17 — ADR-W1: application-owned Windows transport

Supersedes only ADR-10's interprocess transport ownership and no-op Windows I/O
timeouts. Keep byte-stream NDJSON, names, protocol and non-consuming probes.
Use the existing windows-sys dependency for an application-owned bounded IOCP
arena. Explicit `platform::transport::Options` and a synchronous `run` scope
produce a clonable Transport submission capability. The scope owns progress and
retirement outside the application runtime. Capabilities may not construct their
own ports or acquire hidden fallback policy. Unix delegates to existing sockets.

A pending listener is preserved across accept-future cancellation. Acceptance
transfers the same physical token; the next instance waits for an arena-capacity
notification registered before its allocation attempt. Pending, admitted,
rejected and retiring physical instances all consume that arena. Notification
alone never proves native retirement. Blocking streams poll the same operations
using a wake/condition and one absolute deadline per read/write/flush; timeout
aborts the exchange without waiting for native completion. Reader/writer clones
share the single serialized writer and never duplicate its logical Pipe. Final
successful server close is explicit; Drop is cancellation, never delivery.

The private core and the new explicit facade are being integrated in W3. Legacy
constructors remain temporarily during that task and must be removed from all
operational Windows paths before PR readiness. No wire/schema/crate change.

## Delta 2026-11-17 — W3 operational scope and delivery contract

Daemon, direct CLI command, read/control CLI command, MCP and Web compose one
explicit native scope outside their runtime. They pass its Transport capability
into bind/client/pool constructors; constructors never own a port/reaper. CLI
list pagination shares that scope. Test fixtures retain an explicit test owner
and exercise the same production adapter. Remove the obsolete interprocess
transport dependency; keep windows-sys already present in the dependency graph.

Before first native read, an admitted connection installs its original permit
on the native owner. Each decoded request installs its original Activity before
cancellation reads/writes; ordinary successful replies release that tail only
after native flush. Final close keeps the tail until the helper has actually
exited/joined and the physical record retires. Idle reads charge only connection
capacity. Physical retirement clears those tails even if cancelled runtime tasks
retain logical references. Drop/error/expiry may discard a failed exchange;
only successful explicit final close promises complete peer delivery.

Search write/flush/final close share the original SearchOperation deadline.
Already expired errors get only the existing immediate poll. Non-search control
delivery uses one `control_timeout_ms` deadline when its response becomes ready;
Stop's pre-reply drain and final ACK instead share its original `reply_drain_ms`
whole budget. Stop now closes after its ACK. The stop command reads/validates
that complete ACK, refreshing its original15s overall deadline before every
native read, and still observes process exit as the final success criterion.

## Delta 2026-11-17 — причинная проверка вооружённого idle grace (#165)

Регрессионный тест `registration_survives_armed_grace` перенесён из subprocess
fixture в настоящий in-process accept/RegisterSession pipeline. Наблюдение
вооружённого deadline происходит после sessionless Hello; однократный test-only
барьер расположен внутри уже допущенного registration worker, до его настоящей
работы с БД. Проверяются совместимый путь и путь с JobRoot. Источник `now` и
ожидание grace внедряются вместе в приватную accept-loop функцию; production
передаёт прежние Tokio Instant/sleep_until. Остальные deadlines/работа идут в
реальном времени, production grace и правила idle не меняются.

Тест доводит исходный grace до истечения при удерживаемой Activity и ещё
отсутствующей сессии, наблюдает повторную проверку idle, затем освобождает worker
и требует настоящий indexed-ответ и session binding. После disconnect требуется
повторное взведение grace и чистый idle exit. Проверки следуют за освобождением
барьеров/cleanup. Три существующие subprocess-проверки dead-session cleanup,
повторных соединений и shutdown сохраняются. Размер корпуса и задержка потока
больше не служат доказательством регистрации.

## Delta 2026-11-17 — I1: уведомление читателя после возврата отмены

Готовность native-операции для потребителя требует одновременно возврата
submit, terminal completion и нулевого числа выполняющихся cancellation callers.
Проверка и сохранение waker выполняются под одним mutex состояния операции.
Публикация completion может уже забрать прежний waker; пока caller удерживает
буфер, повторный poll сохраняет новый waker для его возврата. Новые попытки
отмены не входят в уже returned/terminal операцию, поэтому после Ready не
возникает нового caller между poll и копированием/потреблением результата.
Это исправляет потерянное пробуждение без изменения deadline или lifetime.

## Delta 2026-11-17 — W3e: подключение и уход клиента до accept

Windows blocking Client использует прежний IO_TIMEOUT2s как явный бюджет
первичного подключения. Новый `Transport::connect_blocking_until` принимает
исходный Instant deadline; только PIPE_BUSY231 запускает WaitNamedPipe на том же
вызывающем потоке. Успешное ожидание не резервирует экземпляр: последующий busy
повторяется под тем же deadline. Каждый конечный Win32 wait получает оставшееся
время в миллисекундах без значений server-default0/INFINITE; после него deadline
проверяется до новой попытки. Отсутствие endpoint, локальный capacity refusal и
другие ошибки не повторяются. Никакого запроса до соединения нет, replay после
него отсутствует. Raw connect_blocking и async connect остаются одной попыткой;
Stop сохраняет свой прежний общий15s цикл и не получает новый вложенный бюджет.

Если peer уже закрылся до ConnectNamedPipe, ERROR_NO_DATA232 завершает только
этот физический экземпляр. Listener убирает его из pending, просит abort,
сохраняет arena-владение до retirement и после cooperative yield начинает новый
accept в той же capacity. Прочие ошибки не маскируются; новый экземпляр ждёт
фактической ёмкости. Отмена accept после yield не оставляет reusable stale handle.


## Delta 2026-11-17 — L1, замена completion implementation

Retain PR164 application scopes and resource accounting; replace Operation,
OVERLAPPED storage and own IOCP dispatch with exact patched Mio/Tokio. A dedicated
transport I/O runtime is owned outside application work and remains alive until
Mio's last-field guards and optional drain workers retire. The bounded arena
contains tokens/accounting/helper ownership; tokens weakly reference endpoints
and the arena, avoiding a guard-to-endpoint reference cycle. Strong endpoint
references fence cancellation and helper FFI. Aborting the logical stream drops
its endpoint; the dependency owns remaining native buffers and completions.
Read capacity is explicit before registration, outgoing writes are capped,
guarded Drop prevents callback resubmission and cancels pending native work.
Successful close still has explicit native-write and final peer-drain boundaries.
No performance advantage is claimed by source size alone. See ADR-WLIB1/plan.
