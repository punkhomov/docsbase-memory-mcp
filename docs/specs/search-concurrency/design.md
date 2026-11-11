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

## Delta 2026-11-10 — S3.4: owned post-candidate pipeline

FR-3/FR-4 foundation: rerank, post-filter и rendering получают один
`OwnedPayload`; после capture эти функции не принимают Db/Connection и не
дочитывают generation. `PreparedSearch` владеет parsed args, candidate order,
active stages/timings и payload; `finish(&HybridStages, &Cancellation)` потребляет
его без SQL/index/file handles. Исходный candidate order задаёт provider documents
и top_n, затем сохраняются прежние rerank/filter/dedupe/truncate/snippet rules.
Explain generation берётся из payload. Bare-array wire format не меняется.

Production boundary `SearchContext` содержит stages/dense/cache/project/window,
явные PayloadBudget и Cancellation. `search_docs_in(&SearchContext,&Db,&Value)`
готовит owned результат и вызывает finish. Shared владеет одним budget из
полного global_config.search; direct CLI создаёт budget из полного config.search.
Ни один consumer не подставляет отсутствующие поля. Старый search_docs_with —
явный standalone compatibility adapter: owner SearchSpec разрешается через
pure resolver на boundary, один budget на standalone invocation, не daemon.
Новый settings::standalone не читает environment/filesystem.

Cancellation проверяется до работы, перед capture/provider и после provider
перед дальнейшими стадиями. Ошибка/отмена/unwind освобождает payload/permit;
provider получает borrowed полные texts без второй Vec<String> copy. Маленькие
ranking/response buffers остаются вне logical payload charge, как в S3.3.
Limit0 сохраняет прежнюю раннюю empty response и не требует payload admission.

Challenge: внешняя Db reference у caller ещё существует — это промежуточный
этап, а не заявление о FR-3 dispatcher completion. До production delivery S3.5
обязан перевести capture на short prepared reader phases, добавить config
revision authority и удерживать cooperative read barrier вокруг cross-store
candidate/capture, но не provider/flight waits. S4 остаётся обязательным.
Remaining S3 и S4 поставляются одной reviewed PR: это исключает временный
consumer pool timeout и публичные настройки без полной production semantics.
Синхронные provider, ranking/window/baseline budgets не меняются.

### S3.4 validation ruling — readiness while watcher writes

Full suite exposed config_runtime::watcher_uses_reopened_config calling
search.expect during a real pending SQL update. S3.3 deliberately rejects
incomplete capture. The runtime integration helper now waits for a successful
search response while Project state is transient, within the existing watcher
watchdog. Admission/Protocol/Internal/Transport errors remain immediate failures;
a persistent Project error fails the watchdog with its last diagnostic. No
refusal maps to an empty result, so negative filter assertions still require a
real successful response. No product retry or deadline change. Add explicit IPC
pending-marker refusal/recovery coverage; real watcher test retains its content
and reopened-ignore assertions. This is a contract correction, not a test removal.

S3.4 same-shape audit: mcp_registry has three rebuild-only polling guards,
admission_ux one, and nightly soak accepts only indexing status. All move to a
small test-only search_visibility predicate: Project status/rebuild/pending
states only; every other error stays fatal. config_runtime uses the same narrow
predicate, retaining the last refusal on timeout. Successful content assertions
are unchanged, with no extra test deadline or production retries. Real IPC
pending refusal is also checked against this predicate before actual recovery.

## Delta 2026-11-10 — S3.5: project revision и preparation authority

FR-3/FR-4/FR-5 foundation. Stable `ProjectSlot = Arc<ProjectCell>` содержит
`state: Mutex<ProjectState>`, `reload: Mutex<()>`, `preparation: RwLock<()>`.
Map mutex нужен только для lookup; cell не заменяется при reopen. Config Arc
остаётся revision identity, stages принадлежат ему; прежняя защита #76 от
obsolete lazy build result/error сохраняется.

