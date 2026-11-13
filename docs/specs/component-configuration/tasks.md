# План внедрения компонентной конфигурации

**Goal:** FR-1–FR-17 с сохранением v1 и явно управляемой инвалидацией.
**Architecture:** typed component specs, общий resolver, отдельный artifact planner
и prepare/publish coordinator. Базовый bootstrap/store slice предшествует #66;
остальное внедряется по владельцам и отдельным storage/compatibility gates.
**Spec:** [requirements.md](requirements.md). **Design:** [design.md](design.md).
**Global constraints:** Rust1.88; Linux/macOS/Windows; cli-full и lean offline;
новых crates нет; production unwrap/expect/panic запрещены; baseline не меняется;
v1 contracts сохраняются; merging — human action.

**Review focus:** hidden fallback (CF1/CF2), legacy scope/merge reset (CF3),
no-write plan/secret leak (CF4/CF5), stale publication/WAL/readers (CF10/CF11),
shrink с живыми handles (CF6/CF7/CF13).

CF0 — текущая docs-only поставка. CF1/CF2 — следующие реализационные PR.
CF3–CF9 задают owning slices; CF12/CF13 — milestones, которые разбиваются на
session-sized tasks до кода. CF10 — обязательный отдельный
storage design gate перед CF11. Если фактический API требует нового crate или
breaking v1 behavior, до кода меняется ADR/plan в reviewable delta.

## CF0 — инвентарь и архитектурный контракт

**Files:** Create requirements/design/settings-inventory/state-dependencies/tasks/
progress в этой директории; Modify AGENTS index и core design dated cross-reference.
**Produces:** правила классификации, typed contract, миграционные и policy decisions,
матрица влияния и порядок реализации. **Covers:** FR-1–FR-17 на уровне design;
приёмка только SC-1/SC-2.
**Verify:** source/symbol/link/FR-coverage audit, docs gate; fresh task/final review,
required CI. Runtime tests для новой документации не добавляются. #78 не закрывается.

## CF1 — typed resolution foundation при сохранённой v1 семантике

**Depends:** CF0. **Covers:** FR-2/3/5/6/7, SC-3 (первый slice).
**Create:** `src/config/schema.rs`, `src/config/bootstrap.rs`, `src/config/resolve.rs`,
`src/store/settings.rs`, `src/hybrid/dense_settings.rs`, `tests/config_resolution.rs`.
**Modify:** `src/config/mod.rs`, `src/store/mod.rs` (module declaration),
`src/hybrid/mod.rs` (ungated settings module); этот progress ledger.
**Consumes:** существующие raw global/project layers/provider validation.
**Produces:** `BootstrapContext`, `DefaultsContext`, `SourceId`, `Provenance`,
`SettingsSpec`, `PatchValue<T>`, `Resolved<T>`; typed `DbSettings`, `ReadPoolSettings`
и `DenseSettings`; neutral data-only `ArtifactId`, `ArtifactFacts`, `ComponentImpact`
в `config/schema.rs`, чтобы `SettingsSpec::impact` не зависел от будущего CF4.
Planner algorithms и `ChangePlan` здесь не вводятся. Settings и defaults catalogs
не содержат model/DB handles; `EntrypointFacts { mode, is_tty }` передаются bootstrap.

1. RED: `v1_hybrid_layer_replaces_whole_value`,
   `absent_and_disabled_stage_are_distinct`, `project_cannot_set_daemon_budget`,
   `component_defaults_are_resolved_once`, `invalid_env_fails_before_runtime`,
   `hybrid_true_uses_both_default_stages`, `hybrid_false_clears_both_stages`,
   `v1_table_replace_uses_empty_stage_table_base`.
   Assert legacy result byte/field-equivalent; true имеет обе default stages,
   false — ни одной, cache-only v1 table — enabled=true без stages. Defaults catalog
   строится один раз на resolution и повторно используется overlay; bool resets
   не наследуют прежние model/window/cache. Secret sentinel отсутствует в
   debug/effective/provenance.
