# Search concurrency — design

Дата: 2026-11-10. Requirements: [requirements.md](requirements.md). Архитектурная
работа #66, machine mode в рамках разрешённой пользователем автономной очереди.

## Constraints and architecture

- Rust1.88/edition2024; Linux/macOS/Windows.
- No new crates, no new unsafe, no production unwrap/expect/panic.
- No ranking, golden/baseline, provider window or latency budget changes.
- Production consumes explicit resolved owner policies; no consumer defaults.

Startup: CLI resolves DbSpec + ReadPoolSpec → lifecycle validates both before
admission → Shared opens/migrates one writer → prepares readonly pool → serves.
S1 adds this resource lifecycle only. Existing search routing stays serial until
singleflight and request snapshot are implemented; no partial concurrency claim.

## S1: prepared metadata readers (FR-1/FR-2, NFR-2, SC-1)

`src/store/read_pool.rs` owns `ReadPool`, `ReadLease<'pool>`, a mutex over idle
slot indices, one Mutex<Db> per fixed slot and a Condvar. `prepare(cache, db_policy, pool_policy)` revalidates
both complete policies, reserves capacity fallibly before opening connections,
and opens exactly `max_connections` operational readonly handles using CF2's
explicit API. Partial construction drops prepared handles on failure. No open
occurs on checkout, return or normal request; no background worker is added.

`acquire(deadline: Instant) -> Result<ReadLease<'_>>` waits only until the supplied
absolute deadline. An expired deadline is rejected even if a handle is idle.
Under the state mutex: check deadline, pop idle, otherwise Condvar wait_timeout
for remaining time and recheck after every wake. SQL and wait execution do not
hold the idle-state mutex. No internal timeout/default, connection-open retry or polling.
Capacity is retained in `Resolved<ReadPoolSettings>` together with provenance.

Lease owns a slot MutexGuard and borrows its pool, so pool cannot drop before lease.
`Deref<Target=Db>` permits existing read APIs. Drop rolls back any unfinished SQL
transaction before return; rollback failure quarantines all handles and marks the pool
unusable, waking waiters with explicit admission error. It never silently creates
a replacement connection or returns a pinned WAL snapshot to the next borrower.
Field drop order unlocks the connection before a private ReturnSlot guard
advertises its index and notifies waiters; there is no second blocking checkout
after the deadline-aware queue. Mutex poisoning is recovered for bookkeeping,
not for skipping SQL cleanup. Shared prepares these resources before lifecycle
binds the listener or writes daemon state.

Shared stores the pool outside writer mutex and exposes it through `read_pool()`.
`Shared::open_with_policies` and `run_daemon_with_policies` receive both resolved
policies. Existing `open`, `open_with_settings`, `run_daemon`,
`run_daemon_with_settings` remain compatibility adapters that explicitly resolve
the omitted owner policy. Production CLI/lifecycle use only the new interfaces.
The resolver helper `standalone_read_pool()` lives next to ReadPoolSpec; numeric
defaults remain solely in the owner. TOML activation stays CF3/CF5, not S1.

## Error handling, dependencies and tests

Admission covers invalid policy, elapsed checkout deadline and unusable pool;
operational SQLite/IO errors keep their source via existing Db APIs. No wire
taxonomy change. Required: std Mutex/Condvar/RAII and existing rusqlite/CF2 Db.
Forbidden: new pool crates, per-query migration, hidden wait defaults.

`tests/read_pool.rs` uses actual WAL DB and typed overrides; gate-driven tests
verify capacity, exclusive handles, waiting/return and independent SQLite reads.
`catch_unwind` verifies lease return during caller unwind. A read transaction
pins an old row while a separate writer commits, then lease drop releases the
snapshot so the next borrower sees the new row. Deadline tests use already
expired input and controlled lease release, no performance thresholds.
Shared/lifecycle tests verify explicit policy propagation and no filesystem
effect for imported zero-capacity policy. Full cli-full/lean targeted, Clippy,
golden/offline and native required CI are delivery gates.

## Alternatives and risks

Lazy per-query opens avoid eager fd cost but violate the prepared-resource goal
and move schema checks into request latency; rejected. One lock around all readers
keeps serialization; rejected. Eager pool costs four operational handles at
startup by default and can make startup fail if readers cannot be prepared; this
is deliberate fail-before-serving behavior. Administratively supplied large
capacities may exhaust OS resources and fail startup; no guessed extra cap.
Do not use a lease through provider/long CPU: S3 must enforce the short-phase
ownership boundary, and S4 must supply a real request deadline/cancellation.

