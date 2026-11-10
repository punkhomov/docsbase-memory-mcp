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

## Delta 2026-11-10 — S3.3: preflight, owned capture и lifetime

- Создать `src/search/{payload,payload_tests}.rs`, подключить модуль. Consumes
  S3.2 PayloadBudget/Permit, Db/repo и S3.1 pending predicate. API/data/charge
  formula — dated S3.3 design; без defaults/connection opens/provider calls.
- RED: missing PayloadRequest/OwnedPayload API. Настоящий SQLite: identity и
  порядок текста/citations/meta, flags без лишних полей, missing-doc/rerank
  behavior, wrong root/gen/pending/foreign keys, cancellation, bound/duplicates.
- SQL preflight/copy snapshot: gated writer commit между ними возвращает целиком
  old payload/charge; success/error/unwind заканчивают только свой transaction.
  UTF-8 multibyte/NUL byte count и UTF-16 CJK/emoji upper bound, request/aggregate
  refusal before large copies, error/cancel cleanup и повторная capture.
- Проверить actual payload data-drop gate: пока destructor данных удерживается,
  второй reserve ещё отклоняется. После release весь charge доступен. Permit
  last field и construction locals order подтверждаются реальными объектами.
- Тесты не закрепляют platform-dependent numeric struct sizes: boundary charge
  берётся из captured payload, а значения/идентичности — из SQL fixture.
- Verify focused payload/policy/metadata, полный cli-full, lean relevant suites,
  full/lean all-target+opt-in Clippy/fmt/diff; свежий task reviewer. Whole feature
  final/native gates сохраняются. Actual consumers/revision/dispatch ещё pending.

## Delta 2026-11-10 — S3.4: owned response consumers

- Изменить src/daemon/tools.rs, src/daemon/server.rs, src/cli/search.rs,
  src/search/settings.rs; создать src/daemon/search_payload_tests.rs;
  расширить tests/cli_read.rs и tests/ipc_server.rs реальной global-policy
  admission через CLI subprocess/daemon wire (isolated per-process config).
- Consumes OwnedPayload/PayloadBudget/Cancellation. Produces SearchContext,
  search_docs_in и private owned PreparedSearch/finish без metadata handles.
- RED: missing new context/prepare API; real SQL/Tantivy fixture и fake reranker.
  После prepare закрыть Db и получить прежние texts/citations/filter/generation.
  Изменить SQL во время provider и проверить целиком прежний payload.
- Проверить explicit tiny request refusal до provider, actual aggregate
  contention при gated rerank, release+retry после success/error/unwind,
  cancellation до/после prepare и после provider. Только gates/watchdogs.
- Production daemon использует один shared global search policy budget; CLI
  direct использует config.search. Compat adapter разрешает SearchSpec явно.
- Сохранить exact provider candidate order/top_n, фильтры/dedupe/snippet,
  missing-doc behavior и legacy wire; limit0 не резервирует payload.
- Verify focused owned consumers и existing ranking/search-ux/config suites,
  full cli-full, lean relevant tests, full/lean all-target+opt-in Clippy/fmt/diff.
  Fresh task review перед commit. S3.5/S4 и whole-branch/native gates ещё pending.

S3.4 validation addition: tests/config_runtime.rs readiness helper accepts only
transient Project states until a real success under the existing watchdog;
explicit pending-marker IPC refusal/recovery added. Preserve all watcher/reopen
assertions. Full-pending-contract-red.log is the actual prior failure evidence.

S3.4 same-shape validation audit: tests/support/search_visibility.rs is included
by config_runtime, mcp_registry, admission_ux and soak; adapt existing polling
refusal classification without dropping assertions or increasing watchdogs.
Actual MCP full-suite pending-marker failure retained as additional RED evidence.

S3.4 additional validation: run existing nightly soak with SOAK_SECS=10 to
exercise its changed search-readiness classification; this does not change the
nightly job, ignored status or its acceptance thresholds. Local full command
uses --no-fail-fast to collect every target in one pass, preserving all checks.

## Delta 2026-11-10 — S3.5: stable runtime cell и memory invalidation

- Base: S3.4 commit22019f7. Изменить src/daemon/server.rs,
  src/daemon/project_state_tests.rs, src/hybrid/dense.rs и их local tests.
- API: ProjectCell state/reload/preparation и with_preparation, crate-private
  DenseCache::invalidate_memory. Contract — dated S3.5 design, без новых defaults.
- RED до production: missing cell fields/with_preparation/invalidate_memory.
  Проверить obsolete revision без work, current revision, error/unwind release,
  state/preparation free при gated model construction, reload serialisation и
  active preparation blocking publication без state-mutex hostage.
- Preserve failure/obsolete-lazy-builder tests #76; добавить reload panic/recovery.
  Семантически changed reload очищает resident identity, unchanged сохраняет;
  query cache/last outcome/persisted ANN и worker reservation сохраняются.
  Late invalidated worker не публикует graph и не освобождает charge раньше конца.