2. Перенести решение defaults в specs, явно захватить bootstrap env/OS facts.
   `defaults(ctx)` возвращает immutable owner catalog, `initial(&catalog)` выбирает
   baseline, `overlay(..., &catalog, origins)` получает те же templates. Hybrid catalog
   содержит disabled, enabled-shortcut с обеими default stages и table-base с enabled
   true/без stages; это не baseline+toggle. `Config::load/global/for_project` пока
   adapters к legacy patch semantics.
   Новые additive daemon-only секции могут вводиться в v1 без изменения старого
   hybrid merge; v2 field-wise merge/strict vector owner ещё не объявляются доступными.
3. DbSettings сохраняет read-write busy30s/WAL3×100ms и read-only busy5s
   (нынешний rusqlite default5000ms), передавая оба значения явно; write default
   не присваивается всем handles.
   ReadPoolSettings содержит max_connections4, но public pool override активируется
   только вместе с реальным pool consumer в #66, не как неработающий ключ.
4. Новые pure types не переводят автоматически весь DenseCache на новые настройки;
   эта adoption принадлежит CF6. Defaults byte/quality values остаются прежними.

**Verify:** `cargo test --locked --features cli-full --test config_resolution`;
`cargo test --locked --test config_resolution --test config`;
full/lean Clippy, fmt и native CI. Проверка consumer override заканчивается в CF2/CF6,
а этот PR не объявляет весь FR-3 завершённым.

## CF2 — явные DB settings и read-handle ownership для #66

**Depends:** CF1. **Covers:** FR-3/5/7/17; SC-3 для store.
**Modify:** `src/store/mod.rs`, `src/daemon/server.rs`, `src/daemon/lifecycle.rs`,
`src/daemon/registry.rs`, `src/daemon/tools.rs`, `src/watch/mod.rs`,
`src/cli/index.rs`, `src/cli/search.rs`, `src/cli/status.rs`,
`src/cli/install.rs`, `src/cli/update.rs` — только фактические metadata DB callsites;
перед началом обновить `rg 'Db::open|Db::open_readonly' src` и ledger.
**Create/Test:** `tests/store_settings.rs`.
**Consumes:** `&Resolved<DbSettings>`, explicit cache path.
**Produces:** `Db::open_with_settings(cache, settings)`,
`Db::open_readonly_with_settings(cache, settings)` для operational readers и
отдельный `Db::inspect_readonly(cache, settings) -> Result<ArtifactFacts>` для
side-effect-free inspection. DTO ArtifactFacts уже введён CF1. Missing/schema
mismatch представлены facts без conflict::record; если достоверное no-write чтение
невозможно, результат Unknown/blocked. Старые operational signatures остаются
compatibility adapters к одному standalone resolver; production их не вызывает.

1. RED: `readonly_open_never_migrates_or_creates`,
   `configured_busy_timeout_reaches_connection`, `wal_retry_policy_is_explicit`,
   `invalid_db_settings_leave_files_unchanged`,
   `inspector_missing_and_schema_mismatch_leave_filesystem_unchanged`,
   `inspector_wal_with_missing_sidecar_does_not_create_or_ignore_wal`.
   Use actual SQLite + controlled lock/wait seam; проверять `PRAGMA busy_timeout`
   read-write30000ms/read-only5000ms, attempts и полный filesystem snapshot,
   включая conflict log/WAL/SHM/directories, а не elapsed-time performance assertion.
2. Развести migrating writer/startup и prepared read connection construction.
   Operational read-only schema mismatch остаётся admission error с прежней
   диагностикой; inspector не вызывает diagnostic writers, не создаёт directories,
   WAL/SHM/markers/locks и не мигрирует. При missing sidecar/невозможности coherent
   no-write чтения выдаёт Unknown/NeedsRecovery, не использует immutable=1 поверх
   active или WAL-bearing DB и не игнорирует committed WAL. Не включать WAL/migration
   на query. Провести settings до watcher/jobs/direct CLI, не читать Config из Db.
3. Обновить legacy adapters и fixtures; проверить отсутствие production fallback
   через callsite audit. Сохранить one-writer/admission/crash recovery.

**Verify:** `cargo test --locked --features cli-full --test store_settings --test
config_runtime --test ipc_server --test admission`; lean equivalent; full CLI,
Clippy/fmt/native CI. Команды с переносом здесь записаны для чтения — запуск одной строкой.
**Checkpoint:** после merge CF2 начать #66. Его bounded pool/snapshot/singleflight
реализуются в собственном PR с новым configuration owner, не ждут CF3–CF14.

