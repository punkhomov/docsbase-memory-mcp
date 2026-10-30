# provider-agnostic — задачи

Дата: 2026-10-29. Требования: [`requirements.md`](requirements.md),
дизайн: [`design.md`](design.md), ADR-18.

Правила: одна задача за раз, TDD (RED → GREEN), свежий ревьюер на задачу,
строка `Task <ID>: complete (…)` + `Ruling:`/`minor` в `progress.md` с каждым
коммитом. `RUSTUP_TOOLCHAIN=1.88.0`, `CARGO_BUILD_JOBS=8`, `--locked`.

Базовые команды проверки (сокращения, используемые ниже):

- `V1` = `cargo test --locked --lib` (default)
- `V2` = `cargo test --locked --features hybrid-local,hybrid-remote --lib`
- `V3` = `cargo test --locked --features hybrid-local --test cli_models --test hybrid_config --test connect --test config --test hybrid_local --test hybrid_models`
- `V4` = `cargo test --locked --features hybrid-remote --test hybrid_config --test config --test hybrid_openrouter`
- `V5` = `cargo clippy --locked --all-targets -- -D warnings` (default и обе фичи) + `cargo fmt --check`

## T1. Реестр провайдеров и адрес модели (без изменения поведения)

**Цель.** Появляется `src/hybrid/provider.rs`: `ModelAddress`, `Task` +
`CatalogModel` (переезжают из `models`, чтобы трейт не зависел от гейченного
модуля), `trait Provider`, **негейченная** таблица `providers()`,
`provider(name)`, `feature_enabled`. Оба существующих провайдера реализуют
трейт **делегированием** текущему коду. `from_config`, `provider_status`, CLI не
трогаются — поведение идентично.

**RED.**
- `model_address_parses_provider_prefix`: `openrouter/nvidia/nemotron-3-embed-1b:free`
  → (`openrouter`, `nvidia/nemotron-3-embed-1b:free`); `fastembed/bge-small-en-v1.5`
  → (`fastembed`, `bge-small-en-v1.5`).
- `model_address_rejects_empty_and_unknown`: `""`, `"/x"`, `"openrouter/"`,
  `"nope/x"` → `Error::Admission`, текст содержит список провайдеров и пример.
- `registry_lists_both_providers_with_features`: `providers()` содержит
  `openrouter` (feature `hybrid-remote`) и `fastembed` (feature `hybrid-local`);
  `provider("openrouter")` = тот же элемент.
- `registry_is_ungated`: таблица содержит обоих провайдеров и в дефолтной
  сборке (без фич) — иначе дефолтный бинарь отверг бы валидный
  `[providers.fastembed]` (ревью muse P1-3).
- `task_and_catalog_model_compile_without_local_feature`: `Task` доступен без
  `hybrid-local` (ревью muse P1-2).
- `feature_enabled_matches_build`: `feature_enabled("hybrid-local")` совпадает с
  `cfg!(feature = "hybrid-local")` для обеих фич.

**GREEN.** Новый модуль + две реализации трейта (тонкие адаптеры над
`openrouter::*` и `local::*`); `hybrid/mod.rs` реэкспортирует; `models`
реэкспортирует `Task` для совместимости импортов.

**Verify.** V1, V2, V3, V4, V5 — без правок существующих тестов.

**Готово, когда** новые тесты зелёные, а диффа в поведении нет (существующие
тесты не менялись).

## T2. `from_config` и `provider_status` через таблицу

**Цель.** Убрать `match name` из `src/hybrid/config.rs` (FR-6, FR-7).

**RED.**
- `from_config_rejects_provider_absent_from_registry` (после T3 — конфиг с
  неизвестным провайдером; на этом шаге — через прямой вызов `Provider`-резолвера).
- `provider_status_golden_unchanged`: таблица (state, path, reason) для обоих
  провайдеров во всех четырёх состояниях — значения те же, что сегодня
  (переписать существующие `provider_status_*` тесты в табличный вид без
  изменения ожиданий).
- `feature_gate_messages_are_byte_identical`: тексты `feature_missing_remote/local`
  не изменились.

