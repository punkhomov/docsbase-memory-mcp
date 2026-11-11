# CF3 — preview версий исходников и pure migration draft

Дата: 2026-11-11. Issue #78. Последовательная реализация существующей CF3.
New crates: none. Rust1.88; production unwrap/expect/panic запрещены;
v1 runtime, ranking/recordings/baselines сохраняются. Отдельный worktree.

## Ограниченный контракт реализации

CF3 — pure source planning API. Обычные Config::load/global/for_project
сохраняют v1; config_version=2 не является operational форматом до CF5/CF7
consent и strict vector-store ownership. Preview сообщает эту границу явно.
Нет CLI command, записи файлов, provider construction, registry discovery
или artifact apply.

- SourceLayer разбирает ограниченный supplied TOML с явным Global/Project path
  и per-file version (absent/1 legacy, 2 deep merge; прочие запрещены).
  Повторно использует существующие raw base/search/provider validation.
  Project не задаёт providers/search/vector_cache. Unknown keys/versions
  отклоняются; error/debug не выводят исходный текст и значения источника.
- V2 hybrid принадлежит hybrid owner. Bool resets используют owner catalog v1.
  Таблица наследует отсутствующие поля, включая enabled. Stage table включает/
  обновляет стадию, false отключает, true запрещён (включение через empty table).
  У существующей стадии provider/model/dimensions наследуются независимо;
  смена provider не подменяет неуказанные model/dimensions. Новая стадия
  получает owner/provider defaults при resolution. Provenance различает
  inherited/supplied поля и stage enable/disable/reset.
- V2 resource spelling — global vector_cache.max_bytes. V2 hybrid.cache
  запрещён: пример ADR-CFG-2 с project-v2 cache противоречил global owner;
  dated delta заменяет его на reranker.window. Mixed global1/project2 и
  global2/project1 сохраняют per-version hybrid semantics. Explicit legacy
  project cache override под global2 виден как scope conflict, а не принятие
  mixed runtime store modes.
- Preview разрешает settings/provenance без runtime construction; сообщает
  legacy budget до strict-global projection и scope conflicts. Все preview
  остаются non-operational до соответствующего adoption.
- MigrationDraft получает explicit registered config paths и observations:
  loaded либо observed missing. Uninspected/duplicate/unregistered entries
  не маскируются под missing. Необследованные проекты блокируют полный draft.
  Max наблюдаемых budgets только предлагается; полный draft требует явного
  UseBytes/AcceptSuggestedMaximum. Для global1 учитываются effective legacy
  budgets; для global2 — его cap и explicit legacy conflicts, без увеличения
  до неиспользуемого default после bool/table reset.
- Source edits содержат original text/hash и proposed canonical TOML.
  Потеря comments/formatting указана явно; запись отсутствует. V1 hybrid tables
  materialize текущие replacement values, включая отсутствующие stages=false.
  Bool остаётся bool, v2 patches сохраняют inheritance. Legacy cache переносится
  в один selected global cap; missing project files остаются missing.
  Каждый proposed source повторно parse/resolve; non-resource settings должны
  совпадать для global и всех inspected projects. Budget changes перечислены
  явно. Нет artifact readiness/apply claim: CF4 потребляет draft, CF5/CF7 apply.

## Шаги

- [x] RED: mixed-layer hybrid semantics, nested inherit/set/disable, bool reset,
  unknown version/key/scope rejection и provenance через настоящий pure API.
- [x] Hybrid-owned v2 patch/overlay и versioned source preview через
  ConfigSpec/Resolution owner catalog без изменения обычных v1 loaders.
- [x] RED/GREEN migration: complete/partial inventory, budgets/explicit choice,
  table materialization, mixed roundtrip, missing file, source/hash/warning,
  filesystem snapshot (включая directories) до/после, redacted diagnostics.
- [x] Dated specs/docs; feature-off/config и v1 CLI compatibility, полный suite,
  fmt/Clippy, fresh task review/fixes, ledger и commit.
- [ ] Fresh final review, required native CI inventory audit, guarded squash
  merge (явное разрешение пользователя). #78 остаётся открыта для следующих slices.

Файлы: src/config/{mod,resolve,schema}.rs (minimal composition), новые
src/config/{source,migrate}.rs; src/hybrid/config.rs и hybrid/config/merge.rs;
src/vector_cache.rs (owner descriptor без runtime changes);
tests/config_migration.rs; owner specs и docs/CONFIG. Нет изменений daemon/server/
IPC/web implementation, пока PR126 находится в работе.
