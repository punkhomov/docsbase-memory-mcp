# provider-agnostic — progress ledger

Спека: [`requirements.md`](requirements.md) · [`design.md`](design.md) ·
[`tasks.md`](tasks.md) · ADR-18 в `docs/specs/docsbase-memory-mcp/design.md`.

## 2026-10-29 — спека (до реализации)

- Исследование по GitNexus (индекс пересобран с `--embeddings --pdg --index-only`):
  четыре шва подтверждены (`from_config` 3 прямых вызывающих, `provider_status` 5,
  PDG по `from_config` показывает ровно `has_openrouter`/`has_fastembed`-ветки),
  в `src/cli/models.rs` 59 cfg-сайтов в 47 функциях, из них ветвление — в
  `list_cmd`/`resolve_provider`/`print_summary`/`resolve_dims`/пробах.
- Спека v1 (requirements + design + tasks) написана; ADR-18 добавлен в мастер-дизайн.
- **Spec review (reviewer-muse, opencode-go/muse-spark-1.3-contributor): BLOCK**,
  6 P1 + 3 P2/P3. Разбор и решения:
  - P1-1 (ядро не может вернуть CLI-трейт провижинера — цикл слоёв):
    **принято**. Провижининг целиком в CLI (`provisioner_for(name)` — вторая
    локальная таблица), ядро о нём не знает; дрейф двух таблиц ловит тест.
  - P1-2 (`catalog(Task)` не компилируется без `hybrid-local`: `Task` в гейченном
    `models`): **принято**. `Task`/`CatalogModel` переезжают в негейченный
    `hybrid::provider`, `models` реэкспортирует.
  - P1-3 (гейт при парсинге сломал бы дефолтную сборку: `[providers.fastembed]`,
    записанный сборкой с фичей, отвергался бы как unknown): **принято,
    ключевое**. Таблица негейченная; фича ограничивает использование
    (`build_stages` → `feature_missing_*`), а не существование провайдера.
    Правило «парсинг никогда не смотрит на фичи» вынесено в FR-4.
  - P1-4 («дешёвый клиент» неверно: `ReqwestTransport` = `reqwest::Client` +
    `tokio::Runtime`): **принято**. `build_embedder`+`build_reranker` заменены
    одним `build_stages`, клиент разделяется внутри провайдера — одно создание
    на конфиг, как сегодня.
  - P1-5 (`#[cfg(test)]`-seam не виден спавнящемуся бинарю): **принято**.
    FR-10 переформулирован на внутрипроцессное доказательство через параметры
    (`from_config_with(table, …)`, `connect_frame(&dyn Provisioner, …)`);
    `#[cfg(test)]`-глобалы из дизайна убраны.
  - P1-6 (FR-11 «byte-identical» противоречит §4–§5): **принято**. FR-11 сужен до
    реально закреплённого (значения cfg, `model_id`, тексты фичевого гейта,
    поведение поиска) + явный список того, что меняется и какие фикстуры правятся.
  - P2-7 (семантика флагов `connect` у openrouter не определена): **принято**,
    таблица флагов в design §6 и FR-8.
  - P2-8 (не описан срез адреса при записи в конфиг): **принято**, новый FR-12
    (`provider` + остаток; полный адрес в `model` испортил бы `model_id`).
  - P3-9 (два поставщика `record()`): **принято**, `Provider::record()` удалён.
  - P3-10 (смена кода ошибки для незнакомого ключа не объявлена): **принято**,
    FR-4 объявляет `Admission` вместо `Internal` и отказ для
    `[providers.openrouter] runtime = "…"`.
- Ruling: таблица провайдеров — данные без гейта; build-фича ограничивает
  *использование* провайдера, никогда не его существование в конфиге/статусе.
- Ruling: провижининг — CLI-забота; ядро (`hybrid::provider`) не импортирует
  `cli` и не тянет `ureq`/`zip` в дефолтную сборку.
- Ruling: расширяемость доказывается параметрами (таблица, провижинер), а не
  тестовыми глобалами — глобалы не существуют в спавнящемся бинаре.

## T1 — реестр провайдеров и адрес модели

