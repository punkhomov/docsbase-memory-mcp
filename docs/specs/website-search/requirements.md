# Поиск сайта: официальный Node/TypeScript IPC-клиент (#79)

Дата: 2026-11-13. База: 4c9ff9f. Владелец потока B.
Пользователь разрешил непрерывную реализацию, создание PR и squash merge после
независимых reviews и зелёных docs/linux/macos/windows. #79 остаётся открытой до
выполнения серверного этапа и реальной corpus acceptance. #81 — отдельный этап.

## Проблема, пользователи и границы

Node-сайт должен использовать долгоживущий docsbase через локальный IPC, без
subprocess на каждый поиск и без неявной индексации. Оператор управляет одним
зарегистрированным проектом; HTTP-пользователь передаёт только поисковые данные.
Первая поставка — lexical. Ключи, модели, расходы и Rust facade не требуются.

P1: как разработчик Node, хочу типизированный search/status для заранее выбранного
проекта. Given реальный daemon и индекс, When клиент ищет, Then возвращает настоящие
citations/snippets; status не выполняет search и административные операции.
P2: как оператор, хочу конечные очереди и отмену. Given заблокированный search,
When очередь заполнена/запрос отменён, Then результат — типизированная ошибка,
соединение не переиспользуется при незавершённом exchange, status имеет свой pool.
P3: как оператор, хочу пережить restart. Given закрытый daemon connection, When
следующий вызов подключается к новому daemon, Then заново выполняются Hello/bind;
уже отправленный запрос не повторяется автоматически.

## Первая поставка: client contract (FR-C*)

- FR-C1 MUST: Node >=24.22.3, ESM пакет `@docsbase/client`; TypeScript declarations
  и compiled JS, native net Unix sockets/Windows named pipes. Endpoint передаёт
  оператор; socket discovery не угадывает cache hash или произвольный TCP host.
- FR-C2 MUST: один immutable project ID, positive safe integer, на client instance.
  Hello protocol=1, exact caller-supplied expectedBuildId и обязательный
  expectedSchemaVersion. Expected и Hello.schema_version — positive safe integers,
  равенство schema проверяется до bind. Build ID равен `docsbase <CARGO_PKG_VERSION>`:
  exact string match проверяет версию Rust-пакета, не fingerprint исходного commit.
  Require search-context-v1 и panel-read-v1, literal-query-v1 до literal dispatch.
  Read-only BindReadOnly, process.pid; никаких
  RegisterSession/RegisterUnbound/StopDaemon/admin tools.
- FR-C3 MUST: capped NDJSON, arbitrary fragmentation/UTF-8 boundaries; frame limit
  8MiB — существующий wire invariant, не configurable runtime default. Malformed,
  incomplete, oversized, unsolicited/extra response closes connection. Outbound
  limit проверяется до write. chunk_id принимается только как raw JSON number
  token с unsigned integer syntax `^(0|[1-9][0-9]*)$`, в диапазоне 0..2^64-1;
  sign/fraction/exponent отвергаются. Приложение получает canonical decimal string
  без округления; unsafe JSON integers в иных полях отвергаются.
- FR-C4 MUST: search(query, options, call) и status(call), metadata hello getter,
  close(). Нет публичного raw call/tool passthrough. Search options следуют
  search_docs, omitted остаются omitted; не менять ranking/defaults/filtering.
  SearchResult — hits либо {hits,meta}; schema проверяется на runtime, additive
  поля разрешены. Daemon error code/message/instruction сохраняются отдельно от
  transport/protocol/deadline/cancelled/overloaded/closed errors.
- FR-C5 MUST: отдельные bounded search/control pools; один exchange/connection,
  bounded active и pending, отменяемая FIFO queue. Complete policy обязательна
  при create: searchConnections, controlConnections, queueLimit, requestTimeoutMs.
  Один владелец ClientSettings разрешает/валидирует её; transport не вставляет
  defaults. Queue limit per pool; zero = немедленный overload при заполненном pool.
- FR-C6 MUST: монотонный ingress timestamp захватывается первым действием
  search/status, до validation/serialization аргументов. Один absolute operation
  deadline включает preprocessing, admission, connect, Hello/bind, write/read.
  После preprocessing истёкшая операция отвергается до admission/connect/write.
  Search context remaining/elapsed/queue_ms берёт исходную operation, без reset;
  elapsed включает preprocessing, queue_ms измеряет только ожидание pool slot.
  AbortSignal до/во время ожидания; timeout/abort destroys interrupted connection,
  очередь/permits освобождаются ровно один раз.