## Later design gates

S2: query/dense singleflight with failure/unwind/replacement tests before parallel
admission. S3: coherent short SQL phases and owned candidate/citation payload,
including explicit sync/rebuild consistency choice. S4: cancellation and queue
telemetry at actual async dispatch boundary, same-project integration and native
acceptance. These tasks require dated concrete interface deltas before code;
S1 alone is not feature convergence or issue closure.

## Delta 2026-11-10 — S2a query embedding singleflight

S2 is split into S2a (query embeddings/cancellation primitive) and S2b (cold dense
loads). This delta settles S2a before code; daemon search remains serial until S3.
FR-5/SC-3 apply to identical query work, FR-6 applies to explicit waiter cancellation.
Request deadlines/dispatch cancellation remain S4, cold rows remain S2b.

- `hybrid::work::Cancellation` is a cloneable request-state token, initially
  active; `cancel` is permanent and wakes subscribed waiters. It has no timing
  budget/default. Request timeout ownership and deadline-aware dispatch remain S4.
- Private `hybrid::flight::Flights<K,T:Clone>` owns only active flights. Key
  admission and cache recheck are short mutex phases; work runs outside all
  registry/cache locks. Leader and waiters share one result without cloning
  vectors. There is no persistent error cache or retired-key history.
- Each participant owns an Arc wake node with a one-slot coalescing notification
  channel. Completion and cancellation publish state before nonblocking wake;
  waiters recheck both states after wake. Channel capacity1 is the invariant
  "one pending wake", not a tunable work/memory budget. Weak subscriptions are
  removed on drop; cancel/remove cannot leave an accumulating waiter list.
- A leader RAII guard publishes failure and removes its matching flight during
  unwind. Provider panic keeps unwinding for its caller; other waiters receive an
  explicit failure and a later call can retry. Normal failures preserve category,
  message, instruction and source chain via a shared original Error wrapper.
- Cancelling one participant does not cancel remaining participants. Before
  starting shared work, the flight checks that at least one participant is active.
  Already running synchronous provider work may finish and populate the pure
  query cache for live waiters; a cancelled caller receives cancellation and
  starts no following phase through the controlled API. No provider interrupt
  or absolute end-to-end deadline is claimed by S2a.
- `DenseCache::query_embedding(embedder, text, &Cancellation)` checks the
  completed LRU, then joins/creates flight `(model_id, blake3(text))`. Recheck the
  completed cache while admitting a leader so a delayed miss cannot duplicate a
  just-finished call. Only the leader calls `blocking::enter(embed_query)` and
  stores the vector. Existing cache limits, keys, hit/miss lookup semantics and
  eviction remain unchanged; all coalesced misses still count as lookup misses.
- Query flights are shared alongside the query cache by `sharing_query_cache`
  and snapshots; cold row flights are separate S2b work. Model identity is the
  existing stable recipe/width/normalization identity contract; generation does
  not alter a pure query embedding. Model replacement uses a distinct key.
- `tools::dense_candidates` adopts this API with a fresh operation token under
  its existing legacy search entry point. S4 will wire actual request cancellation
  into the controlled pipeline. There is no new config timeout selected here.
- `blocking::enter` becomes RAII so panic cannot leave a reused worker thread
  permanently marked as sanctioned. Test the existing defect RED before fixing.

Tests use gated fake Embedders and instance-local flight join observers (test-only)
to prove four simultaneous misses are already admitted before releasing one
provider call. Verify identical Arc results, errors/retry, cancelled waiter/leader,
unwind wake/retry, distinct models, completed-cache sharing and no retired flights.
No new crates/unsafe, provider/window/ranking/baseline change or CI sleep/SLA.
Cold generation/config publication authorization is not claimed here; S2b/S3
must settle it before daemon concurrency. Active flight cardinality follows
active callers, not a new persistent cache; S4 retains daemon admission ownership
through detached blocking work before parallel dispatch is enabled.

## Delta 2026-11-10 — S2a I1: отмена перед provider admission

Терминальное состояние `Completion::Abandoned` отделено от результата provider:
если leader до начала work не видит активных участников, flight снимается с
registry без provider call. Live caller, уже выбравший этот flight или
зарегистрировавшийся до его удаления, повторяет admission, проверку своей отмены
и completed-cache recheck. Поэтому отмена прежних участников не превращается в
ошибку нового live caller; настоящий `Error::Admission` от provider остаётся
обычным общим результатом и автоматически не повторяется.