- `Task <T1>: complete (ed8d7f0..)`. Добавлен `src/hybrid/provider.rs`:
  `ModelAddress` (провайдер — до первого `/`, модель — остаток),
  `Task`/`CatalogModel` (переехали из `models`, `models` реэкспортирует),
  `trait Provider` (`name`/`feature`/`status`/`resolve_embedding`/`resolve_reranker`/
  `catalog`), негейченная таблица `REGISTRY` (`providers()`, `provider()`,
  `provider_names()`), `feature_enabled()`.
- Тесты: `model_address_splits_at_the_first_slash`,
  `model_address_rejects_bare_and_malformed_input`,
  `registry_lists_every_provider_with_its_feature`, `unknown_provider_lists_the_registry`,
  `feature_enabled_matches_the_build`.
- Проверка: default lib 128 passed, combo lib 221 passed, clippy (default и
  `hybrid-local`) чисто, fmt чисто. Поведение не изменилось — `from_config`,
  `provider_status` и CLI пока работают по-старому (T2/T4).
- Ruling: `ModelAddress::parse` проверяет синтаксис и **имя провайдера против
  таблицы** (параметр `table` в `parse_with`, глобальная таблица в `parse`);
  подсказка «did you mean `fastembed/<name>`» строится в CLI по каталогу (T4),
  поэтому от реестра *моделей* парсер не зависит.

### T1 fix round (reviewer-deepseek, APPROVED WITH NOTES)

- P1-1 (`parse` закрыт на глобальный `REGISTRY` → тест третьего провайдера не
  мог пройти через параметр): **исправлено**. `parse_with(table, raw)`,
  `provider_in(table, name)`, `provider_names_in(table)`; `parse`/`provider` —
  обёртки над глобальной таблицей. Тест
  `parse_with_accepts_a_provider_absent_from_the_global_registry`.
- P1-2 (делегирующие `resolve_*` не воспроизводили поведение вызывающих:
  `dims: None` у openrouter молча давал 2048 вместо требования `--dims`,
  `normalize` был захардкожен `true`, границы окна реранка жили в CLI):
  **исправлено**. Сигнатуры расширены до `resolve_embedding(model: Option<&str>,
  dims: Option<u32>, normalize: bool)` и `resolve_reranker(model: Option<&str>,
  window: u32)`; перенос границ `MIN..=MAX` в реализацию провайдера — задача T4
  (там же удаляется `resolve_window`), записано в `tasks.md`.
- P2-3 (`CatalogModel` — второй каталог, `dims: usize` не выражает rerank-строку):
  **исправлено удалением**. `CatalogModel`/`Provider::catalog` появятся в T4 как
  проекция `models::REGISTRY` с тестом паритета (имя/задача/dims/revision), а не
  как ручная копия.
- P2-4 (`feature_enabled` молча возвращал `false` для любой не захардкоженной
  фичи → новый провайдер был бы неработоспособен в любой сборке):
  **исправлено**. Вместо общей функции — `Provider::enabled()` с `cfg!`-литералом
  в модуле провайдера; `feature_enabled` удалён. Тест
  `provider_enabled_follows_its_own_feature`.
- P3-5 (`Task::label` дублировал CLI-шный `task_label`): удалён.
- P3-6 (парсер не триммил, хотя CLI триммит): `parse_with` триммит вход; тест
  `model_address_trims_surrounding_whitespace`.
- P3-7 (в admission про неизвестного провайдера не было примера формы): добавлено
  `write "<provider>/<model>"`, проверено в тесте.
- P3-8/P3-9 (неточности ledger и устаревшие ссылки на строки в дизайне):
  исправлены — `CatalogModel` никогда не жил в `models`; `pub mod models` теперь
  строка 25.
- Verify после правок: `--lib` default 130, `hybrid-local` 209, `hybrid-remote`
  144, combo 223; clippy `--all-targets -D warnings` чисто во всех четырёх;
  fmt чисто.

## T2 — `from_config` и `provider_status` через таблицу

