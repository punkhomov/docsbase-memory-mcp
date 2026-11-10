# Search concurrency implementation plan

**Goal:** remove #66's global read lock without duplicate paid work or mixed citations.
**Architecture:** startup prepared readonly pool, coalesced hybrid work, short
coherent read phases and owned response payload, then explicit cancellation/timing.
**Spec:** docs/specs/search-concurrency/requirements.md
**Global constraints:** Rust1.88/edition2024; Linux/macOS/Windows. No new crates,
no new unsafe, no production unwrap/expect/panic. No ranking, golden/baseline,
provider window or latency budget changes. Production consumes explicit resolved
owner policies; no consumer defaults.
**Review focus:** pool overcapacity/transaction leaks (S1); duplicate paid work or
stuck waiters (S2); stale candidate/new citation mix (S3); detached work/accounting
and invisible queue (S4).

## S1 — prepared reader lifecycle

- Create `src/store/read_pool.rs`, `tests/read_pool.rs`.
- Modify `src/store/{mod,settings}.rs`, `src/daemon/{server,lifecycle}.rs`,
  `src/cli/mod.rs`; append relevant component-configuration adoption delta.
- Consumes CF2 `Db::open_readonly_with_settings`, DbSpec and ReadPoolSpec.
- Produces `ReadPool::prepare(cache, &Resolved<DbSettings>, &Resolved<ReadPoolSettings>)`,
  `acquire(Instant) -> Result<ReadLease>`, Shared/read_pool accessor and explicit
  two-policy startup APIs. Existing wrappers are compatibility-only.
- RED: real SQLite policy/readonly, capacity2/return/error/unwind, transaction
  cleanup, deadline and imported invalid-policy tests reference missing API.
  Run `cargo test --locked --features cli-full --test read_pool` and verify the
  expected missing module/interface diagnostics; then implement GREEN.
- Add actual Shared configured-capacity/timeout and lifecycle pre-effect tests.
- Acceptance: FR-1/FR-2/SC-1, dispatcher unchanged; no query-ready snapshot claim.
- Verify: focused integration + server/lifecycle tests, full cli-full suite,
  lean pool/config/platform suites, full/lean Clippy, fmt/diff, required native CI.
- Fresh task review PASS/APPROVED, whole-slice final review, green latest-head
  checks before PR merge. #66 remains open after S1.

## S2 — coalescing (after S1)

FR-5/SC-3: exact flight keys, failure/unwind/cancel/replacement, no duplicate cold
vector materialization. Concrete interfaces/tests must be fixed by dated delta
after source audit, before implementation. Search remains serial until S3.

## S3 — request resource phases (after S2)

FR-3/FR-4/SC-2/SC-4: coherent config/stages + cross-store snapshot, bounded owned
payload and reader release before provider/CPU/waits. Concrete lease/generation
strategy, bounds and regression tests require dated delta before implementation.

## S4 — dispatch cancellation, timing and acceptance (after S3)

FR-6/FR-7/SC-5 plus all native regression gates. Document synchronous provider
boundary honestly; integrate request deadline and additive explain telemetry.
Concrete wire fields/accounting/tests require dated delta before implementation.

## Delta 2026-11-10 — S2a and S2b execution order

### S2a — coalesce query embeddings

- Create `src/hybrid/{work,flight}.rs`; modify `src/hybrid/{mod,dense}.rs` and
  `src/daemon/tools.rs`; append fixed wake-cardinality exception to settings inventory.
- Consumes existing completed QueryCache/model identity, Embedder and blocking seam.
- Produces `Cancellation::{new,cancel,check}`, internal Flights run/cache-recheck
  API, `DenseCache::query_embedding(&dyn Embedder, &str, &Cancellation)` returning
  shared Arc<[f32]>. Cold dense rows and daemon routing are unchanged.
- RED first: four gated misses one provider call/Arc identity; error wakes
  waiters and retry succeeds; cancellation of one waiter/leader preserves live
  participant result; panic wakes/removes/retries; different model keys do not
  coalesce. Confirm `blocking::enter` leaks its flag on unwind, then fix via RAII.
- Test-only per-instance join observer confirms registration without sleep.
  Unit tests inspect no retired flights/subscriptions after completion/cancel.
- Verify focused flight/query/blocking tests and actual dense search core, full
  cli-full, lean hybrid/config tests, full/lean Clippy/fmt/diff. Cargo feature
  builds execute sequentially when sharing target/debug/docsbase.
- Fresh task review; final source review and required CI before its PR merge.
  FR-5 query subset/SC-3 subset only; #66 stays open.

### S2b — coalesce cold dense initialization (after S2a)

Reuse Flights, admit before full metadata/hash/vector copies, provide lazy
metadata-loader API for S3 pool ownership, preserve ANN slot/accounting contracts.
Settle generation/config publication authority in a dated design/task delta
before code. Existing S3/S4 boundaries remain in force.