Reload serializer покрывает полный per-project reload. Parse и provider/model
construction выполняются без state/preparation locks. После двух fallible
подготовок reload получает preparation WRITE **до** state mutex, затем публикует
config/stages и vector policy, обновляя watcher прежним способом. Отказ/unwind
при parse/build сохраняет последнюю valid пару и policies. Семантически равный
config по-прежнему создаёт свежие stages/Config Arc; resident cache не очищается.
Другие проекты и чтение старой пары доступны во время model construction.

`ProjectCell::with_preparation(&Arc<Config>, FnOnce()->Result<T>)` берёт READ,
кратко проверяет текущую Config Arc под state mutex и отпускает state до work.
Stale revision даёт Project/retry до work. RAII освобождает gate при error/unwind.
Порядок locks: reload -> preparation -> state; state-only reads/lazy publication
не берут preparation, пока держат state. Active preparation задерживает новую
публикацию; уже owned response/rerank может завершиться после reload. Это ещё
не actual search adoption: она обязательна в следующем request-phase task.

`DenseCache::invalidate_memory(project_id)` (crate-private) очищает только
resident entry/digest. Pure query cache/flights, last-query outcomes и persisted
recipe-qualified ANN остаются. Existing delete-oriented invalidate делегирует
memory cleanup перед прежним disk removal, сохраняя его эффекты. При semantic
Config change composition root вызывает memory cleanup под preparation WRITE.
Это явная консервативная политика до dependency/impact planner #78; persisted
rebuild/consent здесь не вводится. Active ANN worker сохраняет charge до actual
completion; existing Building Arc identity запрещает obsolete publication.

Background ANN refresh проверяет захваченную revision под тем же READ перед
warm. Scheduled old warm после reload не заполняет cleared state; cleanup
сохраняется при skipped/stale/failed warm. Настройки readonly Db передаются
прежним explicit способом; pool/lazy-loader adoption остаётся следующему task.
Для delayed scheduling test разрешён private instance-local before-admission
hook, аналогично существующим private seams; публичного callback/settings нет.

Challenge: прежний #76 test утверждал state mutex contention при model build.
Это был механизм сериализации, не пользовательский контракт. Assertion явно
переносится на reload mutex; state/preparation должны быть свободны. Полный
same-project reload order, atomic pair/failure semantics и independence других
проектов остаются обязательными. Никаких provider/SQL lifetime completion или
завершения #66 этим шагом не объявляется; whole-S3/S4/native gates сохраняются.

## Delta 2026-11-10 — S3.6a: owner-resolved request deadline

FR-2/FR-6/FR-7 foundation, before pooled search adoption. SearchSpec adds
`search.request_timeout_ms`: daemon scope, Milliseconds, positive u64, global-only
TOML/typed patch/provenance. Default30000 preserves the existing MCP quick-call
budget for search; the value is declared only in this owner, never in a consumer.
There is no arbitrary universal upper cap; conversion to a platform Instant uses
checked arithmetic and refuses an unrepresentable deadline explicitly.

`search::operation::SearchOperation` consumes complete SearchSettings, a caller-
supplied original start Instant and existing Cancellation. It captures exactly
one deadline. Clones retain start/deadline and share only their cancellation
token; another independently constructed request is unaffected. Methods expose
original start/deadline, elapsed time, remaining budget and cooperative check.
Checks at/after deadline return Admission and cancel this request token so any
existing flight subscribers can wake. Explicit cancellation also refuses work.
Construction validates the full imported policy without IO/default resolution;
checks never reset the budget on a stage boundary, queue or reconnect.

Challenge: a synchronous waiter/provider is not preempted by a timestamp. This
task adds only the policy/context primitive; S4 must wire a cancellation timer
and transport lifetime, and S3.6b must pass the same deadline to pool acquisition.
A shared cold-flight factory must not inherit the leader's personal deadline or
abort live peers; its admission policy is specified before production adoption.
No transport timeout is changed in this primitive task, no parallel dispatch is
claimed, and all remaining S3/S4 still ship in one completed PR. A draft may run
native CI preflight while implementation continues; it stays draft until whole-
feature review and final-head required checks pass. Legacy config JSON remains
unchanged until #78's versioned effective view. Product docs before delivery
will explain restart semantics and the boundary of sync-provider cancellation.

## Delta 2026-11-10 — S3.6b: actual pooled search phases