- Gated delayed old ANN refresh отвергается после реального reload до warm;
  cleanup и configured readonly policy tests сохраняются. Только private hooks,
  channels/gates и watchdog на failure, без sleeps для ordering.
- Verify focused state/dense/refresh + old integrations, full cli-full, lean
  relevant scopes, full/lean all-target+opt-in Clippy/fmt/diff. Fresh task review
  перед commit; source findings только через отдельного implementer.
- Actual search pool/cross-store/cancellation/dispatch/timing и whole-feature
  required native CI остаются отдельными шагами до delivery.

## Delta 2026-11-10 — S3.6a: one request budget, before pool adoption

- Base4a5c28f. Modify src/search/{settings,mod}.rs, src/config/mod.rs doc,
  tests/search_payload_budget.rs and explicit payload-test policy fixtures;
  add src/search/operation.rs and operation_tests.rs. Update owned settings
  inventory. No new crates, transport/schema/ranking/baseline changes.
- API: SearchSettings/SearchPatch.request_timeout_ms; SearchOperation::new(
  &SearchSettings, original_start:Instant, Cancellation)->Result<Self>.
  Accessors started_at/deadline/cancellation, check/remaining/elapsed.
- RED missing policy/context API. Inject Instant values in private check_at/
  remaining_at/elapsed_at helpers: exact deadline, queued elapsed time, clones
  cannot reset budget, cancellation wakes request token, independent requests,
  checked overflow and invalid imported policy. No sleeps or timing SLA.
- Real config tests: default30000/Daemon/Milliseconds provenance, timeout-only
  override preserves byte defaults, partial byte policy preserves timeout,
  project file/typed patch rejects daemon override; inherited global snapshot,
  zero/negative/unknown rejection and unchanged legacy config JSON.
- Verify focused operation/policy/config, full cli-full, lean operation/config/
  existing consumers, full/lean all-target+opt-in Clippy/fmt/diff. Fresh task
  reviewer required before commit; controller delegates reviewer fixes.
- Pool/core adoption and S4 timer/transport/dispatcher/telemetry plus native
  acceptance remain pending. Primitive alone is not independently shippable.

## Delta 2026-11-10 — S3.6b: pooled core and guarded dense admission

- Basebf450f1. Modify src/store/read_pool.rs; add src/search/metadata.rs;
  modify search/mod.rs, daemon/{tools,server}.rs, hybrid/dense.rs, cli/search.rs;
  add daemon/search_phase_tests.rs and hybrid/dense_admission_tests.rs. Adapt
  existing owned-consumer Context constructors and persisted-status fixtures;
  tests/read_pool.rs, tests/ipc_server.rs and shared readiness classifier.
- Consumes complete operation/payload policy, prepared ReadPool, ProjectCell
  revision gate and OS read lease. Produces MetadataSource/MetadataReader,
  pool.try_acquire/cache_root, checked preparation guard, guarded lazy dense
  get/warm and search_docs_from. Compatibility signatures search_docs_with and
  search_docs_in remain; new Context is unpublished within this branch.
- First meaningful RED on existing production: pause real reranker through
  async call_tool dispatcher, observe Shared.db.try_lock fails, release/join
  before assertion. After implementation same-project lexical/status/list must
  complete before release; cap1 pool, config gate and writer lease are free.
- MissingAPI RED for try_acquire/metadata/guarded dense; actual SQLite readonly,
  exhaustion/release/error/unwind/quarantine without elapsed wall-time tests.
- Gate embedding with cap1: no SQL/global Db/preparation/file hostage; actual
  sync generation change or config reload before resumption gives retry, no
  mixed payload/obsolete resident publication. Invalid BM25 query calls no
  embedding. Existing rerank-time old-payload result remains unchanged.
- Dense leader/follower gates prove follower holds no admission guard while
  waiting, one cold loader, leader cancellation retains work for live follower,
  obsolete admission refuses before loader and no registry/gate deadlock.
- Real cooperative writer/IPC refusal and recovery; narrow transient-test
  classification keeps original deadlines, fatal categories and actual success.
- Explicitly persist Indexed in existing inconsistent fixtures; retain counts,
  assertions/golden/provider windows/budgets. Source identity, limit0 and exact
  fallback behavior preserved. No new crates/unsafe/global hooks/defaults.
- Verify focused new phase/dense/pool suites, existing ranking/visibility/config
  integration, full cli-full, relevant lean scopes, full/lean all-target+opt-in
  Clippy/fmt/diff. Fresh task review before commit, delegated reviewer fixes.
- S3.6c refresh adoption and S4/final/native requirements remain separate and
  mandatory before the one whole-S3/S4 draft PR becomes deliverable.

S3.6b timing regression also covers limit0: existing envelope/window/stages stay,
but total includes injected original queue history, with no real wait/SLA.

S3.6b full-suite addition: correct tests/support/bench.rs::build_env persisted
index status (same fixture shortcut), preserve frozen inputs/recordings/baseline;
focused benchmark/external and lean benchmark validate this changed harness.
