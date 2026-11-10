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