- `Task <T2>: complete`. `from_config(cfg, providers)` → `from_config_with(table,
  cfg, providers)`: таблица — параметр (FR-10), внутри один `build_stages` на
  провайдера, клиент `OpenRouter` по-прежнему создаётся один раз на конфиг.
  `provider_status` = обёртка над `Provider::status` (неизвестное имя остаётся
  диагностикой `Unknown` с причиной, а не внутренней ошибкой).
  Тела `provider_status` переехали в реализации провайдеров; `validate_stage_shapes`
  проверяет принадлежность по реестру, сохраняя прежние тексты
  (`hybrid.embedding: unknown provider "x" (supported: …)`).
- Проверка: lib default 130 / local 209 / remote 144 / combo 223; `config` 22,
  `hybrid_config` 13 (combo) и 7 (default), `cli_models` 23, `connect` 7,
  `hybrid_local` 3, `hybrid_models` 4, `hybrid_openrouter` 20; clippy
  `--all-targets -D warnings` чисто (default и combo); fmt чисто.
- Ruling: гейт фичи стал **compile-time** (`Provider::enabled()`), поэтому
  параметры `remote_on`/`local_on` у `from_config` удалены — все 8 вызывающих
  передавали туда `cfg!(…)`, то есть дублировали одно и то же знание. Тесты,
  которым нужен «выключенный» провайдер, используют `from_config_with` с
  таблицей, где провайдер объявлен как feature-disabled (`DisabledProvider` в
  `tests/hybrid_config.rs`), а настоящий текст сообщения проверяется прямым
  вызовом `Provider::feature_missing()` (реальные «feature off»-сборки гоняются
  отдельными CI-лейнами).
- minor (deferred): `resolve_embedding`/`resolve_reranker` пока остаются
  свободными функциями в `config.rs` с `match provider` — их тела переезжают в
  провайдеров в T4 (там же `resolve_window` и границы окна), до тех пор
  `rg`-гейт T6 не применяется.

### T2 fix round (reviewer-deepseek, APPROVED WITH NOTES)

- P2-1 (`feature_off_admission_is_explicit` не ловил удаление гейта: билдер
  фейкового провайдера сам возвращал тот же Admission): **исправлено**.
  `DisabledProvider::build_stages` → `Ok((None, None))`, поэтому отказать может
  только гейт; возвращена ветка `#[cfg(not(feature = "hybrid-remote"))]`, где
  **реальный** реестр отвергает OpenRouter-стадию в сборке без фичи.
- P2-2 (заявленные в tasks.md golden-тесты T2 отсутствовали): **добавлены**
  `provider_status_golden_unchanged` (состояние + reason-фрагмент для обоих
  провайдеров, включая Broken и оба Unknown) и
  `feature_missing_messages_name_the_declared_feature`.
- P2-3 (`provider_status` светил `admission refused:` в пользовательский
  reason): **исправлено** — `err.inner_message()`; тест golden проверяет
  отсутствие префикса.
- P2-4 (док `from_config` описывал удалённый параметр + инертный
  `cfg_attr(allow(unused_variables))`): удалено.
- P2-5 (design §3 всё ещё показывал старую сигнатуру и гейт по `feature()`):
  добавлена дельта 2026-10-29 (T2).
- P3-6 (изменился приоритет ошибок: форма конфига теперь раньше гейта фичи):
  **объявлено** дельтой в design §3; побочно снят `#[cfg(feature =
  "hybrid-remote")]` с `unknown_provider_is_rejected_before_any_network_setup` —
  теперь это покрытие есть и в дефолтном лейне.
- P3-7 (`feature()` без продуктивного потребителя): добавлен тест согласия
  `feature_missing_messages_name_the_declared_feature`; сам метод оставлен как
  объявленные метаданные (FR-5).
- P3-8 (неверные цифры в ledger): исправлено — `from_config` имел 19 вызовов
  (5 в `src/`, 14 в тестах), и в дефолтном лейне реальный гейт теперь снова
  проверяется.

## T3 — generic-записи провайдеров в конфиге