- FR-C7 MUST: reconnect только в следующем независимом вызове. Никаких retries
  внутри уже начатой операции, autostart, CLI/direct fallback или network provider
  вызовов в connector. Close идемпотентен, отвергает очередь и активные операции,
  закрывает сокеты и не останавливает daemon. Permanent disconnect освобождает
  idle slots. Session/PID supported boundary: same OS user + same PID namespace.
- FR-C8 MUST: Rust-owned protocol fixture связывает JSON shapes/constants/error
  codes и citation schema с Node parser tests. TypeScript compile/build/package
  smoke; package не private, но npm publish/credentials не нужны для этого этапа.
- FR-C9 MUST: Node fake peer tests + real daemon E2E в native CI: search/snippets,
  filter/dedupe, project isolation, readiness diagnostics, disconnect/restart,
  compatibility failure, cancellation, bounded pool. Без real providers/downloads.

## Следующие этапы, не критерий закрытия первого PR

- FR-S1: server profile задаёт один проект, watcher off, daemon без idle exit,
  bounded execution и явно resolved owner settings (CF12). Не менять auto-index
  legacy CLI/MCP; Server profile не делает поиск основанием для индексации.
- FR-S2: explicit CLI sync/readiness, completeness/skips, generation/restart/no-index/
  failed-sync semantics. Пока supported отказ при index mutation: не обещать
  непрерывное чтение старого поколения до CF11. Failed/partial sync не ready.
- FR-S3: runnable Node HTTP example с fixed project, validation, bounded request
  bodies, abort propagation и error mapping; никакого browser-controlled tools/root.
- FR-S4: real ~20k Markdown corpus acceptance с заранее зафиксированными workload,
  budgets и provenance; cold/warm, updates/recovery, RSS/disk/p50/p95/p99/errors.
  Synthetic не заменяет real corpus; сейчас корпус/бюджеты ещё не предоставлены.

## Нефункциональные требования и проверка

NFR-C1: поддержка Linux/macOS/Windows native IPC; required checks прежние.
NFR-C2: ноль runtime npm dependencies, ноль новых Rust crates. Dev tooling pinned,
14-day age policy не обходится. Не менять frozen retrieval inputs/baselines.
NFR-C3: bounded retained parser bytes <=wire limit и bounded queue; pending input
args сериализуются/проверяются до admission, объём каждого <=frame limit. Client
владеет только своим API/transport состоянием; существующий SearchOperation,
generation checks, backend singleflight не дублируются.
NFR-C4: hidden provider requests =0 в lexical E2E; query/error logs не добавляются.

SC-C1: compile + Node suite + protocol fixture test успешны; packing даёт JS/types.
SC-C2: deterministic gate tests доказывают fragment framing, non-replay, queue cap,
FIFO cancellation, one request/connection и independent control admission. Clock
tests включают preprocessing в elapsed/remaining и запрещают I/O при исчерпании
deadline до admission. Handshake tests отвергают missing/invalid/mismatched schema
до bind, даже когда build string совпадает.
SC-C3: реальные native Node→IPC→daemon checks успешны на всех трёх OS.
SC-S1: #79 закрывается только после FR-S1..S4, не после client PR.

## Challenge / assumptions / non-goals

1. expectedBuildId и expectedSchemaVersion обязательны; версия npm не источник
   Rust build/schema. Build string — версия Rust-пакета, не source fingerprint;
   разные commits одной версии могут совпасть, поэтому schema сравнивается явно.
2. RegisterSession запускает config/providers/backfill: используем BindReadOnly.
3. EOF/timeout может случиться после платной работы: никакого replay.
4. JS number не сохраняет u64: chunk_id string, без silent rounding.
5. Read-only binding ещё не гарантирует lexical-config: server этап обязан
   валидировать profile; клиент не обещает override hybrid через rerank=false.
6. Same-user local socket не заменяет HTTP auth; filters не ACL. Cross-container
   PID namespaces и remote TCP не поддерживаются, пока не решены явно.
7. Существующий status registry-wide: клиент отдаёт только bound project, но не
   обещает изоляции от доверенного локального процесса с доступом к socket.
