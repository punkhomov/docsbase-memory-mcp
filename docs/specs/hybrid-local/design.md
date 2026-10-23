# Technical Design: hybrid-local (итерация 4)

**Status:** Draft, ревизия 0.1 · **Дата:** 2026-10-23
**Spec:** `docs/specs/hybrid-local/requirements.md` (ревизия 0.1)
**ADR:** ADR-17 (product design), ADR-12/13/14 (смежные).

## 1. Overview

Локальный провайдер `fastembed` добавляется как ещё одна реализация `Embedder`
(`vector_cache.rs`) и `Reranker` (`hybrid/mod.rs`). Всё, что знает про модели,
рантайм и сеть, живёт в двух новых модулях и в CLI; сами ступени получают
готовые объекты и работают офлайн.

```
src/hybrid/local.rs      # FastEmbedEmbedder / FastEmbedReranker (feature hybrid-local)
src/hybrid/models.rs     # реестр моделей, cache layout, fetch (feature hybrid-local)
src/hybrid/runtime.rs    # поиск/установка ONNX Runtime, ort::init_from (feature)
src/cli/connect.rs       # docsbase connect ...
src/cli/models.rs        # + fetch/list/remove, --local/--global
src/config/mod.rs        # [providers], scope/precedence, валидация
```

## 2. Config model

### 2.1 Shape

Глобальный конфиг (только он содержит провайдеров и может содержать дефолты):

```toml
[providers.fastembed]
enabled = true
runtime = "onnxruntime-1.24.0"     # версия, установленная connect
# runtime_path = "/opt/.../libonnxruntime.so"   # необязательный override

[providers.openrouter]
enabled = true                     # ключ — только env (ADR-14)

[hybrid]                           # дефолты для новых проектов (опционально)
ranking = "rrf"
[hybrid.embedding]
provider = "fastembed"
model = "bge-small-en-v1.5"
dimensions = 384
normalize = true
```

Проект (`.docsbase.toml`) содержит только `[hybrid]`; `[providers]` здесь —
ошибка `Admission` («providers are machine-local; run docsbase connect …»).

### 2.2 Precedence and validation

- `Config::for_project` уже мержит global → project; добавляется валидация:
  - `providers` запрещены в проектном файле;
  - `hybrid.embedding.provider`/`hybrid.reranker.provider` обязаны быть
    `enabled` в эффективных провайдерах; иначе `Admission` с подсказкой;
  - `provider = "fastembed"` требует `model` (дефолта нет), `dimensions` =
    реестровой ширине, `normalize = true`;
  - `provider = "openrouter"` сохраняет текущие правила (ключ, feature).
- Порядок: built-in default → global config → project config → CLI overrides.

### 2.3 Scope resolution (`models use`)

- `--local` и `--global` взаимоисключающие.
- Внутри проекта (есть `.docsbase.toml` или `--path` указывает на проект):
  по умолчанию локально; `--local` — явное подтверждение.
- Вне проекта: только `--global`, иначе `Admission`.
- Запись атомарная (temp+rename), существующие секции сохраняются; известное
  ограничение v1 — комментарии не сохраняются (как в текущем `models use`).

## 3. Provider registry and `connect`

- `providers::status(config)` → `Unknown | NotConnected | Connected{version} |
  Broken{reason}`; используется `connect` без аргументов и валидацией конфига.
- `connect openrouter`:
  1. читает `OPENROUTER_API_KEY` из env (флаг не нужен, ключ не пишется);
  2. проверяет запросом к `/models` (feature `hybrid-remote`);
  3. пишет `[providers.openrouter] enabled = true` в **глобальный** конфиг.
- `connect fastembed`:
  1. определяет платформу/арх (`linux-x64`, `linux-aarch64`, `osx-arm64`,
     `osx-x64`, `win-x64`, `win-arm64`);
  2. качает закреплённый архив ORT 1.24.x из официального релиза, проверяет
     sha256 из встроенной таблицы, распаковывает только shared library;
  3. атомарно ставит в `<data>/runtimes/onnxruntime/<version>/<platform>/`;
  4. пишет `[providers.fastembed] enabled/runtime` в глобальный конфиг.
  `--check` — только статус; `--force` — переустановка; `--models` — затем
  `models fetch` для перечисленных имён.
- `connect` не меняет модельный выбор и не пишет в проект.

## 4. Runtime resolution

- Порядок: `ORT_DYLIB_PATH` (если задан и существует) → `runtime_path` из
  конфига → `<data>/runtimes/onnxruntime/<version>/<platform>/libonnxruntime.*`
  → `Admission` с подсказкой `docsbase connect fastembed`.
- `ort::init_from(path).commit()` вызывается один раз на процесс
  (`OnceLock`), **до** создания `TextEmbedding`/`TextRerank`. `commit()`
  идемпотентен по `try_insert_with`, повторная инициализация не перезаписывает.
- Имя библиотеки по платформам: `libonnxruntime.so` (Linux),
  `libonnxruntime.dylib` (macOS), `onnxruntime.dll` (Windows); Windows DLL
  резолвится рядом с загруженным файлом.

## 5. Model registry and fetch

### 5.1 Registry entry