**GREEN.** `from_config` = `from_config_with(providers(), …)` → по одному
вызову `build_stages` на провайдера (клиент OpenRouter создаётся один раз на
конфиг, ревью muse P1-4); `provider_status` = `Provider::status`;
`has_openrouter`/`has_fastembed` и `cfg_attr(unused_mut)` уходят.
`resolve_embedding`/`resolve_reranker` переезжают в реализации провайдеров
(сигнатуры публичных функций сохраняются до T4).

**Verify.** V1–V5.

**Готово, когда** в `config.rs` не осталось `PROVIDER_*`-ветвлений, кроме
констант, и все существующие тесты зелёные без правки ожиданий.

## T3. Generic-записи провайдеров в конфиге

**Цель.** FR-3, FR-4: TOML не меняется, внутренности становятся
`BTreeMap<String, ProviderRecord>`; разбор ключей — у провайдера.

**RED.**
- `unknown_provider_in_global_config_is_admission` (существующий тест, ожидание
  текста с полным списком провайдеров из реестра).
- `unknown_key_in_provider_record_names_the_provider`: `[providers.fastembed]`
  с `foo = 1` → Admission, текст содержит `fastembed` и допустимые ключи
  (сегодня — `Internal` из serde; смена кода ошибки объявлена в FR-4).
- `foreign_provider_record_parses_in_default_build`: файл с
  `[providers.fastembed]` читается сборкой без фич (гейт — при сборке стейджа).
- `provider_record_roundtrip_preserves_unknown_provider_error_for_project`:
  `[providers.x]` в проектном файле по-прежнему не парсится (структурная
  защита).
- `fastembed_record_getter_reads_runtime_and_path`: типизированный геттер
  отдаёт те же значения, что раньше отдавали поля.

**GREEN.** `ProviderFile` → `toml::Table`; `Provider::parse_record` у обоих
провайдеров; `ProvidersCfg::get/iter`; `runtime::resolve`, `cli/connect.rs`,
`cli/models.rs` читают через геттеры.

**Verify.** V1–V5 + `cargo test --locked --features hybrid-local --lib` (runtime
и models-тесты).

**Готово, когда** добавление записи нового провайдера в конфиг не требует правок
`src/config/mod.rs`.

## T4. CLI: адрес `<provider>/<model>` во всех командах моделей

**Цель.** FR-1, FR-2: голых имён нет.

**RED.**
- `bare_model_name_is_rejected_with_corrected_command`
  (`models use embed bge-small-en-v1.5` → Admission, текст содержит
  `fastembed/bge-small-en-v1.5`).
- `addressed_local_model_resolves_dims_from_catalog`
  (`models use embed fastembed/bge-small-en-v1.5 --dry-run`).
- `addressed_remote_model_requires_dims`
  (`models use embed openrouter/<id>` без `--dims` → Admission; с `--dims` — ок).
- `fetch_and_remove_require_an_address`; `fetch_for_provider_without_cache_is_admission`
  (`models fetch openrouter/<id>` → «no local model cache»).
- `selected_line_prints_addresses` (`models list` → `selected: embed=fastembed/...`).

**GREEN.** `ModelAddress::parse` в CLI (с подсказкой по каталогу при голом
имени); `resolve_provider` удалён; `resolve_dims` → `Provider::resolve_embedding`;
`resolve_window` удалён, границы `MIN_RERANK_WINDOW..=MAX_RERANK_WINDOW`
переезжают в реализацию `resolve_reranker` (один владелец диапазона, ревью T1
P1-2), `validate_stage_shapes` переиспользует её вместо своей копии.
Каталог для `models list`/`use` — проекция `models::REGISTRY` внутри
fastembed-провайдера (`OnceLock<Vec<CatalogModel>>`) + тест паритета.

**Verify.** V3 (обновлённые тесты — единственные, где меняется ожидание), V1, V4.

**Готово, когда** `rg 'resolve_provider' src/` пусто и все CLI-тесты зелёные.

## T5. Единый `connect` и единый статус

**Цель.** FR-7, FR-8: `connect <provider> [--check] [--force]`, одинаковые
флаги, один каркас; `trait Provisioner` в CLI-слое.

**RED.**
- `connect_unknown_provider_is_admission_with_registry_list`.
- `connect_check_and_force_are_mutually_exclusive_for_every_provider`
  (fastembed и openrouter, одинаковый текст).