Внутренний `run` заимствует key, cache callback допускает повторный вызов
(`FnMut`), expensive work остаётся `FnOnce`. Регистрация wake после удаления
flight безопасна: caller читает опубликованный `Abandoned` без ожидания
пропущенного notification.
Повторное admission не держит старый membership; registry/cache/notification
mutex не удерживаются через provider. Детерминированные instance-local gates
проверяют admission после no-active decision, выбор до регистрации, регистрацию
после retirement, cache recheck и отсутствие вызова при полной отмене.

## Delta 2026-11-10 — S2b cold dense initialization

This settles S2b before code (FR-5 cold subset, FR-3 metadata-resource foundation).
Daemon dispatch and cross-store/config-revision guarantees remain S3/S4. It
supersedes the earlier S2b sentence that left both publication authorities in one
step: generation ordering is S2b; current config/model publication authority is
S3 before enabling parallel daemon search. No new capacity/default is introduced.

### Owned metadata and lazy admission

- New `hybrid::dense_metadata::DenseMetadata` has private root/project/generation
  and owned `(chunk_id, content_hash)` rows. `capture(&Db, project_id)` reads
  generation and hashes in one SQL read snapshot, then ends the transaction
  before returning. If the caller already owns a transaction, reuse that snapshot
  without committing/rolling back it; its lifetime remains caller responsibility.
  The daemon pool adapter in S3 must own/drop its lease wholly inside this loader.
- `DenseRequest<'a>` contains explicit cache_root, project_id, generation and
  model_id; it has no default or config lookup.
- `DenseCache::get_with_metadata(request, &Cancellation, loader)` and
  `warm_with_metadata(...)` accept a one-shot `FnOnce()->Result<DenseMetadata>`
  loader. They never open a metadata connection. Complete cached state returns
  without invoking the loader. On a miss, shared Flights admission occurs BEFORE
  full hash copies or vector IO/materialization. The loader's identity must match
  every request identity before any vector/ANN IO; mismatch is explicit retryable
  Project/Admission error, not a result labelled with an obsolete generation.
- `get(&Db,...)` / `warm(&Db,...)` remain adapters: cheap generation read, explicit
  request/token, lazy DenseMetadata capture. They no longer retain a transaction
  through cold IO/wait. Their externally borrowed Db can still live as before;
  S3 production uses the lazy pool adapter, not a held pool lease.
- A DenseCache serves one cache root: bind its explicit ANN-store root when
  available, otherwise its first explicitly supplied metadata root. Reject a
  different root before cache reuse. Snapshots share that identity and cold-flight
  registry; sharing_query_cache creates fresh row/cold state while retaining its
  existing ANN-store scope and shared pure query cache. This is instance identity,
  not a fallback path/default setting.

### Flights, refresh and publication

Cold flight key is `(root, project, generation, model)`. Getter/warm share active
work. Getter accepts current Ready/Ann/TooLarge/Building or non-expired Failed.
Warm keeps its existing stronger refresh condition: Ready/TooLarge must reload
coverage; Ann/Building and non-expired Failed are already warm. Recheck these
conditions during flight admission. Different generation/model work stays
separate. Use S2a cancellation/abandonment/error/unwind cleanup unchanged.

Refactor resolve to consume owned metadata/root, never `&Db`, during ANN/vector
load and CPU work. Do not change vector-cache policy, ANN selection, fallback
ranking, retry backoff, resident/build accounting or existing live-slot guards.
Generation ordering is checked under the entries mutex: an older completion
never replaces a newer entry, and an obsolete build never installs Building or
starts a background worker. The old caller may still receive its own coherent
state; existing byte/reservation/disabled-cache refusal takes priority and keeps
its streaming fallback. No return value may be relabelled as the newer generation.

Config replacement at the same generation is a declared S3 authority boundary.
S2b keys isolate different models and return their own data, but it does not
claim that last-finishing same-generation model work is the active config.
General executor/admission budgets and total process/RSS accounting remain
#79/#78; coalescing identical cold work is not an unlimited-concurrency guarantee.

### Verification

