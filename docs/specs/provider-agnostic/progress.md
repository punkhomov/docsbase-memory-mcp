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
- Ruling: `ModelAddress::parse` — чистая синтаксическая проверка; подсказка
  «did you mean `fastembed/<name>`» строится в CLI по каталогу (T4), поэтому
  парсер не зависит от реестра моделей.