## CF3 — v2 merge и reviewable migration

**Depends:** CF1; strict vector-store activation завершается CF7.
**Files:** Modify config/schema/resolve/mod; Create `src/config/migrate.rs`,
`tests/config_migration.rs`; docs/CONFIG и ledger.
**Consumes/Produces:** v1/v2 `SourceLayer` → typed patches → pure `MigrationDraft`
с source diff и scope/version conflicts. CF3 не использует ещё не существующий
`ChangePlan`, не вводит CLI command и не пишет файлы. CLI/state-plan integration —
CF4, принятая запись через consent/apply и atomic file writer — CF5.
**RED/acceptance:** global1/project2 и global2/project1; nested absent/Set/Disable;
bool reset; unknown version/key; differing legacy project vector budgets;
необследованный project; построение MigrationDraft оставляет source files неизменными.
Interrupted atomic config write проверяется CF5, где впервые появляется запись.
V1 и strict-v2 store mode не смешиваются; operator cap decision явный.
**Covers:** FR-4/5/7. **Verify:** migration + config suites, full CLI/native CI.

## CF4 — effective view и pure offline plan

**Depends:** CF1/CF2/CF3. **Files:** Create `src/config/change.rs`,
`tests/config_plan.rs`; Modify config/resolve/migrate, cli/mod; использовать
side-effect-free store inspector из CF2, не operational read-only helpers.
**Consumes:** effective settings, pure MigrationDraft и bounded `ArtifactFacts`;
ArtifactId/ArtifactFacts/ComponentImpact уже объявлены в CF1/schema.
**Produces:** planner algorithms, `ChangePlan{plan_version,plan_id,expected,actions,grants}`,
read-only `config migrate --to 2` source diff + state plan без записи файлов.
**RED/acceptance:** `plan_is_readonly_with_missing_cache`,
`plan_schema_mismatch_does_not_write_conflict`,
`plan_wal_missing_sidecar_is_readonly_or_reports_unavailable`,
`migration_cli_plan_never_writes_sources`,
`plan_never_constructs_provider`, `unknown_provenance_is_not_ready`,
`timeout_change_has_no_artifact_actions`, `action_cycle_is_rejected`,
`effective_output_redacts_secrets`. Compare full filesystem snapshots/SQL version,
включая conflict log/WAL/SHM/directories при schema mismatch; provider constructor/
request counters0. JSON version/units/scopes/origins pinned.
Unimplemented actions видимы как blocked, не успешно применённые.
**Covers:** FR-6/8/9/10/11/12. **Verify:** plan/resolution + offline suites, CLI tests.

## CF5 — consent и неинтерактивный startup

**Depends:** CF4. **Files:** Modify cli/mod, main, error, daemon/lifecycle,
mcp/frontend, config/migrate/paths; Create `tests/config_policy.rs`.
**Consumes:** plan + grants + captured EntrypointFacts; **Produces:** resolved CLI
policy, принятая migration write/apply и new-path exit0/3/4/5.
**RED/acceptance:** `legacy_v1_without_policy_flags_keeps_startup_behavior`,
`auto_direct_cli_with_tty_prompts_before_apply`,
`auto_non_tty_service_mcp_autostart_fail_required_change_with3`,
`explicit_prompt_without_tty_and_required_change_returns5`,
`no_consent_change_initialization_returns0`. Auto разрешается один раз на границе:
только direct CLI+TTY → prompt, остальные entrypoints → fail; explicit flag override
проверяется отдельно. Service/MCP loop никогда не читает consent даже с TTY.
Fail/no-TTY не пишет state; stale4; missing grants5; bare apply не вызывает remote/
download/destructive; runtime restart/cache creation не приравнивается к reindex.
`interrupted_atomic_migration_write_preserves_source` покрывает accepted write path.
Old CLI error code1/`config` output сохраняются. До CF11 apply поддерживает лишь
готовые локальные runtime/resource actions; другие планы блокируются явно.
**Covers:** FR-13/14. **Verify:** subprocess CLI/MCP tests с fake provider/TTY seam.

## CF6 — dense/query resource settings и общий учёт

