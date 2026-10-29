# provider-agnostic — дизайн

Дата: 2026-10-29. Требования: [`requirements.md`](requirements.md). ADR-18.

## 1. Что уже общее и что меняется

Уже общее и остаётся: `trait Embedder` (`src/vector_cache.rs`),
`trait Reranker` (`src/hybrid/mod.rs`), `EmbeddingCfg`/`RerankerCfg`,
`HybridStages`, `dense/ann/rrf/jobs` (не знают о провайдерах вовсе).

Меняется четыре шва:

| Шов | Сегодня | После |
|---|---|---|
| сборка стейджей | `from_config` строит OpenRouter инлайном, local — через `local::embedder` | `from_config` находит провайдера и вызывает его билдеры |
| статус | `provider_status` = `match name` | `Provider::status` |
| `connect` | два раздельных флоу | один каркас + `Provider::provision` |
| разрешение стадии | валидация каждого провайдера инлайном в `resolve_embedding`/`resolve_reranker` | `Provider::resolve_embedding`/`resolve_reranker` |

## 2. Новый модуль `src/hybrid/provider.rs`

Ядро без провижининга (провижининг — CLI-забота, см. §6).

**Таблица не гейтится фичами.** Все известные провайдеры зарегистрированы в
любой сборке; фича решает, можно ли провайдера *использовать*, а не существует
ли он. Причина — сегодняшний контракт: `providers_from_raw`
(`src/config/mod.rs:108`) не гейтится, поэтому дефолтная сборка обязана
принимать `[providers.fastembed]`, записанный сборкой с `hybrid-local`, и
падать только на этапе сборки стейджа (`src/hybrid/config.rs:560`). Гейт при
парсинге сломал бы FR-3/FR-11 (ревью muse, P1-3).

```rust
/// Адрес модели: `<provider>/<model>` (FR-1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModelAddress<'a> { provider: &'a str, model: &'a str }

impl<'a> ModelAddress<'a> {
    /// Провайдер — сегмент до ПЕРВОГО `/`, модель — остаток как есть
    /// (`openrouter/nvidia/nemotron-3-embed-1b:free`).
    pub fn parse(raw: &'a str) -> Result<Self>;
    pub fn provider(&self) -> &'a str;
    pub fn model(&self) -> &'a str;
}
impl std::fmt::Display for ModelAddress<'_>;  // `provider/model`

/// Роль модели. Живёт здесь (не в `models`), потому что нужна трейту в любой
/// сборке: `models` гейтится `hybrid-local` (`src/hybrid/mod.rs:24`), а
/// `Provider::catalog` — нет (ревью muse, P1-2). `models` реэкспортирует.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Task { Embedding, Rerank }

/// Запись закрытого каталога (только данные, без ML-типов).
pub struct CatalogModel {
    pub name: &'static str,
    pub task: Task,
    pub dims: usize,
    pub revision: &'static str,
}

/// Одна строка таблицы провайдеров (G2, FR-5).
pub trait Provider: Sync {
    fn name(&self) -> &'static str;
    /// Build-фича, без которой провайдера нельзя ИСПОЛЬЗОВАТЬ (не «запрещён
    /// к парсингу конфига»). `None` — доступен всегда.
    fn feature(&self) -> Option<&'static str> { None }
    fn status(&self, providers: &ProvidersCfg) -> ProviderStatus;
    /// Разбор своей записи `[providers.<name>]`; по умолчанию принимает только
    /// `enabled` и отвергает незнакомые ключи (FR-4).
    fn parse_record(&self, table: &toml::Table) -> Result<ProviderRecord>;
    fn resolve_embedding(&self, model: &str, dims: Option<u32>) -> Result<EmbeddingCfg>;
    fn resolve_reranker(&self, model: &str, window: u32) -> Result<RerankerCfg>;
    /// Обе стадии за один вызов: провайдер сам решает, что строить, и может
    /// разделить дорогие ресурсы между ними (ревью muse, P1-4: OpenRouter
    /// держит `reqwest::Client` + `tokio::Runtime`,
    /// `src/hybrid/openrouter.rs:432-447`, и второй клиент на процесс не нужен).
    fn build_stages(
        &self,
        embedding: Option<&EmbeddingCfg>,
        reranker: Option<&RerankerCfg>,
        providers: &ProvidersCfg,
    ) -> Result<(Option<Arc<dyn Embedder>>, Option<Arc<dyn Reranker>>)>;
    /// Закрытый каталог моделей; `None` — каталога нет (remote API).
    fn catalog(&self, task: Task) -> Option<&'static [CatalogModel]> { None }
}

/// Регистрация: единственное место, где перечислены провайдеры (FR-5, G4).
pub fn providers() -> &'static [&'static dyn Provider];
pub fn provider(name: &str) -> Result<&'static dyn Provider>;  // Admission + список
pub fn feature_enabled(feature: &str) -> bool;                 // один match по фичам
```