FR-2/FR-3/FR-4/FR-5/FR-6/FR-7, SC-2/SC-4. SearchContext owns one explicit
SearchOperation instead of a bare token; the compatibility search_docs_with
adapter constructs it from the existing pure owner resolution. Production CLI
passes Config.search and its original invocation start. Daemon constructs the
operation before spawn_blocking and routes search before taking Shared.db.
Binding reads use a short prepared lease; project-runtime construction has no
SQL/global Db/state lock. Other tool routes retain their current behavior.

`search::metadata::MetadataSource::{Borrowed,Prepared}` identifies the exact
cache root and yields a short MetadataReader. ReadPool records its supplied root
and adds try_acquire: immediately borrow one prepared slot or return Admission,
without waiting/opening/migration/default timeout. Existing acquire uses the
original absolute operation deadline. Both share rollback/quarantine/return
semantics and never advertise a locked slot. Imported policy validation and
capacity remain unchanged. Borrowed compatibility handles stay caller-owned;
the higher-level search adapter refuses an already active caller transaction
before index/provider work, because its old SQL snapshot could disagree with
new Tantivy commits. Lower-level DenseMetadata/OwnedPayload retain their
existing caller-transaction contract.

Search_docs_from(context, source, args, admission:Fn()->Result<Guard>) executes:
1. Validate explicit identities/project/args/plan. Limit0 keeps the old array/
   explain shape and no payload reservation. Acquire a short reader, config
   preparation READ and cooperative OS project READ; read current project
   status/schema, generation G and pending marker. Release SQL before opening
   and searching Tantivy. Return owned BM25 hits+G and release index/file/config
   guards. Invalid lexical query fails before any paid embedding, as before.
2. Query embedding with only immutable stages and request cancellation. No SQL
   lease/transaction/global Db/config preparation/file guard survives this call.
3. Dense lookup uses the original G. Only a cold-flight leader lazily captures
   owned DenseMetadata through try_acquire and a short OS read lease, checking
   current status/pending/G. No leader's personal deadline/token is captured in
   this shared factory: live followers must survive leader cancellation. Drop
   SQL/file resources before vector IO/CPU. ANN fallback also captures owned
   hashes before streaming, whose API needs a root rather than Db.
4. Fusion/truncation preserve current order/window. Under a new short reader,
   current config preparation and OS read lease, check status and capture the
   bounded OwnedPayload at expected original G. Changed G/config or pending
   state returns explicit Project retry before response; no automatic replay.
5. Release every preparation resource before rerank/filter/render. PreparedSearch
   owns the original operation and checks it before/after providers and before
   later phases. Explain total_ms begins at original request start, including
   worker/pool queue; existing stage names/default array/provider ordering remain.

ProjectCell exposes a checked preparation lease; with_preparation delegates to
it. DenseCache adds guarded get/warm adapters accepting admission Fn()->Guard.
Initial hot lookup has a short guard; actual flight leader retains its guard
through resolve/publication; followers wait without it. Validate after flight
return outside the flight registry. NEVER take a blocking preparation guard in
Flights.cached callback (it executes under registry mutex); keep its pure cache
recheck. Leader may retain preparation READ across vector CPU, but no SQL/file
lease or provider call. This prevents obsolete config work republishing after
memory invalidation without blocking reload on follower waits.

Challenges/rulings: cooperative writer try_write remains fail-fast in this task;
a short search read may cause explicit writer admission refusal, before project
status mutation. No hidden writer queue, priority/default or destructive state
change is introduced. Production coordination covers the cooperating full-index
and watcher writers, not arbitrary raw SQL/library writes. Readiness test polls
may retry only the documented Project generation/config changes and lease-busy
message in addition to prior states, preserving their original absolute deadline
and success/content assertions. New actual lock contention regression covers it.

Fixture correction: several old tests set only the in-memory Project.status to
Indexed while SQLite remained NotIndexed (tools::tests::env, owned payload
consumer fixture and search_golden; fixtures using run_project_index already
persist correct state). Real production binding reads
SQLite; fixtures must persist mark_indexed as well. Preserve every ranking/golden/
provider expectation; this does not authorize a baseline/threshold change.