**Depends:** CF1/CF4/CF5. **Files:** Modify dense_settings, hybrid/dense/jobs/meta,
daemon/server; Test `tests/hybrid_ann.rs`, `tests/search_cost.rs` и новый
`tests/cache_settings.rs`.
**Consumes:** Resolved DenseSettings/QueryCacheSettings; **Produces:** explicit
constructors/update policy для I28–I31/I35; `jobs::ann_byte_cap` получает ту же policy.
**RED/acceptance:** omitted/default и explicit override до cache consumer; query
entry/byte eviction; shrink при gated live build не теряет charged bytes;
project override не создаёт вторую копию global budget; failure retains state.
**Covers:** FR-3/5/16. **Verify:** cache/ANN/cost suites, full+lean/native CI.

## CF7 — один strict vector-store owner

**Depends:** CF3/CF4/CF6. **Files:** Modify vector_cache, hybrid/config/jobs/dense,
config/migrate, daemon/server; Test vector_cache/config_migration/cache_settings.
**Consumes:** migration conflict resolution + global VectorCacheSettings;
**Produces:** один owner/eviction policy, config max_entries/bytes/busy timeout.
**RED/acceptance:** two projects с разными legacy budgets не молча переключают
shared cap; отказ от migration сохраняет v1; strict store не вызывается legacy
evictor; pins/leases соблюдаются; одинаковые text/model vectors shared/reused.
**Covers:** FR-3/4/5/16. **Verify:** real shared vectors.db tests + native CI.

## CF8 — ANN construction/search settings и provenance

**Depends:** CF4/CF6/CF7. **Files:** Create ann_settings;
Modify ann/ann_store/dense; Test ann_recall/hybrid_ann + `tests/ann_settings.rs`.
**Consumes:** AnnBuildSettings/AnnSearchSettings; **Produces:** versioned graph
recipe/meta/key и conservative admission estimator для qualified M/layers domain.
**RED/acceptance:** search ef не rebuild; M/ef_construction rebuild только graph,
старые vectors reused; incompatible/unknown meta не admitted как новый graph;
неподдерживаемый memory-estimator domain отклонён до HNSW constructor;
existing default recall/fallback gates сохранены. Формат меняется deliberate delta.
**Covers:** FR-3/10/11/15. **Verify:** ANN suites + unchanged quality gate.

## CF9 — index/chunk/search settings и relevant identities

**Depends:** CF4/CF8. **Files:** Create `src/index/settings.rs`;
Modify `src/index/job.rs`, `src/index/chunk.rs`, `src/index/tokenizer.rs`,
`src/index/tantivy_index.rs`, `src/index/walk.rs`, `src/daemon/tools.rs`,
`src/hybrid/rrf.rs`, `src/store/repo.rs`, `src/cli/mod.rs`, `src/cli/search.rs`,
`src/mcp/tools.rs`; Test index/config/runtime/search suites + `tests/index_settings.rs`.
**Produces:** versioned parse/chunk/lexical recipes, relevant per-document provenance,
explicit I01–I18/I21/I22/I68 policy; lexical collector widening I68 имеет отдельный
owner/settings от UX post-filter overscan I13. Не начинать одновременно с #67/#68/#87 в tools.
**RED/acceptance:** unchanged source hash + changed recipe не skip; tokenizer-only
change не re-embed; chunk change reuse identical texts; metadata-only parser
change не маскируется как body change; `lexical_widening_policy_preserves_settled_stop`
проверяет initial max(limit×4,limit+16)/widening×4, checked growth и прежнее
settled/exhausted условие без обещания scoped recall; defaults preserve golden results.
**Covers:** FR-1/3/9/11/15. **Verify:** index + all quality slices/native CI.

## CF10 — storage publication ADR перед generation apply

**Depends:** #66 conclusions, CF4/CF8/CF9. Это отдельный design task, не псевдо-код.
**Files:** Create `storage-publication.md` в этой spec; dated deltas design/tasks;
проверить store/migrations/repo, index/job, platform atomic-write behavior.
**Produces:** reviewed schema/path/journal contract и разбиение CF11 на session-sized
prepare/publish/recovery tasks до его начала. Выбрать side-by-side metadata rows
или per-generation metadata snapshot по измерению copy/disk cost на bounded fixture.
Зафиксировать IDs, migration/backups, writer/read leases, Windows open mappings,
fsync/rename ordering, startup recovery и peak budgets. Один manifest над mutable
rows не принимается. До этого #66 может явно reject search при in-place mutation.
**Covers:** FR-15/16. **Verify:** source-backed ADR, failure-point table и fresh review;
прототип только в isolated scratch, не production architecture claim.

