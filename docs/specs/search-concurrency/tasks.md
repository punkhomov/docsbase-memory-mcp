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

## Delta 2026-11-10 — S2b concrete execution contract

- Create `src/hybrid/dense_metadata.rs`; modify `src/hybrid/{mod,dense}.rs`;
  add dense cold tests under `src/hybrid/` or existing dense tests module.
- Consumes S2a Flights/Cancellation, CF2 Db and existing repo/vector/ANN owners.
- Produces private-data `DenseMetadata::capture(&Db, i64)`, explicit
  `DenseRequest`, `get_with_metadata` / `warm_with_metadata` lazy loader APIs;
  existing get/warm remain source-compatible adapters. No direct metadata open.
- RED first: real SQLite coherent capture, identity validation/no vector IO, four
  gated misses one loader/Arc, error/cancel/retry, warm Ready refresh and late old
  generation cannot replace/new-build. Run filtered unit targets and confirm
  missing API/old behavior failures before implementing each behavior.
- Refactor cold resolve from Db reference to owned metadata; bind canonical
  cache identity and share cold registry only with shared row snapshots.
- Preserve reservation refusal, ANN live-slot/worker cleanup, max-project
  disablement and all existing cache counter/quality contracts.
- Verify focused units + existing dense/ANN/worker tests, full cli-full, lean
  hybrid and relevant integrations, full/lean all-target+opt-in Clippy/fmt/diff,
  then fresh task/final reviews and latest-head four required native checks.
- FR-5 cold/generation subset complete only; config revision publication and
  coherent Tantivy/citation capture remain S3. #66 stays open.

## Delta 2026-11-10 — S3.1: read barrier и crash-marker predicate

- Изменить `src/daemon/lifecycle.rs`, `src/store/repo.rs`; добавить локальные
  lease tests в `src/daemon/project_read_tests.rs`. FR-4 foundation only.
- API: `with_project_read_lease(cache, project_id, FnOnce()->Result<T>)` и
  `repo::has_pending_docs(&Connection, project_id)->Result<bool>`.
- RED: реальные shared/shared, shared/exclusive, exclusive/shared locks;
  освобождение reader при error/unwind и независимость разных проектов.
  Настоящий SQLite: empty/ready/pending, изоляция проектов, cleanup/удаление
  marker. Сначала подтвердить ошибки отсутствующего API.
- GREEN: fail-fast shared lock по существующему writer path, без provider/
  migration/default timeout. Predicate только читает переданный connection.
- Проверки: focused units, существующие writer admission/watch retry tests,
  полный cli-full, lean relevant tests, full/lean Clippy и fmt/diff. Свежий
  task reviewer. Whole-S3 final review и required native CI перед одним PR.
- После S3.1 не объявлять FR-4/SC-4 или #66 завершёнными: owned payload,
  config authority, production adoption и S4 остаются отдельными шагами.

## Delta 2026-11-10 — S3.2: component policy и shared byte permits

- Создать `src/search/{mod,settings,budget}.rs`, подключить в `src/lib.rs`;
  изменить `src/config/{mod,resolve}.rs`; дополнить settings inventory.
- API/числа/scope — dated S3.2 design: owner defaults64MiB/256MiB,
  `Config.search`, global-only table, pure owner/provenance и shared RAII budget.
- RED до production: missing SearchSpec/SearchSettings/Config.search/budget API.
  Проверить omitted/partial/full global policy, project/unknown/invalid отказ,
  source/unit/scope, impact и неизменность legacy hybrid/config JSON.
- Budget tests: на границе/за границей, aggregate contention с удерживаемыми
  permits, clone sharing, release на error/unwind/other thread, checked overflow
  без allocation, imported invalid settings и отсутствие изменения после отказа.
- Consumer не выбирает defaults и не выполняет filesystem/network actions.
  Новые payload settings применяются реальным pipeline до delivery whole S3.
- Verify focused config/budget, полный cli-full, lean config/budget и нужные
  integrations, full/lean all-target+opt-in Clippy/fmt/diff; свежий task review.
  S3.3 owned SQL capture и последующая revision/pipeline adoption ещё не готовы.