Use real SQLite/VectorCache and per-instance gate/join observers: four cold
callers (including shared cache snapshots) call one metadata loader and receive
one shared row state. While loader waits, the independent writer/control reader
remains usable. Force metadata-generation change between generation/hash reads
and prove one SQLite snapshot; validate wrong root/project/generation before
vector artifact creation. Error/cancel/unwind wake/remove/retry cases reuse the
S2a lifecycle. Verify warm rechecks Ready coverage and stays idempotent for
Building/Ann. Pause old generation work, install newer state, then finish old:
new cache slot remains; no obsolete background build. Preserve #69/#73 tests,
golden/offline/replay and full/lean/native gates. No sleep/performance SLA.

## Delta 2026-11-10 — S3.1: общая read-аренда проекта

S3 состоит из нескольких локальных задач с отдельным review перед одним
delivery PR. S3.1 даёт основу FR-4: file lease и обнаружение незавершённой
публикации. Поисковый pipeline, ownership provider-фаз и dispatcher пока прежние;
готового согласованного ответа и параллельного поиска этот шаг не обещает.

- `lifecycle::with_project_read_lease(cache, project_id, work) -> Result<T>`
  открывает тот же `projects/<id>/.writer.lock`, что production full index и
  watcher. fd_lock shared guard существует только во время `work`. Несколько
  readers совместимы; writer вызывает немедленный Project error с retry hint.
  Нет очереди, sleep или скрытого timeout. Guard/file освобождаются при success,
  error и unwind. Provider нельзя вызывать внутри `work`.
- Writer lease и его прежняя классификация конфликтов сохраняются. Проверка
  использует настоящие OS locks с отдельными handles, включая один процесс;
  daemon-only mutex не заменяет межпроцессную координацию.
- `repo::has_pending_docs(connection, project_id) -> Result<bool>` использует
  SQL EXISTS для docs этого проекта с пустым content_hash. Не открывает
  connection/transaction. Отсутствующий проект не считается проиндексированным:
  будущая preparation сначала проверяет project/status, затем этот predicate
  и существующие ReadIndex schema/rebuild gates под общей read-арендой.
- Marker нужен после crash, освобождающего writer lock: production выполняет
  marker SQL -> provider -> Tantivy -> final SQL. SQLite snapshot не делает
  эти commits атомарными. S3.1 лишь предоставляет predicate; применение будет
  в owned preparation. Произвольный writer, обходящий lease, вне cooperative
  protocol; низкоуровневые public index jobs требуют координации от caller,
  как уже делают production wrappers.

Следующие S3-задачи до реализации фиксируют byte limits owned candidate text,
citations/filter metadata, config/stages authority и warm/search adoption.
S3.1 не выбирает числовую payload policy. S4 сохраняет ответственность за
dispatcher cancellation/deadline/timing и включение параллелизма.

## Delta 2026-11-10 — S3.2: owner policy и byte admission для payload

Перед owned capture вводится отдельный чистый owner в `search::settings`:
`SearchSettings`, `SearchPatch`, `SearchSpec: SettingsSpec`. Поля типа u64:
`max_request_payload_bytes` и `max_retained_payload_bytes`, scope Daemon,
unit Bytes. Оба положительны, первый не больше второго. Owner defaults:
64MiB на один payload, 256MiB на удерживаемые payload одного budget instance.
64MiB оставляет место для metadata при обычном window50 и default max_file_size
1MiB, включая atomic fences; 256MiB — самостоятельная policy, не формула от
capacity read pool. Это настраиваемые defaults, не fixed invariants или RSS SLA.

`Config.search: SearchSettings` всегда полон. ConfigSpec один раз получает
SearchSpec defaults при resolution, агрегирует owner field descriptors и
делегирует overlay/validation. Только global `config.toml` принимает `[search]`
с двумя optional patch fields; project table и typed project override
отклоняются до runtime. Неизвестные поля/неверные числа отклоняются. Whole-value
v1 hybrid merge и legacy JSON `docsbase config` не меняются: этот view пока
показывает прежние поля; versioned effective view/provenance UI остаётся #78.
Typed resolution сохраняет реальные provenance двух новых fields уже сейчас.
Global policy меняется через существующий daemon restart; artifacts/vector
identity и project invalidation от этих budgets не зависят.