8. Нет hardcoded defaults на transport paths: только resolved ClientSettings;
   protocol constants и технические timer/int representability bounds исключения.

Открытые вопросы первого PR отсутствуют; реальные corpus/budgets нужны только
для FR-S4, не блокируют client/lifecycle implementation.

## Delta 2026-11-13 — MODIFIED: ingress deadline и schema handshake

FR-C6/SC-C2 уточнены: validation/serialization оплачиваются из исходного deadline,
а expiry после preprocessing запрещает admission и I/O. FR-C2/SC-C2 требуют
caller-supplied expectedSchemaVersion и точное safe-positive-integer равенство
Hello.schema_version до bind. Build string описывает package-version equality;
гарантия source-commit identity не заявляется.
FR-C3 уточнён: chunk_id имеет unsigned integer token syntax, u64 range и canonical
decimal string output; C1 проверяет обе границы и запрещённые формы записи.

## Delta 2026-11-13 — целевой корпус пока недоступен

MODIFIED FR-S4 / SC-S1: пользователь сообщил, что сейчас не может предоставить
реальный корпус/нагрузку/целевую среду, и разрешил пропустить этот прогон. FR-S4
откладывается и не блокирует реализацию/поставку client/server этапов. Сохранять
пометку «не проверено на реальном ~20k корпусе»; synthetic E2E не заменяют FR-S4,
capacity/production latency claims запрещены. После FR-S1..S3 допускается закрыть
implementation scope #79 с явным указанием отложенного измерения. Первый client
PR всё ещё не закрывает #79: FR-S1..S3 остаются обязательными.

## Delta 2026-11-13 — C3 native test preparation

MODIFIED SC-C1/SC-C3: every entrypoint executing the full Rust suite with `web`
or `cli-full`, or selecting `node_client` with either feature, must explicitly
build `packages/client` after workspace dependency installation and before test
execution. The ignored `dist/index.js` public export is not produced by install.
This includes the existing nightly release suite as well as required native CI
and documented agent/local commands. Preserve full-suite execution and release
profiles; no test skip, cfg weakening, npm publishing or new job/check is needed.
CLI-only builds, compile-only feature checks, lean suites and selected targets
that do not execute the Node E2E have no client-build prerequisite.

## Delta 2026-11-13 — ADDED: server-stage acceptance details

This is the S-stage design baseline, not an implemented capability.

- **FR-S1a:** Explicit foreground website profile binds one existing registered
  project and performs lexical search only. It starts no watchers, never exits
  merely because sessions are absent, and never registers/indexes/downloads a
  model because a reader connects or searches. Reject incompatible global/project
  hybrid policy and other project bindings before their side effects. Preserve
  ordinary CLI/MCP shared-daemon behavior outside the explicit website profile.
- **FR-S1b:** Daemon owner defines all new operational defaults, validation,
  provenance and runtime-change assessment. Parse the global launch snapshot
  once and pass complete immutable settings into lifecycle/Shared/session/admission;
  consumers do not reread a raw patch or fill missing settings. New global owner
  fields are rejected in project files and appear in effective views. Changes
  require restart, not index invalidation or automatic strict-v2 activation.
- **FR-S1c:** Bound physical search/control workers and pending waiters before
  spawn_blocking, with distinct reserved control-worker capacity. A queued search
  consumes its original SearchOperation deadline and cancellation; an expired or
  cancelled waiter starts no worker. Cancelling a caller does not release a live
  physical worker's permit. Bound connections/sessions separately; finite worker
  queues are not a promise against all connection slots being occupied.
- **FR-S1d:** Read-only binding, registration and metadata blocking paths obey
  control admission. Website sync remains explicit and fixed-project, using the
  existing sync-job deduplication/lease. Shutdown tracks actual outstanding work;
  no claim that dropping a blocking future stops execution. Preserve existing
  documented generation refusal/retry while index mutation occurs.
- **FR-S2a:** Expose a versioned fixed-project server readiness result. Ready
  requires a healthy readable current index and no current failed/partial sync.
  SyncState::Done with stats.errors>0 is incomplete. stats.skipped means unchanged
  content, not failure; warnings are capped and cannot replace the error count.
  Account for running/queued/interrupted sync, daemon restart, absent index and
  generation changes. This is a checked observation, not a CF11 snapshot lease.