- `Task <T3>: complete`. `[providers.<name>]` больше не типизирован в
  `src/config/mod.rs`: таблица читается как `toml::Table`, имя проверяется по
  реестру, ключи — реализацией провайдера (`Provider::parse_record`).
  `ProvidersCfg` стал `BTreeMap<String, ProviderRecord>` (`get`/`iter`/`insert`/
  `with`), `ProviderRecord { enabled, params }`; типизированные вьюхи —
  `FastembedProvider`/`OpenrouterProvider` с `to_record`/`from_record` и
  геттерами `fastembed_record`/`openrouter_record`.
- Новые тесты: `unknown_key_inside_a_provider_record_is_an_admission`
  (опечатка `rutime_path` → Admission со списком допустимых ключей, было
  `Internal` из serde), `provider_record_of_a_feature_disabled_provider_still_parses`
  (файл, записанный сборкой с `hybrid-local`, читается дефолтной сборкой),
  плюс ужесточён `unknown_provider_name_rejected_at_global` (текст обязан
  перечислять имена из реестра).
- Проверка: lib 132 / 211 / 146 / 225 по четырём конфигурациям; `config` 24,
  `hybrid_config` 8 (default) и 13 (combo), `cli_models` 23, `connect` 7,
  `hybrid_local` 3, `hybrid_models` 4, `hybrid_openrouter` 20; clippy
  `--all-targets -D warnings` чист (default и combo); fmt чист.
- Ruling: `ProviderRecord.params` — `toml::Table`, поэтому `Config` и
  `ProvidersCfg` больше не `Eq` (только `PartialEq`); ничего в продукте `Eq` не
  требовало.
- minor (deferred): `write_global_provider` в `cli/connect.rs` по-прежнему
  собирает TOML руками из пар ключ-значение; унификация записи записи — T5.

## T4 — адрес `<provider>/<model>` в CLI

- `Task <T4>: complete`. `models use embed|rerank`, `models fetch`, `models remove`
  принимают только адрес: `ModelAddress::parse` + подсказка по каталогу для
  голого имени (`"bge-small-en-v1.5" is not a model address; use
  "fastembed/bge-small-en-v1.5"`). `--provider` удалён.
- Тела `resolve_embedding`/`resolve_reranker` переехали из `config.rs` в
  реализации провайдеров (тексты сохранены дословно); свободные функции стали
  обёртками над реестром. Границы окна реранка остались у CLI-флага
  (`resolve_window`), а провайдер валидирует свою стадию для конфиг-пути.
- Каталог: `CatalogModel { name, task, dims: Option<usize>, revision }` +
  `Provider::catalog()`; fastembed отдаёт **проекцию** `models::REGISTRY` через
  `OnceLock` + `Box::leak`, паритет закреплён тестом `catalog_matches_the_registry`.
- Вывод: `models list` печатает строки каталога адресами
  (`fastembed/bge-small-en-v1.5`), `providers:` — циклом по реестру,
  `selected:` — адресом из конфига (`fastembed/bge-small-en-v1.5`,
  `…@w50` для реранка).
- `fetch`/`remove` для провайдера без локального кэша → Admission «no local model
  cache; models are downloaded by the provider» (NG3).
- FR-12 закреплён структурно: `use_cmd` строит конфиг из `EmbeddingCfg`/
  `RerankerCfg` (provider + голое имя), полный адрес в `model` невозможен.
- Тесты: юнит-набор `cli::models` переписан на адреса (адрес → dims из каталога,
  голое имя → подсказка, провайдер без каталога → обязательный `--dims`,
  `openrouter/some/model` сохраняет слэши модели); интеграционные —
  `bare_model_name_is_rejected_with_the_corrected_address`,
  `unknown_provider_in_an_address_is_admission`; удалены два теста про
  несуществующий `--provider`.
- Проверка: lib 132 / 213 / 146 / 227; `config` 24, `hybrid_config` 8+13,
  `cli_models` 22, `connect` 7, `hybrid_local` 3, `hybrid_models` 4,
  `hybrid_openrouter` 20; clippy `-D warnings` чист в default и combo; fmt чист.
- Ruling: голое имя больше не выводит провайдера **нигде** — в том числе в
  `fetch`/`remove`; единственный источник провайдера — первый сегмент адреса.