`search::budget::PayloadBudget::new(SearchSettings) -> Result<Self>` принимает
полные settings, повторно валидирует imported value и не читает TOML/env/
defaults. Clone разделяет один aggregate counter. `reserve(bytes)` проверяет
request cap, checked sum и aggregate cap, возвращая private-field RAII
`PayloadPermit`; ошибка — Admission без ожидания и без изменения counter.
Permit не Clone/Copy, освобождается при drop/error/unwind, включая другой thread.
Callbacks и providers не выполняются под mutex counter; ноль bytes допустим
для пустого payload. Гарантия — charged owned bytes, не весь RSS/allocator,
protocol buffers, ANN working sets, serde temporaries или executor (#79/#78).

S3.2 не публикуется отдельно: до whole-S3 PR обязательны реальные consumers
для SQL preflight/capture и production search/warm phase ownership. Нельзя
выпустить принятые, но не применяемые настройки. S3.3 до реализации фиксирует
состав/учёт payload, SQL snapshot/preflight и отсутствие provider text truncation.

## Delta 2026-11-10 — S3.3: owned SQL payload

Новый `search::payload` предоставляет `PayloadRequest<'a>` с обязательными
cache_root/project_id/generation/chunk_ids/include_text/include_filter_metadata
и `OwnedPayload::capture(&Db, request, &PayloadBudget, &Cancellation)`.
Нет открытия metadata connection или consumer defaults. Caller отдельно
проверяет indexed project/ReadIndex schema и удерживает cooperative project
read lease вокруг candidate generation + capture. Сам capture даёт согласованный
SQL payload, не атомарность с произвольным внешним writer/Tantivy snapshot.

До копирования данных: cancellation/root identity, bounded unique chunk_ids
(существующий MAX_HITS), затем generation и отсутствие pending markers в одном
коротком read transaction. Собственный transaction заканчивается до возврата;
caller-owned transaction используется без commit/rollback и остаётся его
ответственностью. Несовпадение generation/pending — явный retry/rebuild Project
error; wrong root — Admission; недопустимый internal candidate set — отказ.
Все row queries ограничены project_id, включая ошибочно чужие chunk ids.

Preflight и copy относятся к тому же SQL snapshot. До reserve читаются только
length/identity scalars, без Rust копий больших текстовых полей. Использовать
`octet_length(column)` непосредственно: bundled SQLite3.53.2 поддерживает
OPFLAG_BYTELENARG без загрузки текста. CAST вокруг column убирает эту оптимизацию.
PRAGMA encoding читается без изменения: UTF-8 byte length точен, для UTF-16
консервативная UTF-8 граница — checked `3 * ceil(bytes / 2)`. Не считать
валидную UTF-16 БД corrupt и не вводить молчаливый UTF-8 fallback.

Charge включает encoded root bytes, UTF-8 upper bounds всех копируемых strings
и fixed record bookkeeping через std::mem::size_of: общий owned struct, ids,
id->position entries, citation entries, text String slots при include_text,
distinct doc-meta entries при include_filter_metadata. Это логическая верхняя
оценка payload data/records, не allocator/hash-table control bytes, SQL work
buffers, protocol output buffers или полный RSS. Все суммы/произведения checked;
overflow — Admission до allocation. Временные SQL parameters/id sets ограничены
MAX_HITS и прежним BATCH_KEYS; новых числовых batch defaults нет.

Private data: root/project/generation, исходный порядок ids и positions,
citations keyed chunk_id, optional Vec<String> texts в исходном порядке,
doc_meta keyed distinct doc_id. Полный текст копируется только для rerank/snippet,
без truncation provider documents. Missing citation/text остаётся отсутствующим
для snippet/rendering; `rerank_documents()` требует text mode и полноту всех
исходных ids, сохраняя прежнюю ошибку missing rerank text. Getters citation,
chunk_text, doc_meta, root/project/generation/charged_bytes работают без SQL.
Missing doc/filter semantics #62 и порядок ranking/post-filter здесь не меняются.

`OwnedPayload` не содержит SQL/index/file handles, Send+Sync+'static. Его data
поле объявлено раньше финального private PayloadPermit: сначала освобождаются
данные, затем charge. При construction permit local объявляется до data local,
чтобы error/unwind имели тот же порядок. Cancellation проверяется перед фазами
и batches; query/transaction/permit cleanup обязателен при отказе/error/unwind.
Instance-local test hooks разрешены только для детерминированного interleaving/
drop ordering, без process-global flags или production callback API.

S3.3 не активирует dispatcher и не завершает #66: config revision authority,
actual pipeline/pool consumers и S4 по-прежнему обязательны до delivery.