- `connect_openrouter_check_prints_same_columns_as_fastembed`
  (одна строка таблицы, колонки совпадают).
- `connect_provider_without_provisioning_says_so` (фейковый провайдер из T6 —
  здесь заглушка через `Provider::provisioner() -> None`).
- Существующие `connect`-тесты (7) остаются зелёными; `connect_fastembed_noop_does_not_ask_for_a_restart`
  и `restart_note_only_after_a_real_install` — без изменений.

**GREEN.** `ConnectProvider`/`FastembedArgs` удалены; `provisioner_for(name)`
(CLI-таблица) + `connect_frame(&dyn Provisioner, name, check, force)`; тест
`every_provider_with_provisioning_is_registered` ловит дрейф двух таблиц;
семантика флагов — по таблице FR-8 (openrouter `--check` = probe без записи).

**Verify.** V3, V1, V4 + ручной смоук офлайн-путей
(`connect --status`, `connect fastembed --check`, `connect openrouter --check`).

## T6. Тест расширяемости (FR-10) и гейт по `PROVIDER_*`

**Цель.** Доказать G4 и закрыть приёмку 4.

**RED→GREEN.**
- `third_provider_needs_only_a_registration`: `#[cfg(test)]` провайдер `echo`
  с каталогом из одной модели; тест прогоняет его через
  `from_config_with(&[&ECHO], …)`, `Provider::status`,
  `ModelAddress::parse("echo/tiny")` + `resolve_embedding` и
  `connect_frame(&EchoProvisioner, "echo", true, false)`. Правок в
  `config.rs`/`cli/*` тест не требует — это и есть проверяемое свойство
  (ревью muse P1-5: без `#[cfg(test)]`-глобалов, они не видны спавнящемуся
  бинарю).
- Гейт: `rg -n 'PROVIDER_FASTEMBED|PROVIDER_OPENROUTER' src/` даёт совпадения
  только в `src/hybrid/{provider,local,openrouter}.rs`, в объявлениях констант и
  дефолт-значении провайдера (`src/hybrid/config.rs:18,21,131,155`,
  `src/config/mod.rs:227`), в типизированных вьюхах записей
  (`fastembed_record`/`openrouter_record` в `src/hybrid/config.rs` — это чтение
  конкретной записи, а не диспетчеризация) и в трёх CLI-местах, где имя — это
  **значение**, а не ветвление: CLI-таблица провижинеров
  (`src/cli/connect.rs`), дефолт-провайдер для строки `selected:`
  (`unwrap_or(PROVIDER_OPENROUTER)`, исторический продукт-дефолт) и
  fallback-владелец строки каталога в `models list`. Ни одно из них не является
  `match`/`if` по имени провайдера, и добавление провайдера их не трогает.

**Verify.** V2, V3, гейт-`rg` (в отчёте задачи — вывод команды).

## T7. Документация и breaking-нота

**Цель.** FR-3 (пользовательские файлы не мигрируют), приёмка 1–3.

- `docs/CONFIG.md`: таблица провайдеров, адрес модели, «голых имён нет».
- `README.md` / `README.ru.md`: примеры `models use`/`fetch`/`remove` с адресом.
- `CHANGELOG.md`: секция `Changed` с явным **breaking** для CLI
  (`models fetch bge-…` → `models fetch fastembed/bge-…`); конфиги не меняются.
- ADR-18 уже в мастер-дизайне; добавить в `design.md` §9 ссылку на T6-гейт.

**Verify.** CI-джоба `docs` (гигиена файлов), `rg` по док-примерам: все
команды моделей содержат `/`.

## T8. Финальная сходимость

- Полная матрица: V1, V2, V3, V4, V5, `cargo test --locked` (default),
  `cargo test --locked --features hybrid-local,hybrid-remote`.
- `rg`-гейт T6, проверка `cargo tree` (NFR-1) для дефолтной сборки.
- Прогон `search_bench`-гейта (поведение поиска не должно измениться).
- Ledger: итоговая запись + список отложенных minor'ов.
- PR с описанием: что изменилось в CLI (breaking), что нет (конфиг, model_id),
  как добавлен тест расширяемости.