Тяжёлые части (fastembed/ORT, HTTP-клиент) остаются в своих модулях за
`#[cfg(feature = …)]`; в сборке без фичи `build_stages` возвращает
`feature_missing_*()`, `status`/`parse_record`/`resolve_*` работают всегда —
как сегодня (`provider_status` и `resolve_embedding` не гейтятся, гейтится
только сборка стейджа).

`Provider::record()` из первой редакции удалён: единственный поставщик
содержимого `[providers.<name>]` — CLI-провижинер (§6, ревью muse, P3-9).

### Почему трейт с дефолтами, а не `struct { fn pointers }`

Провайдер — модуль с разными зависимостями (fastembed тянет ML-крейты за
фичей), поэтому реализация нужна кондициональная: `#[cfg(feature = …)]`.
Трейт с дефолтными методами позволяет провайдеру без каталога объявить ничего,
а таблица остаётся `&'static [&'static dyn Provider]`.

## 3. `from_config` (FR-6)

```rust
pub fn from_config(cfg: &HybridCfg, providers: &ProvidersCfg,
                   remote_on: bool, local_on: bool) -> Result<HybridStages> {
    from_config_with(providers_registry(), cfg, providers, remote_on, local_on)
}

/// Таблица — параметр, а не глобаль: тест расширяемости (FR-10) передаёт
/// сюда свой срез и проверяет всю цепочку без подмены глобалов.
fn from_config_with(table: &[&'static dyn Provider], …) -> Result<HybridStages> {
    // … существующие проверки формы (rrf без embedding, пустые секции) …
    let mut stages = HybridStages { ranking: cfg.ranking, ..Default::default() };
    for p in distinct_providers_in(cfg, table)? {   // 1 или 2 записи
        let (e, r) = p.build_stages(
            cfg.embedding.as_ref().filter(|c| c.provider == p.name()),
            cfg.reranker.as_ref().filter(|c| c.provider == p.name()),
            providers,
        )?;
        stages.embedder = stages.embedder.or(e);
        stages.reranker = stages.reranker.or(r);
    }
    Ok(stages)
}
```

`resolve(p)` = `provider(p)` + проверка `feature()` против build и
`remote_on`/`local_on`; тексты `feature_missing_remote/local` остаются
байт-в-байт (FR-11), просто вызываются из одного места.

Клиент OpenRouter живёт внутри `build_stages` и разделяется между стадиями
одного провайдера — то есть ровно одно создание на конфиг, как сегодня
(`src/hybrid/config.rs:579-594`). Двух стадий у разных провайдеров это не
касается: у каждого свой вызов.

## 4. Конфиг: generic-записи (FR-3, FR-4)

TOML остаётся как есть:

```toml
[providers.fastembed]
enabled = true
runtime = "1.24.4"
runtime_path = "/home/u/.local/share/docsbase/runtimes/onnxruntime/lib/libonnxruntime.so"

[providers.openrouter]
enabled = true
```

Внутри `ProvidersCfg` вместо полей `fastembed`/`openrouter`:

```rust
pub struct ProviderRecord {
    pub enabled: bool,
    /// Провайдер-специфичные ключи в том виде, как они лежат в TOML.
    pub params: toml::Table,
}
pub struct ProvidersCfg { entries: BTreeMap<String, ProviderRecord> }

impl ProvidersCfg {
    pub fn get(&self, name: &str) -> Option<&ProviderRecord>;
    pub fn iter(&self) -> impl Iterator<Item = (&str, &ProviderRecord)>;
}
```

Разбор `[providers.*]` (`src/config/mod.rs::providers_from_raw`) больше не
`match`-ит имена: для каждой таблицы берётся `provider(name)?` и вызывается
`Provider::parse_record(&toml::Table) -> Result<ProviderRecord>`. Дефолтная
реализация принимает только `enabled` и отвергает незнакомые ключи
(Admission с именем провайдера и списком его ключей) — так опечатка не
превращается в молчаливо выключенного провайдера.