## CF11 — prepare/publish/recovery по CF10

**Depends:** CF10 approved/refined tasks. **Files:** exact schema/path список из CF10;
Create `tests/config_apply.rs`, `tests/generation_recovery.rs`; Modify coordinator,
index jobs, store, daemon project snapshots, docs/recovery.
**Produces:** stale-safe generation apply, leases/reclaim и maintenance migration path.
**Required RED cases:** crash до/после каждого durable boundary; source mutation;
reload overtakes gated old build; concurrent query не смешивает old IDs/new text;
live reader удерживает old charged bytes; prepare failure оставляет old state;
disk/RAM peak overflow fail до destructive step. Filesystem/process fixtures на3ОС.
**Covers:** FR-9/10/15/16, SC-4. Нельзя закрыть этот пункт одним зелёным unit mock.

## CF12 — watcher/lifecycle/transport/bootstrap limits

**Depends:** CF2/CF5/CF11; согласовать #79 settings перед новым server flow.
**Files:** Create watch/daemon/ipc settings; Modify watch/mod, lifecycle/session/server,
ipc/client, mcp/frontend/tools, config/paths, limits/vcs, cli/install/status/update.
Owning sessions делятся по watcher, daemon и CLI/bootstrap, каждый с отдельным review.
**RED/acceptance:** I41–I58 overrides до реального consumer; pending watcher batch
drain; disabled watcher + explicit sync; active sessions при shrink; bootstrap file
limit применяется до parse. Windows timeout capability проверяется реальным adapter,
до этого no-op не выдаётся за enforcement. Root confinement/modes не ослабляются.
**Covers:** FR-1/3/6/7/14/16/17. **Verify:** runtime/IPC/watcher/install + native CI.

## CF13 — provider/provisioning settings и retained model state

**Depends:** CF4/CF7/CF11; orchestration согласовать с #81, новые website budgets с #79.
**Files:** Create local_settings; Modify provider/config/local/wire/openrouter/models/
runtime/jobs, typed descriptors/bootstrap. Test hybrid_config/local/openrouter/models,
wire unit tests + `tests/provider_settings.rs`.
**RED/acceptance:** I24–I26/I36–I40/I51/I59–I64/I67 explicit defaults/overrides;
max_length меняет persistent recipe, batch/thread/runtime relocation не удаляют
пригодные vectors; recipe для query/doc различима; dependency env captured once;
model instance eviction не считает freed ещё удерживаемый handle. Download/grants
и retry policy hermetic; no paid calls для verification. Async waits — #81.
**Covers:** FR-2/3/6/7/11/16/17. **Verify:** provider matrix/default/full/native CI.

## CF14 — аудит завершённости #78

**Depends:** все adopted slices и #66/#79/#81 scope contracts, если их consumers
уже существуют. **Files:** inventory/requirements/design/tasks/progress, docs/CONFIG,
все production callsites новых explicit-settings constructors.
**Acceptance:** повторить const/literal/env/dependency scan; каждое оставшееся fixed
значение имеет X-rule; runtime Default/fallback adapters не используются production;
SC-3–SC-5 фактически доказаны. Один source scan не заменяет consumer tests.
**Verify:** полная feature/platform matrix по действующей CI policy; no-write plans,
миграция legacy fixtures, interruption/concurrency/recovery, golden baseline и
индивидуальные resource invariants. Только после этого можно закрыть #78.

## Покрытие и порядок