- **FR-S2b:** Explicit CLI sync/status can inspect and update the fixed corpus
  while watchers stay disabled. Do not add mutating operations to the read-only
  SDK public API. Report failure/partial results without silently declaring ready.
- **FR-S3a:** Runnable Node HTTP example uses the official client, a fixed server
  project and complete operator-supplied configuration. Validate routes, methods,
  query/options and bounded headers/body/active requests before IPC dispatch.
  Propagate disconnect and one original HTTP deadline through preparation and SDK
  writes; synchronous work must not gain a fresh deadline. No replay/autostart,
  arbitrary tools, roots, provider controls or query/document logging. Auth and
  deployment policy remain the embedding website's responsibility.
- **FR-S3b:** Native E2E covers watcher-off explicit updates, incomplete sync,
  overload with independent control work, cancellation while queued/running,
  disconnect, restart/reconnect and shutdown accounting. Use real files/daemon
  and gated fake workers where deterministic; no paid APIs/model downloads.

**SC-S2:** Each S task has actual RED/GREEN and fresh task review. Final S-stage
acceptance requires relevant full suites, lint/format, native docs/linux/macos/
windows and independent whole-diff review. Preserve existing test inventory and
retrieval baselines. Real ~20k FR-S4 remains user-deferred, unverified and carries
no capacity/latency claim. Full CF12 and #81 remain separately tracked.

## Delta 2026-11-13 — MODIFIED: S challenge contracts

This amendment resolves the five Important and three Minor plan findings; it
does not claim implementation or passed runtime acceptance.

- **FR-S1a/b (MODIFIED):** Global `[daemon] profile` is a complete tagged value:
  Shared has no root; Website requires one absolute existing registered root.
  Missing Shared and explicitly selected Shared preserve different provenance.
  CLI `serve --project ABS` selects complete Website; `--profile shared` replaces
  the complete file profile. Website without its explicit CLI root, Shared with
  a root, relative roots and unknown selector shapes are rejected. CLI highest
  precedence permits file Website(A) -> CLI Website(B), subject to B's normal
  existing-project/lexical admission. Canonical aliases identify the same root.
  Omitted `--grace-ms` preserves file value/provenance; Shared explicit0 remains
  valid. Website rejects explicit idle-grace/`--detached` options as inapplicable.
- **FR-S1b/c (MODIFIED):** Daemon owner resolves an explicit blocking executor
  capacity (default512), active/pending lane counts and their complete consumer
  limits before writes/runtime construction. Validate checked sums and actual
  Tokio semaphore/channel limits. The physical control-reserve guarantee is
  scoped to lexical Website's closed inventory: search S + control C + one
  fixed-project sync + one reserved nominal helper slot <= executor capacity.
  Janitor/bind/register/metadata share C; no unbounded helper may bypass the
  inventory. Shared hybrid background/provider behavior keeps legacy semantics
  and is outside this reserve proof until separately reviewed #81 work.
- **FR-S1c (MODIFIED):** Queued search cancellation has a lost-wake-safe async
  wait on the existing Cancellation state, preserving blocking subscribers and
  the original SearchOperation deadline. No blocking bridge, polling worker,
  second token or worker spawn for a cancelled/expired waiter. Drop retires
  pending ownership; physical permits remain owned through closure completion.
- **FR-S1d (MODIFIED):** The lifecycle loop retains at most one janitor attempt,
  skips/coalesces busy ticks and polls its admission/completion beside accept,
  StopDaemon and signals. Busy control admission must never suspend the tick
  branch. Janitor and fixed sync keep physical Activity/permits until finish,
  and graceful shutdown counts them. Existing daemon-lifetime Lease is preserved.
