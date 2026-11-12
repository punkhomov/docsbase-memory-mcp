# CF4a — versioned effective source view Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans; выполнять последовательно, свежий reviewer после task. Implementation subagents не используются.

**Goal:** добавить рабочую `docsbase config effective --format=json`, показывающую значения v1 source resolution, их units/scope/origins и build capabilities без probing providers или изменения состояния.

**Architecture:** каждый существующий owner формирует значения своих полей; общий builder соединяет их с существующим Provenance и проверяет полноту/дубликаты. Provider registry владеет безопасной проекцией своих records и сведениями о feature. Новый view явно относится к текущим исходникам, а не live snapshot демона. Старый `docsbase config` сохраняется.

**Tech Stack:** Rust1.88, существующие serde/serde_json/clap; New crates: none.

**Spec:** requirements.md FR-6/8/12, design.md, tasks.md CF4 и dated CF4a delta; остальные части CF4 не считаются завершёнными.

## Global Constraints

- Rust1.88/default/cli-full, Linux/macOS/Windows.
- No production unwrap/expect/panic; no new crates; no baseline/threshold changes.
- Никаких inference/downloads, provider status/build/provisioning, IPC/autostart или открытия БД в effective path.
- Source-resolution view не подтверждает активный daemon snapshot или пригодность индексов.
- Scope PR: source view и CLI; ChangePlan/registry observation/apply следуют отдельно.
- #128: использовать существующий Provider registry; не вводить вторую таблицу моделей/возможностей.

## CF4a — один законченный task

**Files:** Create src/config/view.rs, src/hybrid/config/view.rs, src/cli/config.rs, tests/config_view.rs. Modify config/{mod,schema}.rs, search/settings.rs, hybrid/provider.rs, cli/mod.rs; specs/docs/ledger.

**Consumes:** Resolved<Config> из существующего v1 resolver, FieldOrigin/Scope/Unit и существующий provider registry. Defaults повторно не выбираются.

**Produces:** EffectiveView с view_version=1, basis=source_resolution, live_runtime_observed=false, provisioning_checked=false, fields в стабильном key order. Каждое поле содержит key, typed JSON value, source, scope, unit и optional provider capability (provider/required_feature/compiled). Capability означает только compiled support. Никакой credential environment не читается.

ProviderRecord не сериализуется целиком: provider-owned безопасная проекция содержит enabled и известные non-secret параметры; unknown/unexposed parameters не выводятся, их наличие обозначается boolean. FastEmbed показывает runtime/runtime_path из typed record, OpenRouter — enabled. Новый registry provider по умолчанию не раскрывает произвольные params. Это preparation для единого model layer, не новый registry.

Builder отклоняет undeclared/duplicate/missing поля; SourceId::ProvidedSnapshot остаётся явно таким, не подменяется предполагаемым filename. Значение null у отключённой stage означает семантическое отсутствие. V2 source-preview helper использует уже установленный daemon-scoped vector key, но CLI остаётся runtime-compatible v1 loader и не активирует v2.

CLI: Config становится optional subcommand, bare config вызывает прежний show_config. effective поддерживает только format=json (default), неизвестный format отклоняется Clap. Путь проекта — current directory, как раньше. Ошибки нового пути сохраняют категорию resolution/view и не выводят сырые TOML values или error source chain. Старые diagnostics не меняются.

- [x] RED pure tests: real global/project resolver, defaults, replacement, full field inventory, typed values, units/scope/origins, ProvidedSnapshot, stage disabled/capability, safe provider projection и v2 preview provenance.
- [x] GREEN owner values + checked builder + versioned serialization; без generic unused settings framework или изменения runtime consumers.
- [x] RED/GREEN реальные CLI tests: bare config compatibility, effective JSON, unavailable provisioning не вызывает probe, malformed secret-bearing TOML не раскрывается, missing cache/config/data остаются missing, полный filesystem snapshot unchanged.
- [x] Проверить full/lean config/view/provider и CLI suites, all-target Clippy/fmt; полный cli-full suite, frozen gate; обновить docs/specs/ledger.
- [ ] Fresh task review/fix, task commit; fresh final review; required native CI inventory audit и разрешённый squash merge.

Бюджеты planner/DAG, state inspection и полный `config migrate --to2` остаются
CF4b+; причины и unresolved boundaries сохранены в cf4-investigation.md.