| Требования | Owning tasks |
|---|---|
| FR-1 | CF0 inventory, CF6–CF9/CF12/CF13 adoption, CF14 audit |
| FR-2/3 | CF1 foundation, CF2/CF6–CF9/CF12/CF13 consumers |
| FR-4/5 | CF1 scoped types, CF3 migration, CF7 owner |
| FR-6/7/8 | CF1 bootstrap, CF3 schema, CF4 view, CF12/CF13 capability |
| FR-9/10/11/12 | CF4 planner, CF8/CF9/CF13 identities, CF10/CF11 publication |
| FR-13/14 | CF5 policy, CF12 lifecycle |
| FR-15/16 | CF6/CF7 resources, CF8–CF11 artifacts, CF13 models |
| FR-17 | CF1 → CF2 → #66; #79/#81 получают те же resolved owners |
| NFR-1/2/3/4 | feature/native gates / golden checks / CF4+CF11 bounded plan / owning consumer tests |
| SC-1/SC-2 | CF0 |
| SC-3/SC-4/SC-5 | consumer tasks / CF5+CF11 / CF14 соответственно |

#76 уже merged; его runtime revision guard сохраняется. #61/#62/#87 и design
#67/#68 могут идти отдельным потоком. Во время search split #66 implementation
#67/#68 не меняет тот же pipeline параллельно. CF9 не откладывает их исправления:
adoption обновляется по фактически merged реализации. #80 не блокирует эту работу.

## Delta 2026-11-09 — границы реализации CF1

- В Modify CF1 добавлен `src/hybrid/config.rs`: owner catalog, optional raw patches
  и structural validation живут рядом с hybrid/provider resolution.
- Db/read-pool/dense overrides в CF1 typed-only; новые file keys остаются rejected
  до реальных consumers CF2/CF6/#66. Scope checks выполняются общим `Resolution`.
- Legacy plain snapshot получает `ProvidedSnapshot`, typed snapshot сохраняет
  provenance. Explicit-path adapters валидируют global до project; infallible
  `with_overrides` не изображается как validated `Resolved`.
- Дополнительный behavioral RED: busy_timeout выше `i32::MAX` milliseconds должен
  отклоняться owner validation до будущего вызова rusqlite, который иначе panic.
- Bootstrap не включает consumer adoption CF13: whitelist capture/validation pure,
  dependency-specific fallback semantics сохранены; credentials не читаются.

## Delta 2026-11-10 — CF2 session tasks

CF2 сохраняет scope typed consumption/read ownership; TOML activation — CF3/CF5,
полный read pool/snapshot/singleflight — #66. New crates: none. Вся поставка —
один PR, #78 остаётся открытым. Runtime FR/SC всего umbrella не объявляются готовыми.

### CF2a — explicit storage constructors and pure schema inspection

Modify `src/store/{mod,settings}.rs`; Create `src/store/inspect.rs` и
`tests/store_settings.rs`; private store tests для controlled WAL wait и invalid
imported snapshots. Обязательные settings до FS, writer/read timeout30s/5s явно,
WAL attempts3/delay100ms от owner. Compatibility adapters используют единый resolver.
RED: отсутствующие explicit APIs; actual PRAGMA override/default, missing/old schema
readonly no migration, invalid settings no files, real lock с injected wait и
точным budget, filesystem snapshot around missing/schema mismatch/WAL sidecar cases.
Inspector Ready относится лишь к schema header identity; WAL/hot journal/race не
маскируются main-only identity. Native formats/invariants не становятся knobs.
Verify: store/config/admission focused + cli-full full suite, lean store/config,
full/lean Clippy/fmt. Fresh task review, Critical/Important fixes via implementer.

### CF2b — production policy propagation and adapter audit

Depends CF2a. Modify только реальные callsites: CLI composition/direct commands,
daemon lifecycle/Shared/jobs, watcher consumer. Добавить explicit with-settings
пути там, где нужно сохранить публичный compatibility wrapper. Shared удерживает
policy и передаёт её каждому child open; consumer не разрешает собственный default.
Обновить fixtures и закрыть T55 Minor M1 при изменении watcher Env wiring: Db должен
уничтожаться до cache TempDir. Проверить actual overrides на initial/child handles,
выполнить source callsite audit (учесть wrappers, не только Db::open grep).
Verify: store/config_runtime/ipc_server/admission/index/watcher + full cli-full,
lean focused; Clippy/fmt, fresh task review, fresh final whole-branch review и
required docs/Linux/macOS/Windows CI. Native inventory сохраняет прежние tests.