```rust
struct ModelEntry {
    name: &'static str,            // "bge-small-en-v1.5"
    task: Task,                    // Embedding | Rerank
    repo: &'static str,            // "BAAI/bge-small-en-v1.5"
    revision: &'static str,        // HF commit sha (pinned)
    onnx: &'static str,            // "onnx/model.onnx"
    additional: &'static [&'static str],
    dim: usize,
    pooling: Option<Pooling>,      // Cls/Mean for embeddings
    quantization: QuantizationMode,
    output_key: Option<&'static str>,
    license: &'static str,
    files: &'static [FileSpec],    // path + size + sha256
}
```

Стартовый набор: `bge-small-en-v1.5` (384/Cls), `multilingual-e5-small`
(384/Mean), `bge-reranker-base` (rerank). `jina-reranker-v2` не включается
(CC-BY-NC).

### 5.2 Cache layout

```
<model cache>/                      # DOCSBASE_MODEL_CACHE или <data>/models
  <name>/<revision>/
    model.onnx                      # + additional files
    tokenizer.json
    config.json
    special_tokens_map.json
    tokenizer_config.json
    .complete                       # маркер: все файлы проверены
```

- Скачивание: `https://huggingface.co/<repo>/resolve/<revision>/<path>`,
  `ureq` (rustls), поток в `*.part`, sha256 по таблице, `fsync`, rename.
- Лок от параллельных fetch: lock-файл `<name>/.lock` (`fd-lock` уже в deps).
- `.complete` пишется последним; отсутствие маркера = модель не готова.
- `remove` удаляет каталог целиком; `list` показывает размер/статус.

## 6. Embedder / reranker

- `FastEmbedEmbedder::new(entry, options)`:
  1. ensure runtime (init_from);
  2. читает файлы из кэша (после `models fetch`), строит
     `UserDefinedEmbeddingModel::new(onnx_bytes, TokenizerFiles {..})`
     `.with_pooling(entry.pooling)` (и `.with_quantization`, если задана);
  3. `TextEmbedding::try_new_from_user_defined(model, InitOptionsUserDefined)`
     с `max_length`, `intra_threads`, `batch_size`;
  4. `Embedder::model_id()` = `fastembed/<name>@d<dim>@l2@r<rev8>`.
- `FastEmbedReranker`: `UserDefinedRerankingModel` + `TextRerank`; `rerank`
  возвращает `(index, score)`, `top_n` = переданный.
- Модель создаётся один раз на процесс (`Arc`, `OnceLock` на конфиг-ключ);
  вызовы — внутри `blocking::enter`; потоки ограничены `min(4, cores)`.
- Ошибки ORT/токенизатора → `Error::Internal`; отсутствие файлов → `Admission`
  с подсказкой `docsbase models fetch`.

## 7. CLI

| Команда | Поведение |
|---|---|
| `docsbase connect` | статус всех провайдеров |
| `docsbase connect openrouter` | env key → probe → global `[providers.openrouter]` |
| `docsbase connect fastembed [--check\|--force\|--models a,b]` | runtime (+models) → global `[providers.fastembed]` |
| `docsbase models list` | реестр + cached/missing |
| `docsbase models fetch <name>` | явная загрузка pinned файлов |
| `docsbase models remove <name>` | удаление кэша |
| `docsbase models use … [--local\|--global]` | выбор модели, project/global |

## 8. Errors

Все пользовательские ошибки — `Error::Admission` с одним действием:
- «provider `fastembed` is not connected — run `docsbase connect fastembed`»;
- «model `bge-small-en-v1.5` is not fetched — run `docsbase models fetch bge-small-en-v1.5`»;
- «dimensions 1024 do not match model `bge-small-en-v1.5` (384)»;
- «providers are machine-local; edit the global config / run connect».

## 9. Tests and CI

- Offline (default build): парсинг `[providers]`, запрет в проекте, scope
  `--local/--global`, валидация provider/model/dims, `model_id`, CLI dry-run,
  отсутствие сетевых крейтов в `cargo tree`.
- Feature build (`hybrid-local`): сборка на 3 ОС, unit-тесты реестра/кэша с
  fake-`ModelSource`, статус connect с fake-runtime, `models use --global`.
- Живые тесты `#[ignore]`: `connect --check` + `models fetch` + embed/rerank на
  реальной модели; запуск manual/nightly, не per-PR.
- CI: новый job `linux-local` (сборка+тесты без сети), гейт дерева в `linux`.

## 10. Security

- Рантайм — исполняемый код: explicit `connect`, pinned version+sha256, запись
  в `docs/specs/security`/SECURITY.md; отказ от загрузки без подтверждения.
- Модели — данные, но парсятся ORT/токенизатором: pinned revision, sha256,
  size caps, права кэша 0700.
- Ключи не пишутся на диск; `connect openrouter` читает env и не логирует ключ.

## 11. Acceptance mapping

| Требование | Задачи |
|---|---|
| FR-1, FR-2, FR-5 | T2, T3 |
| FR-3, FR-4 | T1 |
| FR-6 | T4 |
| FR-7 | T5 |
| FR-8, NFR-1 | T3, T7 |
| FR-9, NFR-6 | T1–T5 (ошибки), T6 |
| SC-1…SC-8 | T1–T8 |