Разбор **никогда не смотрит на фичи**: `parse_record` принимает ключи
провайдера в любой сборке (иначе дефолтный бинарь отвергал бы валидный
`[providers.fastembed]` — см. §2). Ошибки разбора объявляются явно:
неизвестное имя провайдера и незнакомый ключ внутри известного провайдера —
`Error::Admission` (сегодня незнакомый ключ даёт `Internal` из serde,
`src/config/mod.rs:96,477`, а `[providers.openrouter] runtime = "…"` молча
отбрасывается — после переезда это Admission с перечнем ключей провайдера).

Детали, которые надо сохранить при этом переезде (проверено по коду):

- `ProviderFile` (`src/config/mod.rs:94`) сейчас типизирован
  (`enabled`, `runtime`, `runtime_path`) и помечен `#[serde(deny_unknown_fields)]`.
  Он становится `HashMap<String, toml::Table>`: `deny_unknown_fields` переезжает
  в `Provider::parse_record` (у каждого провайдера свой набор ключей), иначе
  generic-парсер принимал бы мусор.
- Структурная защита проектного файла не меняется: `ProjectFileConfig`
  (`src/config/mod.rs:85`) не имеет поля `providers` вовсе, поэтому
  `[providers.*]` в проекте по-прежнему не парсится — это не проверка после
  merge, а отсутствие поля в типе.
- `ProvidersCfg` обязан сохранить `Debug + Default + PartialEq + Eq`
  (используется в `Config`, тестах сравнения и `cli_models`): `BTreeMap` их
  сохраняет, `toml::Table` — `PartialEq`/`Eq` тоже.
- Типизированный доступ для провайдера остаётся в его модуле:
  `fastembed_record(&ProvidersCfg) -> Option<FastembedRecord>`, чтобы
  `runtime::resolve` не разбирал `toml::Table` руками.

Публичный доступ к данным провайдера — типизированный, через его же модуль
(`fastembed_record(&ProvidersCfg) -> Option<FastembedRecord>`), чтобы
`runtime::resolve` не парсил `toml::Table` руками.

## 5. CLI: адрес и единый UX (FR-1, FR-2, FR-7, FR-8)

```
docsbase connect <provider> [--check] [--force]
docsbase models use embed <provider>/<model> [--dims N]
docsbase models use rerank <provider>/<model> [--window N]
docsbase models fetch <provider>/<model>
docsbase models remove <provider>/<model> [--force]
docsbase models list
```

- Разбор адреса — `ModelAddress::parse` в CLI, дальше только провайдер.
- Голое имя → Admission: `"bge-small-en-v1.5" is not a model address; use
  "fastembed/bge-small-en-v1.5" (providers: fastembed, openrouter)`.
- `--dims` необязателен там, где провайдер отвечает сам (локальный каталог),
  обязателен при неизвестной размерности (FR-4 прежней спеки).
- `fetch`/`remove` для провайдера без локального кэша → Admission:
  `provider "openrouter" has no local model cache; models are downloaded by
  the provider — nothing to fetch/remove` (NG3).
- `print_status`/`models list` — цикл по `providers()` с одинаковыми колонками;
  `version`/`path`/`reason` даёт провайдер (сегодня `version` добирается
  `match`-ем в `connect.rs:82-88`). Конкретно меняются три места вывода:
  строка `providers:\tfastembed=…\topenrouter=…` в `list_cmd`
  (`src/cli/models.rs`) становится циклом по таблице; строки каталога печатают
  полный адрес (`fastembed/bge-small-en-v1.5`) вместо голого имени; строка
  `selected:` печатает адрес, собранный из `provider` + `model` конфига.
- `docsbase status` (MCP-инструмент демона) провайдеров не показывает и в этой
  спеке не меняется — единый UX касается CLI-поверхности.
- `ConnectProvider` enum и `FastembedArgs` исчезают; провайдер-специфичные
  флаги, если появятся, пойдут в `#[arg(last = true)] extra: Vec<String>` —
  но сегодня таких флагов нет, поэтому YAGNI: общий набор `--check/--force`.

## 6. Провижининг (FR-8)

Провижининг живёт **целиком в CLI-слое**: ядро о нём не знает, поэтому
`hybrid::provider` не импортирует `cli` (обратный импорт был бы циклом слоёв,
ревью muse P1-1), а таблица в дефолтной офлайн-сборке не тянет `ureq`/`zip`.