CF2a file expansion (2026-11-10, ADR-9): Modify
`src/platform/{mod,unix,windows}.rs` для minimal no-follow read-only open;
`store/inspect.rs` вызывает только facade. Existing platform_boundary regression
остаётся без изменений/исключений. Verify platform_boundary и store_settings
без name-filter; private store unit filter выполняется отдельной командой.

## Delta 2026-11-11 — ограниченная реализация CF3

Подробный контракт и шаги: [cf3-plan.md](cf3-plan.md), CF3-1…CF3-5.
Добавить source.rs и hybrid/config/merge.rs для pure versioned inputs и правил
компонента; без runtime v2 activation, CLI/state apply и новых зависимостей.
Прежний пример project-v2 cache заменён override поля стадии: v2 resource
settings остаются global-only. Native/final delivery evidence фиксируется
в PR после task implementation/review ledger.

## Delta 2026-11-12 — CF4a effective view до state planner

Первый самостоятельный deliverable CF4: [cf4a-plan.md](cf4a-plan.md), CF4a-1…3.
Owning values + checked provenance composition + реальная read-only CLI команда.
Не включать пока неподтверждённые artifact/registry readiness в output. Pure
state planner/migration CLI остаются следующим owning task; #78 не закрывается.

## Delta 2026-11-12 — CF4b pure compiler и три реальных owner adapters

Контракт CF4b-1…5 и [cf4b-plan.md](cf4b-plan.md). New files:
`src/config/change.rs`, `src/config/change/compile.rs`, `src/store/plan.rs`,
`tests/config_plan.rs`; owners SearchSpec/DbSpec/MigrationDraft получают pure
assessment methods. Проверить реальные SQLite artifacts, actual validated owner
policies и CF3 drafts наряду с graph invariants. CLI migration/registry/apply
остаются отдельными задачами CF4; общий issue не закрывать.

## Delta 2026-11-12 — CF4c1 actual registry observation

Одна задача по [cf4c1-plan.md](cf4c1-plan.md), CF4c1-1…5. New crates: none.
Новые `src/store/registry_inspect{.rs,/lease.rs,/tests.rs}` и
`tests/registry_inspect.rs`; export в store/mod.rs. SQL rows под native lease,
explicit limits, typed unavailable, no-write/recovery и exact context revision.
Fresh task/final reviews и required native CI обязательны. После этого отдельно
идут actual source observations/migration CLI; #78 целиком остаётся открытым.

## Delta 2026-11-12 — I133 header observer lock integrity

Одна задача по [issue133-plan.md](issue133-plan.md), закрывает #133 в границе
FR-12 и общих state-consistency требований. Existing schema inspector должен
сохранить reader/registry lease и bounded/no-write semantics; public API не
меняется. RED external-writer сценарии, GREEN native protocols и прежние schema/
FIFO/sidecar cases; full/lean checks, fresh task/final review и native inventory.

## Delta 2026-11-12 — CF4c2

- [x] **CF4c2:** Actual bounded source survey and `config migrate --to 2 --format=json`;
  explicit Missing preconditions, safe output, complete persisted registry,
  context-sensitive identity and remaining strict-store/executor blockers.
  Plan: [cf4c2-plan.md](cf4c2-plan.md). No runtime activation/apply.

## Delta 2026-11-13 — CF6a construction-only query policy

CF6 is split for independently reviewable adoption. CF6a consumes I30 at existing
construction boundaries before configurable CF5 apply. This ordering does not
activate new source settings or permit live resource updates. Remaining dense,
resident/backfill and strict-store ownership retains CF6/CF7 dependencies.

- [x] **CF6a:** shared query-cache policy from existing DenseSpec, positive validation,
  explicit daemon/direct CLI propagation, measured eviction/bypass/sharing behavior.
  Plan: [cf6a-plan.md](cf6a-plan.md). Rust 1.100.0 stable / updated main 9d12a72.

## Delta 2026-11-13 — CF6b construction-only dense policy

- [x] **CF6b:** project existing DenseSpec defaults into actual dense construction;
  propagate daemon/direct CLI policy through real sync/backfill, share static
  builder/pin eligibility, preserve live worker accounting and legacy vector
  budget precedence. Explicitly correct small-corpus pins and duplicate-row pin
  retention. Plan: [cf6b-plan.md](cf6b-plan.md). No live shrink or CF7 activation.