Background refresh still needs prepared-pool/lazy adoption in S3.6c. S4 timer/
transport EOF cancellation, active-group checks, actual-job accounting and queue
telemetry plus whole-feature final/native acceptance remain mandatory. This
task enables concurrent search resource ownership, not whole #66 delivery.

S3.6b validation ruling: cli_read::read_path_needs_no_writer previously conflated
a live Tantivy writer handle (which does not prohibit reading a committed index)
with the exclusive cooperative project lease (which protects an active multi-
store update). Preserve success while only the Tantivy writer handle is held;
under the project writer lease search must return explicit retry, while list/
status succeed; after releasing that lease search succeeds with the original
content even before the Tantivy writer handle is dropped. The old unconditional
search-success assertion under the project WRITE lease contradicts FR-4 and is
corrected explicitly, not removed. Real IPC refusal/recovery tests the same rule.

S3.6b telemetry ruling: original-start explain timing also applies to limit0.
Keep zero hits/window/stages and no payload/provider work, but report actual
request time in the existing meta.stage_ms.total field rather than hardcoded0.
The injected-start test covers both nonzero and zero limits without sleeping.
Self-review caught the omitted zero branch before full functional tests began.

S3.6b full-suite ruling: tests/support/bench.rs::build_env used the same
in-memory-only Indexed shortcut as the smaller fixtures; the new real SQL
status check rejected19 benchmark/external cases before retrieval. Persist
mark_indexed and read back the registry Project. Corpus, recordings/model pins,
reports, expected quality/legacy parity and latency thresholds remain unchanged.
The prior full failure is retained as diagnostic evidence, not a passing run.

## Delta 2026-11-10 — S3.6c: prepared-reader background ANN refresh

FR-1/FR-3/FR-4/FR-5 completion for the refresh consumer. Shared stores the
already prepared ReadPool in Arc; clones share the same capacity/connections,
not a new pool. Refresh captures only this Arc, its config/cell, project/model
identity and dense snapshot, without retaining all Shared. No per-thread Db
open, migrating path, fallback policy or personal query timeout is introduced.

A warm-enabled worker obtains G through try_acquire plus current preparation
and OS project READ, using the same checked SQL status/root/schema/pending
helper as search. A private before-metadata observer sees this real readonly
reader for policy/failure/gate tests. Scalar reader and both guards drop before
calling warm_with_metadata_admitted. Its actual cold leader lazily captures owned
metadata through the same prepared-source helper; only its revision guard spans
resolve/publication. Followers retain no outer preparation/SQL/file lease.
Making those two tools helpers pub(super) shares daemon implementation without
a public API or new duplicate status/error/default logic.

Existing refresh gates stay: enabled + Ranking::Rrf only, usable embedding config,
recipe-qualified model/G, Ready/TooLarge coverage refresh, ANN threshold/backoff
and worker charge. Cleanup runs after skipped, stale, busy-pool or failed warm,
even when no reader was available. No ANN/query outcome reset or destructive
invalidation. Background metadata admission is fail-fast and independent of any
interactive caller's budget/cancellation, preserving shared work for live peers.

Challenge: a worker holding the scalar reader while the lazy factory starts
would fail at capacity1. A real warm/cache-hit regression configures capacity1
and exact-only fixture cache to isolate this ownership requirement. Another
regression holds every prepared reader: old per-thread open still invokes its
callback, while the new worker refuses without an unpooled replacement and
still cleans stale artifacts. The existing refresh fixture already indexes and
reads back persisted Indexed state; it needs no status shortcut correction.

Private test callback changes from whole warm replacement (DenseCache+Db+IDs)
to actual scalar metadata observation (Db+IDs); production performs the real
warm after it. Preserve readonly29ms, cleanup-on-error, gate order, disabled/
lexical skip, threshold and obsolete delayed-revision assertions. No sleep for
ordering or increased watchdog/threshold. S4 timer/transport/group cancellation/
accounting/telemetry and whole-feature final/native acceptance remain mandatory.

