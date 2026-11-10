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