```rust
// src/cli/connect.rs
trait Provisioner {
    fn check(&self) -> Result<Vec<String>>;        // строки отчёта, без записи
    fn install(&self, force: bool) -> Result<Vec<String>>;
    fn record(&self) -> toml::Table;               // то, что пойдёт в конфиг
}

/// Вторая таблица — CLI-локальная. Дрейф с реестром ловит тест
/// `every_provider_with_provisioning_is_registered` (сравнение имён).
fn provisioner_for(name: &str) -> Result<&'static dyn Provisioner>;

/// Каркас принимает провижинера параметром: тест расширяемости (FR-10)
/// прогоняет его с фейком, не подменяя глобалы.
fn connect_frame(p: &dyn Provisioner, name: &str, check: bool, force: bool)
    -> anyhow::Result<()>;
```

Каркас: `--check` и `--force` взаимоисключающи (одинаковый текст для всех
провайдеров) → `check()` или `install(force)` → запись `[providers.<name>]`
из `record()` → печать строки → подсказка о рестарте, если установка была
настоящей.

Семантика флагов по провайдерам (ревью muse P2-7; сегодня у `connect
openrouter` флагов нет вообще — `src/cli/connect.rs:25-39`):

| Команда | openrouter | fastembed |
|---|---|---|
| `connect <p>` | zero-token probe ключа → запись `enabled = true` | установить рантайм (или no-op, если уже стоит) → запись |
| `--check` | probe ключа, отчёт, **без записи** | отчёт о состоянии рантайма, без установки |
| `--force` | повторный probe → перезапись записи | переустановка рантайма |
| `--check --force` | Admission, общий текст | Admission, общий текст |

Провайдер без провижининга: `provisioner_for(name)` → `None` →
Admission «провайдер не требует подготовки».

## 7. Тест расширяемости (FR-10, приёмка 5)

Сев в продуктивном коде — только параметр таблицы в `from_config_with` (§3) и
параметр провижинера в `connect_frame` (§6). Никаких `#[cfg(test)]`-глобалов и
env-подмен: интеграционные тесты спавнят настоящий бинарь
(`assert_cmd::cargo_bin_cmd!`, `tests/cli_models.rs:20-33`), и `#[cfg(test)]`
элементы в нём не существуют (ревью muse P1-5).

В `src/hybrid/provider.rs`, `#[cfg(test)] mod tests`:

```rust
struct EchoProvider;   // embedding: детерминированный вектор из хеша текста
impl Provider for EchoProvider { … }        // каталог: одна модель `echo/tiny@d4`
static ECHO: EchoProvider = EchoProvider;
```

Тест проверяет, что для `echo` работают: `from_config_with(&[&ECHO], …)`,
`status`, `ModelAddress::parse("echo/tiny")` + `resolve_embedding`, и
`connect_frame(&EchoProvisioner, "echo", true, false)`. Ни один шаг не требует
правок `config.rs`/`cli/*` — это и есть проверяемое свойство G4.

## 8. Порядок работ (детали — в `tasks.md`)

1. `provider.rs` + адрес + таблица с двумя существующими провайдерами
   (переезд без изменения поведения; golden-тесты держат FR-11).
2. `ProvidersCfg` → generic-записи + `parse_record` у провайдеров.
3. `from_config`/`provider_status` через таблицу; удалить `match`-и.
4. CLI: адрес в `models use|fetch|remove`, единый `connect`, единый список.
5. Тест расширяемости + удаление оставшихся `PROVIDER_*` из не-провайдерских
   файлов.
6. Docs: `docs/CONFIG.md`, README/README.ru, CHANGELOG (breaking CLI),
   ADR-18.

## 9. Риски и как их снимаем

| Риск | Снятие |
|---|---|
| Разъедется поведение admission-текстов | FR-11 + существующие тесты `hybrid_config`/`cli_models`/`connect` как golden |
| Breaking CLI (`fetch bge-…` перестанет работать) | запись в CHANGELOG + текст ошибки содержит готовую команду; конфиги не мигрируют |
| `build_embedder` дважды создаёт OpenRouter-клиент | клиент = `Arc<ApiKey>` + транспорт без пула; профиль не в горячем пути (сборка стейджей один раз на конфиг) |
| Generic-записи усложнят `runtime::resolve` | типизированный геттер в модуле провайдера; `toml::Table` не покидает конфиг-слой |
| Тест расширяемости потребует seam в продуктивном коде | seam только `#[cfg(test)]` (thread-local override), как в `runtime::test_env` |