S3.6c API note: read_pool() still returns &ReadPool, but no longer has a const
qualifier: Rust1.88 cannot dereference Arc in a const function. Shared is a
runtime-only constructed service; runtime calls and capacity identity stay.

## Delta 2026-11-10 — S4.1: interest between shared cold phases

FR-5/FR-6, SC-3/SC-5. Flights expose a private checked-work variant with a
group-interest view, derived from all registered live participants rather than
the leader's token/deadline. Its private typed outcome distinguishes ordinary
work failure from abandonment. The existing run adapter and public dense/query
APIs stay unchanged. Checks run outside registry/state locks; provider work and
configuration admission never run while a flight mutex is held.

Cold dense initialization checks group interest before/after revision admission,
after owned metadata returns, before vector/ANN IO and after those synchronous
phases before publication or starting ANN CPU work. A phase already executing
may finish; cancellation is cooperative, without forced interruption or rollback
of already published state. No personal request token is consulted by the shared
factory. Background warm has its own live participant as before.

On an explicit no-interest outcome retire the flight with Abandoned, release
the leader's guard/metadata, wake followers and return cancellation to its now
cancelled leader. Live callers that joined/selected around the decision retry
normal admission and recheck completed cache; they must not inherit an obsolete
group-cancellation error. An ordinary Admission from a loader/provider remains a
shared error and is not silently retried. Leader unwind still completes/wakes.

Challenge: a live caller can arrive after the last interest check but before
retirement. Preserve the earlier atomic-retirement/retry semantics for this
new between-phase decision, not just the existing before-start decision.
Tests gate metadata and retirement, cancel all original participants and verify
no resident state/vector materialization, retry recovery, and late live caller
success. Existing leader-cancel/live-follower and error/unwind tests remain.
No new timers, sleeps, defaults, crates, ranking/window/baseline or transport
changes here. S4 transport/caller timer/actual-job accounting and queue telemetry
plus final whole-feature/native gates remain mandatory; PR112 remains draft.

## Delta 2026-11-10 — S4.2a: cancellation-safe bounded IPC framing

FR-6 prerequisite. Before adding concurrent EOF/deadline observation, move the
existing daemon NDJSON reader into private ipc::framing::FrameReader<R> for
AsyncBufRead. The per-connection object owns its pending bytes; read_frame()
may be dropped and polled again without losing consumed partial input. Its
fill_buf/consume loop copies only up to the existing protocol cap plus framing
lookahead. No await separates copying bytes and consuming those same bytes.

Preserve Frame Line/Eof/Incomplete/TooLarge semantics, exact accepted payload
limit, delimiter handling and the daemon's existing error replies/connection
closing policy. This private helper can later serve the async frontend client;
it introduces no public wire/API/config key or new bound/default. Janitor idle
select uses the persistent reader. Timer/EOF during active search, scoped wire
capabilities, accounting and original frontend budget remain future S4 tasks.

TDD: write a prefix into a real Tokio duplex stream, manually poll the actual
old read_frame future until Pending, prove the prefix was consumed, drop that
future, append the suffix and verify the full original frame. Existing local
Vec loses the prefix and fails without a real-time wait. Adapt only reader
construction/call to the new helper and move this regression into its private
unit module; expected bytes and cancellation sequence stay identical. Existing
real IPC exact-limit/oversize and frontend suites remain authoritative for wire
compatibility. Incomplete-frame classification is preserved by code inspection;
no dedicated partial-EOF test is claimed. No timing sleeps/watchdog loosening or
new crates.

## Delta 2026-11-10 — S4.2b: deadline driver and actual-work ownership

FR-6/SC-5, without claiming the later scoped-wire/frontend delivery. Each search
has its existing original SearchOperation before spawn_blocking. The async
dispatcher races its actual worker against the same absolute Tokio deadline.
Expiry cancels that caller token and returns Admission; dropping the dispatch
future also cancels it. A running sync provider may finish, but operation checks
prevent the next expensive stage. Shared flights still use group interest,
never the initiating request's personal budget. No new timeout/default.

