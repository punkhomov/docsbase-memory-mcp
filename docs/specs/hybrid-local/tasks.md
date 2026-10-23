# Tasks: hybrid-local (итерация 4)

**Status:** Draft (2026-10-23). T1–T8; один таск за раз, TDD (RED → GREEN),
ledger в `progress.md` при каждом коммите.

## Global constraints

- Rust 1.88, edition 2024; `cargo fmt`; clippy `--all-targets -D warnings`.
- Без `unwrap`/`expect`/`panic!` в `src/**`.
- Дефолтная сборка офлайн: `fastembed`/`ort`/`tokenizers`/`hf-hub`/`ureq`/
  `reqwest` отсутствуют в `cargo tree` (CI-гейт).
- Новые крейты — только ADR-17: `fastembed = "=7.1.1"`, `ort = "=2.0.0-rc.13"`,
  `ureq 3.4.x`, `sha2`, `flate2`, `tar`, `zip` (все под `hybrid-local`).
- Веса моделей и рантайм в бинарь не попадают; сеть только в `connect`/`fetch`.
- Контракты `search_docs`, MCP, ANN не меняются.

## T1 — Model registry + cache + fetch

**Goal:** реестр моделей и явная загрузка с HF по pinned revision.

**Files:** `src/hybrid/models.rs`, `tests/hybrid_models.rs` (или unit),
`Cargo.toml` (фича `hybrid-local`, deps).

**Steps (RED → GREEN):**
1. RED: тесты реестра — уникальные имена, `dim`, pooling, license, наличие
   `model.onnx` + 4 tokenizer-файла; `model_id` формат.
2. RED: кэш — layout `<cache>/<name>/<revision>/`, `.complete` последним,
   `is_cached()`, `remove`, идемпотентный fetch через fake `ModelSource`
   (трейт: `fetch_file(repo, revision, path) -> bytes`).
3. GREEN: реализация; sha256/размер из `FileSpec`; атомарная запись
   (`*.part` → `fsync` → rename), lock-файл через `fd-lock`.
4. Офлайн: `fetch` без source → `Admission` с подсказкой.

**Acceptance:** тесты зелёные без сети; `cargo tree` дефолта чист.

## T2 — ONNX Runtime downloader + `connect`

**Goal:** `docsbase connect` ставит ORT и регистрирует провайдеров.

**Files:** `src/hybrid/runtime.rs`, `src/cli/connect.rs`, `src/cli/mod.rs`,
`tests/connect.rs`.

**Steps:**
1. RED: выбор платформы (6 целей), таблица версия→URL→sha256; `--check`
   без сети; статус `Unknown/NotConnected/Connected/Broken`.
2. RED: fake downloader + fake filesystem: атомарная установка, `--force`,
   идемпотентность, ошибка при неверном sha256 (ничего не установлено).
3. GREEN: скачивание (ureq), распаковка (flate2/tar; zip на Windows), только
   shared library; запись `[providers.fastembed]` в глобальный конфиг.
4. `connect openrouter`: env key → probe → `[providers.openrouter]`; ключ не
   сохраняется и не логируется; без ключа — admission с подсказкой.
5. `connect` без аргументов — таблица статуса.

**Acceptance:** офлайн-тесты; живой `--check` на машине без сети даёт статус.

## T3 — Provider registry in config + scope validation

**Goal:** `[providers]` в глобальном конфиге, запрет в проекте, проверка
provider/model/dims в `from_config`.

**Files:** `src/config/mod.rs`, `src/hybrid/config.rs`, `tests/config.rs`.

**Steps:**
1. RED: `[providers]*` в `.docsbase.toml` → `Admission`.
2. RED: `provider="fastembed"` без `[providers.fastembed]` → `Admission`
   с подсказкой; без `model` → `Admission`; `dimensions` ≠ реестр → `Admission`;
   `normalize=false` для реестровой модели → `Admission`.
3. RED: global defaults → project override (precedence) на примере provider/model.
4. GREEN: парсинг/валидация, `providers::status`, интеграция в `from_config`
   (feature-off/fastembed → `feature_missing`).

**Acceptance:** все ветки покрыты; старые OpenRouter-тесты обновлены на
`[providers.openrouter]`.

## T4 — `FastEmbedEmbedder`