## Delta 2026-11-13 — CF6c: внедрение I35 при создании

- [x] **CF6c:** единая проверенная policy для первого/повторного backfill pin и
  актуальности ANN outcome; сохранить compatibility/default windows и настоящую
  daemon/direct передачу. Реальные lease/status regressions, transactional
  patch/provenance/scope tests. План: [cf6c-plan.md](cf6c-plan.md).
  Нет новых source keys/live updates, CF7 activation или artifact invalidation.


## Delta 2026-11-13 — CF7a: construction-only vector-store owner

- [x] **CF7a:** pure VectorStoreSpec, native legacy/strict exclusion и удержание
  lifetime дочерними connections; общая policy/recency, transactional strict eviction,
  настоящие SQLite/OS/subprocess regressions. План: [cf7a-plan.md](cf7a-plan.md).
  Полный CF7 не завершён: production consumer rollout, persisted adoption,
  consent/apply/quiescence и live resource changes выполняются отдельно.

## Delta 2026-11-13 — CF7b: consumers до startup adoption

- [x] **CF7b:** explicit VectorCacheSource, owner-backed DenseCache и реальные
  backfill/coverage/search consumers, strict project-budget refusal до publication.
  План [cf7b-plan.md](cf7b-plan.md). Без strict startup/file-v2/apply activation;
  production sync/status composition остаётся следующим этапом CF5/CF7.

## Delta 2026-11-13 — CF7c1: source-aware index jobs

- [x] **CF7c1:** borrowed IndexEmbedding; реальные full/incremental prepared APIs
  с общим core, owner admission до mutations, stale-gated recovery и retry markers.
  План [cf7c1-plan.md](cf7c1-plan.md). Additive embedded path; daemon/sync/watcher
  composition следует CF7c2, strict startup/file activation остаётся CF5/CF7.

## Delta 2026-11-13 — CF7c2: coherent sync/index/status consumers

- [x] **CF7c2:** prepared config/stages/source для project index, sync/backfill и
  child Db; actual daemon/CLI propagation и source-aware configured coverage.
  План [cf7c2-plan.md](cf7c2-plan.md). Runtime stage admission до enqueue; provider
  preparation без shared DB guard. Watcher reload/consumer ownership — CF7c3,
  strict startup и persisted migration по-прежнему CF5/CF7.

## Delta 2026-11-13 — CF7c3: prepared watcher consumers

- [x] **CF7c3:** единый WatchRuntime для producer/consumer/retry/recovery;
  root admission, actual Shared ensure/reload и отсутствие stale creation.
  План [cf7c3-plan.md](cf7c3-plan.md). Configured APIs сохраняют прежний режим;
  strict startup/adoption/apply и оставшиеся CF5/CF7 задачи ещё не завершены.

## Delta 2026-11-13 — CF5a before operational policy/apply

- [x] **CF5a:** pure controlling-boundary resolution, explicit grants, immutable
  canonical consent review, prompt answers and stale receipts. Plan:
  [cf5a-plan.md](cf5a-plan.md). Actual SearchSpec/DbSpec/migration assessments;
  launch/TTY/refusal/limit/mutation evidence, full/lean checks, fresh task/final
  review and native inventory. CLI integration, source writes, strict adoption
  and execution readiness remain separate CF5b/CF7/CF11 work; #78 stays open.

## Delta 2026-11-13 — CF5b0 before source/store mutation

- [x] **CF5b0:** source-backed adoption/recovery investigation in
  [adoption-recovery.md](adoption-recovery.md), current source map, primary platform
  references, stopped-maintenance/SQLite-journal choice, failure/namespace/secret/
  resource cases and explicit subsequent proof gates. Docs-only source/link audit,
  fresh task/final review and required CI; no runtime tests for prose.
- **CF5b1:** exact SQL/control-state/namespace/lease contract and isolated native
  prototype before activation; details and acceptance cases in investigation.
- **CF5b2:** native source publication/durability and failure inspection seam.
- **CF5b3/CF5c:** stopped migration/recovery executor, then coherent CLI/startup;
  providers/reindex/online generations remain outside initial local adoption.