Each actual connection request has one shared RAII activity: increment in_flight
once, retain its counter/events and an optional Arc of the already acquired
connection semaphore permit. The async handler holds the activity through reply
delivery, while the actual blocking closure independently holds a clone until
completion/unwind. Last drop decrements and emits a lifecycle event. A timed-out
search attempts its error reply and closes that connection rather than starting
more detached work under one permit. The existing graceful drain and signal
escalation count actual still-running jobs as well as replies.

Real accept uses an admitted connection entry with its existing permit;
handle_connection's public compatibility signature remains an explicit adapter
without an externally admitted permit. Ordinary tools also capture activity in
their blocking closures, preserving accounting on future abandonment. A private
peer state groups hello/session/janitor fields to pass dispatch state coherently.
Legacy CallTool/session/half-close semantics otherwise stay unchanged.

The driver has a private injected-expiry seam for deterministic tests; production
always supplies sleep_until(operation.deadline). An expiry signal cancels the
token explicitly, without assuming Tokio virtual time advances std::Instant.
No timer polling, sleeps for ordering, hard provider preemption, new crates or
test clock feature is needed. Test actual dispatcher abandonment during gated
embedding: abort and await the caller future, release the provider, drain its
runtime, then assert no reranker call. Original code wrongly runs the next stage.
Activity/expiry tests gate actual blocking work and reply ownership, verify one
count/permit survives caller expiry and drops only after both owners finish,
including error/unwind; ordinary successful dispatch preserves response behavior.

This is not yet active transport EOF/capability negotiation, a deadline over a
blocked reply write, original frontend queue/reconnect budget or queue telemetry.
Those remain mandatory S4.3 in the same draft PR before whole-feature review and
final-head native acceptance; an intermediate task is never a deliverable.

## Delta 2026-11-10 — S4.3a: scoped server transport

Add Request::Search { args, context: SearchCallContext } with required u64
remaining_ms/elapsed_ms/queue_ms. Response::Hello advertises a Vec<String>
capabilities field (serde default empty, omit when empty for legacy fixtures).
The fixed search-context-v1 identifier and one-outstanding-call rule are
protocol identity/framing invariants, not configurable operational limits.
No timeout/default is added to the protocol. Old CallTool/Hello decoding remains.

The composition root checks positive remaining and queue<=elapsed, reconstructs
local original start/caller deadline with checked Instant subtraction/addition,
then SearchOperation::constrained(complete policy, original start, caller
deadline, token) clamps to the owner deadline and checks expiry. Existing new()
primitive semantics remain. A shared private atomic queue accumulator records
explicit upstream duration, actual blocking-worker queue and personal prepared
reader admission; shared cold factories remain independent of caller budgets.
No arbitrary upper time cap or store->hybrid cancellation dependency is added.

A private request scope groups Activity and optional SearchOperation. One decode
selects legacy dispatch or scoped search. Scoped dispatch concurrently observes
the persistent FrameReader, per-session janitor signal and original deadline.
EOF cancels/closes; another complete request while scoped work is pending is a
protocol refusal/close, not a second job. An incomplete next read remains owned
by FrameReader if work finishes first. Graceful global shutdown retains its
existing drain behavior; it is not an extra cancellation source.

The same absolute deadline bounds reply waiting. On expiry, cancel the caller
and close after a best-effort immediately writable error frame; never start a
fresh delivery timeout or wait indefinitely for that error. Already-partially
written replies may end at EOF and clients must discard their connection.
Activity stays owned through actual work and delivery/closure. Legacy search
also retains its original daemon budget; its read half is not monitored during
work, preserving successful write-half-close/reply semantics.

SearchOperation queue methods take explicit durations; no public settings are
read by the consumer. Metadata owner adds to_json_with_queue/envelope_with_queue
adapters, preserving existing SearchMeta struct and old serialization signatures.
Actual search and limit0 use the queue-aware envelope; default arrays, score,
windows, snippets, citations and stage names stay unchanged.

Tests first prove missing capability on an actual daemon Hello, then cover
legacy empty-capability decode, scoped serialization, malformed/expired context,
original budget clamp and explicit queue propagation. Gate actual providers and
real byte streams for scoped disconnect, leader/live-follower separation,
physical activity/permit retention, legacy half-close, reply backpressure and
partial-frame preservation. Generous explicit test policy/watchdogs establish
ordering without asserting host performance. OS async connect, when needed,
stays in platform (Tokio UnixStream, existing ConnectOptions::connect_tokio on
Windows); no new crate. Native tests must distinguish logical duplex behavior
from actual platform EOF behavior and must not silently skip Windows failures.