**Goal:** embedder на user-defined модели, один на процесс, офлайн.

**Files:** `src/hybrid/local.rs`, `src/hybrid/mod.rs`, `tests/hybrid_local.rs`.

**Steps:**
1. RED: `model_id` = `fastembed/<name>@d<dim>@l2@r<rev8>`; отсутствие модели →
   `Admission`; `ort::init_from` вызывается до сессий (fake runtime path).
2. RED: fake ONNX? Если нельзя — тест на конструктор с отсутствующими файлами и
   на `blocking`-контракт; реальный инференс — ignored live.
3. GREEN: `UserDefinedEmbeddingModel` + pooling/quantization; `max_length`,
   threads; `Arc`/`OnceLock`; ошибки → `Internal`.
4. feature-gated: без `hybrid-local` модуль не компилируется в сборку.

**Acceptance:** `cargo clippy --features hybrid-local` чист; live ignored тест
описан.

## T5 — `FastEmbedReranker`

**Goal:** локальный rerank через `TextRerank`.

**Files:** `src/hybrid/local.rs`, `tests/hybrid_local.rs`.

**Steps:**
1. RED: `Reranker::rerank` возвращает `(index, score)` для всех документов,
   `top_n` соблюдается; ошибки — `Internal`.
2. GREEN: `UserDefinedRerankingModel` + `RerankInitOptionsUserDefined`;
   модель шарится с embedder-кэшем (не грузится дважды).

**Acceptance:** контракт совпадает с OpenRouter-реализацией; live ignored.

## T6 — `models use` scope + CLI

**Goal:** `--local/--global`, provider/model validation, запись в нужный файл.

**Files:** `src/cli/models.rs`, `src/cli/mod.rs`, `tests/cli_models.rs`.

**Steps:**
1. RED: внутри проекта без флагов → `.docsbase.toml`; `--local` ok;
   вне проекта без `--global` → `Admission`; оба флага → `Admission`.
2. RED: fastembed model без fetch → `Admission` с `docsbase models fetch`;
   `--provider fastembed --embedding bge-small-en-v1.5` без `--dims` → dims из
   реестра; с неверным `--dims` → `Admission`.
3. RED: глобальная запись не трогает `[providers]` и другие секции.
4. GREEN: реализация; `--dry-run` печатает diff.

**Acceptance:** scope-матрица покрыта; комментарии не сохраняются (известное
ограничение, в выводе).

## T7 — Feature wiring, CI gates, docs

**Goal:** фича собирается на 3 ОС, дефолт офлайн, доки полные.

**Files:** `Cargo.toml`, `.github/workflows/ci.yml`, `docs/CONFIG.md`,
`README.md`, `SECURITY.md`, `CHANGELOG.md`.

**Steps:**
1. `hybrid-local = ["dep:fastembed", ...]`; default без изменений.
2. CI: `linux-local` job (`cargo clippy/test --features hybrid-local`, без
   сети для тестов), гейт `cargo tree` (fastembed/ort/...) в `linux`.
3. Docs: CONFIG (providers, scope, fetch/connect), README (quickstart), SECURITY
   (runtime как внешний артефакт), CHANGELOG.
4. `cargo tree -e normal --features hybrid-local` — зафиксировать состав.

**Acceptance:** все jobs зелёные; дефолтный артефакт не вырос.

## T8 — Live verification (ignored)

**Goal:** подтвердить реальный путь на модели.

**Steps:**
1. `#[ignore]` тест: `connect --check` (или `ORT_DYLIB_PATH`) + `models fetch
   bge-small-en-v1.5` + embed 3 текстов + `bge-reranker-base` rerank.
2. Записать в ledger: версия ORT, sha256, размеры моделей, время загрузки,
   latency embed/rerank, RSS.
3. Не включать в per-PR CI; manual/nightly.

**Acceptance:** ledger с числами; тест воспроизводим по инструкции.

## Definition of done

- Дефолтная сборка офлайн и без новых крейтов в дереве.
- `connect`, `models fetch/use` покрыты офлайн-тестами; scope-правила соблюдены.
- Embedding+rerank через fastembed работают офлайн после явной подготовки.
- Ошибки — admission с действием; никакой сети на index/search.
- Доки/ADR/CHANGELOG обновлены; живой прогон записан в ledger.