- **FR-S1d/S2b (MODIFIED):** Land narrow CLI selector dependency
  [#147](https://github.com/punkhomov/docsbase-memory-mcp/issues/147) after #143
  and before S1. Explicit `sync ABS` uses ABS from any cwd; Website binds the
  existing fixed project read-only before its allowed explicit Sync, without
  registration. Different-root/profile denial has a typed stable code and never
  triggers direct fallback. Interruption after daemon contact never replays a
  mutation. No-daemon ordinary Shared sync retains deliberate direct indexing;
  globally selected Website requires an explicitly started daemon and does not
  direct-fallback/autostart. `status --project ABS` is a live-daemon read-only
  fixed-project route; ordinary direct registry status is preserved.
- **FR-S2a (MODIFIED):** Readiness requires trustworthy current **full-survey**
  quality evidence as well as healthy readable index, Indexed status, no active
  attempt/Sync and no latest failed/partial Sync. Missing/malformed counts or
  association, legacy-only/direct-only evidence without proof, and zero-error
  incremental/subset scope are UnknownNotReady. A full survey may count unchanged
  files as skipped and still be complete. Failed/partial/interrupted work,
  errors>0, absent/corrupt/rebuild-needed index and moving metadata are NotReady.
  A later direct success does not clear a failed/partial latest Sync; a later
  successful attributable full Sync, including no-op, does. A later partial
  direct attempt invalidates an older Done even without a generation change.
  Legacy indexes need an explicit Sync after the new producer is available.
- **FR-S2a (ADDED dependency):** S2 adoption requires A's narrow all-writer
  outcome projection, recorded under the existing ProjectLease for direct full
  index, Sync, incremental and recovery/rebuild writers. It invalidates prior
  proof before mutation and coherently publishes final status/outcome with
  strict actual counts, completeness scope, attempt identity and association
  to existing index_generation/job. Nonterminal/crash state cannot reuse stale
  success. Attempt identity attributes outcomes, not a new search generation or
  publication lease. B consumes read-only; timestamps/equal generations/post-lease
  inference are not evidence. This specific producer gate does not block S1 or
  require all CF11; park S2 and continue independent #81 if it is unavailable.
- **FR-S2a (MODIFIED observation):** Use two bounded authoritative metadata
  observations around one read-only ReadIndex health check under existing read/
  mutation admission, using fresh short metadata snapshots rather than carrying
  O1's SQLite snapshot into O2. Changes in project/status/generation/attempt/outcome/Sync
  tuple return state_changed with no retry. No readiness-triggered Repair,
  writer lease, provider, registration or hidden Sync. This remains an
  observation and preserves current mutation refusal semantics.

**SC-S2 (MODIFIED):** S1 updates the component settings inventory and state
dependency matrix in the same integrated PR after a source scan; identify
adopted/deferred subsets and runtime restart effects. S1a/b/c keep separate fresh
reviews and merge only together with all accepted fields wired. Named test
targets and exact implementation verification commands are in the amended
[server-plan.md](server-plan.md). No S runtime acceptance or real ~20k result is
claimed by this plan amendment.

## Delta 2026-11-13 — MODIFIED: FR-S1b bounded capture (#152)

S1a must close [#152](https://github.com/punkhomov/docsbase-memory-mcp/issues/152)
inside the existing S1 integration PR. Global/project config input and daemon
restart-diagnostic reads must enforce an actual byte bound before parsing or
hashing: consume/retain at most trusted budget+1 bytes and reject over-budget
input, even if the source grows after metadata inspection or has unknown length.
Do not size allocations from untrusted metadata.len. Reuse the existing bounded
configuration observation reader/cap where possible; no private daemon default.
The current trusted boundary stays MAX_STATE_BYTES=1MiB, established before
reading the file; configurable bootstrap I52 remains deferred, with no recursive
file-defined initial-read budget.

Launch parsing/resolution and the baseline diagnostic stamp use the same captured
bytes, without a second source read. Later restart checks are also bounded.
Distinguish Missing, Loaded and Unavailable: Missing becoming oversized/unreadable
must produce change/unavailability diagnostics, never silently compare equal as
None. Any current Unavailable reports a diagnostic even if its reason repeats;
unchanged Missing or equal Loaded digests may report unchanged. Preserve legacy
Shared parse-error diagnostics/fallback policy and Website fail-closed startup.
Diagnostic stamps are not CF5 authoritative identity/revalidation/publication.

Acceptance: deterministic growth and CountingRead/stream tests assert the actual
budget+1 consumption limit; misleading metadata does not drive buffer sizing;
startup parse/stamp share capture even when the file changes afterwards; and
oversized/unreadable restart checks after initial Missing produce a notice.
Use bounded fixtures/counters, not huge allocations, RSS or timing thresholds.
S1's inventory/dependency classification records the enforced current I52 slice
and runtime/diagnostic-only effect; no CF5/v2/source-write/strict-activation scope
is added. The original reviewer's scoped plan-addendum PASS precedes runtime code.
