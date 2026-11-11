# Search concurrency — requirements

Дата: 2026-11-10. Источник: #66 и дополнение 2026-11-08; prerequisite CF2 (#105).
Этот spec реализуется по частям; завершение одной части не закрывает #66.

## Problem, goals and actors

Медленный hybrid search удерживает daemon-wide `Mutex<Db>` через provider calls.
Агент в другой сессии, в том числе того же проекта, не может выполнить даже
lexical search. Цель — конкурентные чтения с согласованной выдачей и ограниченными
ресурсами. Потребители: daemon/MCP/CLI, будущий HTTP adapter (#79).

## Prioritized stories

- P1: как агент, хочу получить status/list/lexical, пока другой search ждёт
  provider. Given gated reranker, When независимый запрос приходит из любой
  сессии, Then он завершается до открытия gate. Это основная пользовательская
  причина #66; fake providers и настоящий dispatcher проверяют её без сети.
- P1: как владелец, хочу одну оплату одинакового embedding miss. Given четыре
  одновременных запроса, Then один provider call; ошибка будит всех waiters,
  следующий запрос может повторить попытку. Отмена одного waiter не отменяет других.
- P1: как читатель, хочу citation/snippet того же index state, что candidate.
  Given sync/reload во время provider call, Then результат содержит целиком
  старый или целиком новый payload, либо явный retryable отказ до выдачи.
- P2: как оператор, хочу видеть очередь в explain и понимать границы отмены.
  Given request waiting for admission, Then end-to-end timing включает ожидание;
  отменённый caller не запускает следующие дорогие стадии.

## Functional requirements

- FR-1 (must): daemon готовит ограниченный pool operational read-only Db после
  единственной startup writer/migration фазы. Query не вызывает migrating open.
  Размер берётся из `Resolved<ReadPoolSettings>`, DB policy передаётся явно;
  malformed imported settings отвергаются до filesystem effects.
- FR-2 (must): handle принадлежит одному borrower и возвращается при success,
  error и unwind. SQL transaction не переживает возврат. Pool не задаёт скрытый
  timeout: acquire принимает абсолютный deadline caller; истечение — явный отказ.
- FR-3 (must): SQL выполняется короткими фазами; provider, длительный CPU и
  singleflight wait не удерживают global Db mutex/SQL transaction/pool handle.
- FR-4 (must): search фиксирует config/stages revision и согласованные index,
  generation, metadata/text/vector identities. Границы sync/rebuild описаны явно;
  независимые commits Tantivy/SQLite не объявляются атомарными.
- FR-5 (must): identical query embedding misses и cold dense initialization
  coalesced. Error/unwind/cancel/replacement не оставляют вечных waiters и не
  публикуют obsolete result поверх нового state. Не создавать большие duplicate
  vector copies до flight admission.
- FR-6 (must): caller cancellation проверяется между фазами. Уже работающий sync
  provider не объявляется прерываемым; shared work живёт для остальных waiters.
  Сохраняются bounded transport retries, shutdown accounting и single-writer.
- FR-7 (must): end-to-end telemetry включает queue delay; существующие wire names
  и default array payload сохранены. Новые диагностические поля аддитивны.

## Non-functional requirements and data model

- NFR-1: Rust1.88/edition2024, Linux/macOS/Windows, без новых crates/unsafe и сети
  по умолчанию. Golden/baseline, ranking, provider windows и budgets неизменны.
- NFR-2: pool capacity — верхняя граница подготовленных metadata connections;
  ожидание не держит pool mutex. Default4 принадлежит ReadPoolSpec, не consumer.
- NFR-3: регрессии используют channels/gates; watchdog — страховка deadlock,
  не абсолютный performance SLA для shared CI.
- `ReadPool` владеет подготовленными Db; `ReadLease` временно владеет одним Db.
  Immutable policies/provenance живут не меньше pool. Request context и flights
  будут добавлены отдельными task deltas до включения parallel search.

## Interfaces and success criteria

Конфиг/CLI/IPC форматы сохраняются; `store`/`read_pool` TOML пока отклоняются.
Typed overrides доступны composition root. Совместимые constructors остаются
явными adapters, production передаёт обе resolved policies.

- SC-1: capacity1/2, deadline expiry, return-on-error/unwind, transaction cleanup,
  readonly enforcement и custom timeout проверены настоящим SQLite (S1).
- SC-2: gated provider не блокирует same-project lexical/status/list (S3/S4).
- SC-3: четыре identical misses → один call; error/retry/cancel/replacement
  и cold dense coalescing проверены детерминированно (S2/S4).
- SC-4: sync/reload при paused search не смешивают candidate/snippet identities
  (S3/S4); full suite/golden/offline и четыре required checks проходят.
- SC-5: queue timing и cooperative cancellation проверены без новых CI sleeps (S4).

## Challenges, boundaries and open questions

| Challenge | Resolution |
| --- | --- |
| Async mutex решит блокировку? | Нет: нужно изменить область владения ресурсами (FR-3). |
| Можно открывать Db на каждый query? | Нет WAL/migration path; startup pool (FR-1). |
| Pool сам выберет wait5s? | Нет: абсолютный caller deadline (FR-2), как требует CF0. |
| Четыре читателя автоматически дают snapshot? | Нет; FR-4 нужен отдельно до parallel search. |
| Снять serialization до singleflight? | Нет: S2 предшествует S3, чтобы не умножать costs. |
| Большая replica данных на каждый запрос? | Нет; bounded candidate payload и flight до vector materialization; масштаб проверяется при S3 design delta. |
| Отмена spawn_blocking прервёт provider? | Нет; cooperative boundary и честная документация FR-6. |

Вне scope: whole-async rewrite, remote async/absolute provider deadline (#81),
общесерверные budgets/Node connector (#79), v2 configuration/planner/immutable
published generations (#78). S1 только готовит pool, старый dispatcher пока
сохранён. Открытых вопросов для S1 нет; detailed S2–S4 interfaces фиксируются
датированным design/task delta после проверки действующего кода.

## Delta 2026-11-10 — S4.3 scoped transport and queue diagnostics

FR-6/FR-7/SC-5: protocol1 gains an additive capability-advertised search request
with explicit remaining/elapsed/queue milliseconds. Clients must discover its
versioned capability before using it; missing capability means actionable
restart/refusal, not an unchecked fallback. Package version alone cannot prove
capability because development daemon/frontend builds share the same build_id.
Legacy CallTool and its write-half-close/reply behavior remain supported within
the configured request budget. Scoped search opts into one outstanding call
and EOF cancellation; no multiplexing/correlation extension is promised.

One original request budget includes queueing and reconnect, and the server
clamps received remaining time to its own complete policy and original elapsed
time. Expired/malformed context is refused before paid work. Active scoped EOF,
session cancellation or deadline cancels only that caller; actual job/reply/
connection charges retain S4.2b semantics. A blocked reply write receives no fresh
budget; closure may prevent a complete error frame after expiry. Synchronous
providers are still not forcibly preempted. Frontend/CLI propagation, bounded
retry and MCP cancellation integration are S4.3b before whole-feature delivery.

Explain gains additive queue_ms for admission waiting, while stage_ms.total
retains original elapsed time and default array responses stay unchanged.
Queue accumulation starts at structural zero, not a consumer setting default.
Existing public metadata constructors/serialization helpers retain their
signatures; producers use an explicit queue-aware envelope adapter.

## Delta 2026-11-10 — S4.3b: canonical caller lifetime

FR-6/FR-7/SC-5: canonical MCP and CLI socket search use asynchronous IPC on all
three platforms. One complete owner search policy creates the original operation
before daemon startup, connection admission or reconnect. Connect, handshake,
serialized connection waiting, search exchange and reply decoding consume that
same budget. Client cancellation/expiry drops or poisons its owned connection;
a cancelled queued caller must not close another caller's active connection.

Search requires the advertised search-context-v1 capability. A same-version
legacy daemon without it yields actionable restart Admission before Search is
sent; no silent CallTool search fallback. The old blocking Client remains an
explicit compatibility API. Generic non-search tools keep their existing tool
budgets; no new consumer default or provider feature reduction is introduced.

MCP request cancellation uses the actual rmcp per-request cancellation signal.
A transport failure may reconnect/retry at most once while the original budget
is live. Expired/cancelled requests cannot replay paid work. CLI falls back to
direct mode only when its initial connection cannot be established; connected
handshake/call failures propagate. Direct CLI retains its original ingress time.
Explain total is finalized using the canonical caller's original monotonic
clock after receipt; returned queue is not double-counted. Default arrays stay.

## Delta 2026-11-11 — Windows busy endpoint admission

FR-5/FR-6 and SC-4 also cover connection admission: a busy named pipe must not
leave an unbounded dependency blocking worker after cancellation, or make the
CLI runtime teardown wait indefinitely. Windows async connect makes one
nonblocking OS attempt and returns a transport error when all instances are
busy. Liveness probes must return present for a busy endpoint and false for
an absent endpoint without waiting for an instance. Preserve available-pipe
byte-stream/protocol behavior and generic legacy blocking-client compatibility.

### Delta 2026-11-11 — non-consuming liveness

A Windows liveness probe must leave an available named-pipe instance available
for the actual client. It does not reserve capacity against concurrent clients,
but must not itself create a client connection. Reject embedded NUL rather than
querying a live truncated prefix. Busy means present, missing means absent.

## Delta 2026-11-11 — MODIFIED NFR-1: non-consuming Windows probe

MODIFIED NFR-1: permit exactly one new `unsafe` FFI call,
`WaitNamedPipeW(..., NMPWAIT_NOWAIT)`, only in
`src/platform/windows.rs::connect_probe` for non-consuming liveness. Reject
embedded NUL before FFI; pass a NUL-terminated UTF-16 buffer that remains live
for the entire call. Require a function-scoped `#[allow(unsafe_code)]`, a
`SAFETY:` comment and real native regression coverage for non-consumption and
embedded-NUL rejection against a live prefix. This records the
[existing S4.3c design ruling](design.md#delta-2026-11-11--liveness-must-not-consume-a-pipe-instance)
without broadening its implementation safety approval.

All other NFR-1 restrictions remain unchanged: Rust1.88/edition2024 and
Linux/macOS/Windows support, no new crates or other new unsafe, no network by
default, and unchanged golden/baseline, ranking, provider windows and budgets.