S4.3b still owns canonical async MCP/CLI clients, capability refusal, original
queue/retry budget, rmcp cancellation and product docs. The same draft PR stays
unmergeable until that work, whole-feature final review and final-head native
checks complete. No remote async provider rewrite, HTTP adapter or v2 config.

## Delta 2026-11-10 — S4.3b: async callers, staged adoption

S4.3b1 adds ipc::async_client::AsyncClient using the existing platform Stream
and FrameReader. Connect/call/Hello/session/tool operations are asynchronous;
no socket timeout setter or blocking socket read is used. The low-level connect
and generic exchange have no invented timeout; their composition owner bounds
them. AsyncClient::search requires an explicit SearchOperation and capability,
serializes explicit remaining/elapsed/queue, bounds exchange by its original
absolute deadline and finalizes existing explain total after decoding. Missing
capability returns Admission with restart guidance before sending a Search.

An exchange arms a poison-on-drop guard before IO. Interrupted/malformed/EOF/
oversized exchanges leave the client unusable; a fully decoded response disarms
it. A search operation driver checks before and after completion, races original
sleep_until, cancels on expiry/drop and never creates a new stage budget. A
private injected expiry future permits deterministic tests without treating
Tokio virtual time as std::Instant. Normal daemon errors retain their category.
The driver is reusable by caller composition; it has no rmcp dependency.

S4.3b2 adopts this client in MCP with a Tokio connection mutex. A caller first
waits for the mutex, then takes the Option<Conn> into its future and restores it
only after a healthy completed exchange. Dropping a caller closes only its own
leased connection; a cancelled mutex waiter leaves the active lease untouched.
One original operation precedes ensure_daemon/spawn/lock/reconnect and includes
actual queue delay. Async lifecycle wrappers retain real blocking startup work
and check cancellation before subsequent phases. Search observes rmcp request
context cancellation. A single reconnect is permitted only after Transport and
only while that same operation remains live; no expired/cancelled replay. Normal
startup resolves Config::global search settings; Frontend::new compatibility
obtains defaults explicitly from SearchSpec at its composition boundary.

S4.3b3 adopts the same async client/driver through a safe synchronous CLI bridge,
with no nested-runtime panic. Failed initial connect alone permits direct mode;
there is no direct fallback after a connected failure. Both routes retain the
original CLI ingress time and complete configuration-owned search policy before
providers. Existing non-search routing remains compatible. Update product IPC/
architecture/configuration documentation, then whole-feature fresh final review
and all four latest-head native checks before ready/squash merge.

All stages remain in PR112 draft. Native real-stream client tests cover normal
context/response, missing capability/no request, dropped partial exchange and
poisoning, expiry/cancellation and original elapsed/queue metadata. Tests use
provider/IO gates and generous watchdogs, never new ordering sleeps or tight
host budgets. Existing CappedRead newline-overflow candidate must first be
proved against its actual implementation and duplicate-checked before a bug
claim/issue; any fix retains the existing exact frame limit. No new crate,
unsafe, network policy, remote-provider async rewrite, HTTP or v2 config scope.

### Delta 2026-11-11 — B2 implementation rulings

Keep non-search mutex waiting outside the existing catalogue exchange budget;
async control/bind operations use its existing QUICK_TIMEOUT, and index_project
uses LONG_TIMEOUT. Search bypasses those per-exchange timers and retains one
original operation across all startup/control/wait/retry phases. Connection and
handshake Transport errors also participate in the same single reconnect limit.
Cancellation takes/drops only the current future's leased connection; a queued
waiter has not taken one. Restore healthy completed exchanges, discard transport/
admission/protocol failures. Successful index remains successful if the later
rebind transport fails; reconnect on the next request instead of replaying it.

A private cancellable helper boxes the workflow before constructing its async
state (initial unboxed service future measured16744bytes by Clippy). This keeps
MCP service futures small without a lint suppression or another operational
limit. A startup helper retains actual spawn_blocking work, measures its queue,
checks the original token before invoking startup and after its real completion;
a cancelled caller can return while an already-running synchronous startup still
finishes. It cannot then dispatch paid search. Existing lifecycle locks/OS calls
are not forcibly interrupted and may complete an already-started daemon launch.

Normal MCP startup parses Config::global before creating the service, so invalid
global configuration fails explicitly rather than supplying a hidden search
policy. The compatibility constructor uses SearchSpec defaults explicitly; the
complete-policy constructor validates on each search before any startup/IO.

## Delta 2026-11-11 — B3 CLI composition and product contract

CLI search captures ingress, resolves one global Config snapshot and constructs
SearchOperation before any connection/provider. The async attempt wraps native
connect, Hello/project binding and scoped Search in within_operation. Only an
initial failed connect while the operation remains live returns None for direct
mode. Connected errors and expired/cancelled operation propagate, without CLI
transport retry or local fallback. The existing generic CLI try_daemon and
blocking Client remain non-search compatibility paths.

The synchronous CLI bridge creates/runs/drops a current-thread runtime on a
plain thread. If already inside a multithread Tokio runtime, use its sanctioned
block_in_place context for the bridge; inside an existing current-thread runtime
return explicit Admission instead of panicking/nesting or blocking its sole
executor. This is a sync CLI API, not an async library embedding promise.
No new threadpool, crate, timeout or implicit policy is added.

Direct fallback uses Config::for_project with that same global snapshot and
keeps the same operation/start. Check before/after synchronous provider creation;
direct sync providers remain cooperative, not forcibly preempted. Payload policy
and deadline derive from the same owner configuration. Do not reopen global
configuration and silently reset either elapsed time or policy at fallback.

Update docs/ARCHITECTURE.md, docs/MCP.md, docs/CONFIG.md and README.md where
applicable: prepared read phases, coalescing/consistency, configured search budget,
original queue/total, scoped capability/context, old-client limitations, EOF/
request cancellation, physical charging/nonpreemption, CLI initial-connect-only
fallback and legacy half-close. #79/#81/#78 remain separate future capabilities;
no official 20k capacity/HTTP/remote-async/configv2 claim.

## Delta 2026-11-11 — Windows busy-pipe compatibility

Locked interprocess2.4.4 local-socket adapters ignore connection wait mode.
Their named-pipe defaults use Unbounded: async busy admission spawns a blocking
WaitNamedPipe worker, and synchronous liveness enters the same indefinite wait.
Tokio runtime Drop waits for such workers. A cancelable outer future is
insufficient. Use the existing safe raw byte-pipe API with explicit
ConnectWaitMode::Timeout(Duration::ZERO), then its safe local-socket conversion
for the existing Stream type. ZERO selects one nonblocking attempt; it is not
a local operational timeout/default. Preserve GenericNamespaced local prefix
\\.\pipe\ and endpoint validation. No retry spin, sleep, new configuration,
crate or unsafe. A busy probe is present (raw zero-mode TimedOut), while absent
and invalid endpoints are false. Keep generic connect_blocking unchanged.

First test the actual old async adapter on native Windows with max_instances1
and an occupied byte pipe. Reserve the runtime's sole blocking worker using a
channel gate so the dependency's detached connect cannot start before cleanup.
One manual poll must return Ready with busy error; old code returns Pending.
Drop that future and both pipe handles before releasing the worker, then join
and drop the runtime before asserting. This proves the boundary without a
host latency threshold or leaving an indefinite worker behind in RED.

### Delta 2026-11-11 — native probe fixture teardown contract

An absent endpoint means no kernel pipe instance, not merely logical Drop of
Tokio wrappers. Mio retains Arc-owned handles for pending IO completion; Drop
cancels operations but their completions release those references later. Test
busy presence while occupied and true absence on a never-created unique name.
The existing transport removal regression joins its server/runtime before
checking absence. Do not impose immediate name disappearance after wrapper Drop
or add a sleep/timing allowance to make that invalid contract pass.
