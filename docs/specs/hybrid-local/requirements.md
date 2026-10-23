# SPECIFICATION: hybrid-local (итерация 4 hybrid-providers)

**Status:** Draft, ревизия 0.1 · **Дата:** 2026-10-23
**Constitution:** `docs/specs/constitution.md` (1.3.0)
**Связано:** `docs/specs/hybrid-providers/` (dense + RRF), `docs/specs/ann-index/`
(ANN), ADR-12 (`(model_id, chunk_hash)`), ADR-13 (локальные модели за флагом),
ADR-14 (OpenRouter), ADR-17 (эта итерация).

## 1. Problem

Hybrid-ступени сейчас умеют единственный провайдер — OpenRouter (feature
`hybrid-remote`). Проекты без сети/ключей и с требованиями приватности не могут
использовать dense/rerank: `provider = "openrouter"` требует `OPENROUTER_API_KEY`
и сетевой доступ на index/backfill/search. Нужен локальный провайдер, который
работает офлайн после явной подготовки, не ломает дефолтную сборку и не тащит
веса в бинарь.

## 2. Goals / Non-goals

Goals:
- **G1.** Провайдер `fastembed` для embedding и rerank за фичей `hybrid-local`;
  контракты `Embedder`/`Reranker` и ключ `model_id` не меняются.
- **G2.** Дефолтная сборка остаётся офлайн: нет `fastembed`/`ort`/`tokenizers`/
  `hf-hub`/`reqwest` в `cargo tree`.
- **G3.** Никакой сети на index/search: рантайм и модели готовятся только
  `docsbase connect` / `docsbase models fetch`; иначе — admission-ошибка.
- **G4.** Общая модель конфигурации: `[providers.*]` — только глобальный конфиг;
  `[hybrid.*]` — проект (по умолчанию) или глобальный (`--global`) как дефолт
  для новых проектов.
- **G5.** Воспроизводимость: pinned revision моделей, sha256 рантайма и файлов,
  атомарная установка, идемпотентные команды.

Non-goals:
- GPU/CUDA/Metal/DirectML, image-модели, fine-tuning, авто-обновление моделей.
- MCP-инструменты для connect/fetch (CLI достаточно).
- Модели по умолчанию и любое авто-скачивание.
- Изменение MCP/CLI контрактов `search_docs`, `docsbase search`, ANN.

## 3. Functional requirements

- **FR-1 (провайдерский реестр).** `[providers.<name>]` допускается только в
  глобальном конфиге (`~/.config/docsbase/config.toml`); поле `enabled = true`
  означает «провайдер подготовлен на этой машине». Для `fastembed` дополнительно
  хранятся `runtime` (версия) и необязательный `runtime_path`; для `openrouter`
  ключ не хранится (env-only, ADR-14). Появление `[providers]` в проектном
  `.docsbase.toml` — ошибка валидации.
- **FR-2 (`connect`).** `docsbase connect` без аргументов печатает статус
  провайдеров. `connect openrouter` берёт `OPENROUTER_API_KEY` из окружения,
  проверяет его запросом к API и записывает `[providers.openrouter] enabled=true`;
  ключ не сохраняется. `connect fastembed` скачивает закреплённый ONNX Runtime
  под текущую платформу/арх, проверяет sha256, атомарно устанавливает в data-dir
  и записывает `[providers.fastembed]`. `--check` — только проверка,
  `--force` — переустановка, `--models a,b` — дополнительно `models fetch`.
  `--local` для connect запрещён (провайдеры машинные).
- **FR-3 (реестр моделей).** В коде — закрытый реестр: имя, HF repo, pinned
  revision (commit sha), файлы (`onnx/model.onnx`, `tokenizer.json`,
  `config.json`, `special_tokens_map.json`, `tokenizer_config.json`,
  `additional_files`), `dim`, pooling (`Cls`/`Mean`), quantization, `output_key`,
  license. Дефолтной модели нет: `[hybrid.embedding].model` обязателен при
  `provider = "fastembed"`.
- **FR-4 (`models fetch/list/remove`).** `fetch <name>` скачивает только файлы
  реестра по pinned revision, проверяет размер и sha256, ставит атомарно
  (temp+rename) под локом, идемпотентен; `list` показывает реестр и состояние
  кэша; `remove` удаляет модель. Офлайн/недоступность HF — ошибка с подсказкой.
  Встроенный загрузчик `fastembed` не используется: сеть только явная.
- **FR-5 (`models use` и scope).** `models use --provider <p> --embedding <m>
  [--dims N] [--rerank <m>] [--window N] [--local|--global] [--dry-run]
  [--path <dir>]`. Внутри проекта по умолчанию пишется `.docsbase.toml`;
  `--local` допустим только внутри проекта; `--global` пишет глобальный конфиг
  (дефолт для новых проектов) и обязателен вне проекта; оба флага — ошибка.
  Команда валидирует провайдер (connected), модель (реестр/кэш), `dimensions`
  (для fastembed — из реестра, для OpenRouter — обязателен), окно rerank.
- **FR-6 (embedding).** `provider = "fastembed"` создаёт `Embedder`; `model_id =
  fastembed/<name>@d<dim>@<l2|raw>@r<rev8>`, `normalize` обязателен `true` для
  реестровых моделей (иначе admission). Модель загружается один раз на процесс.
- **FR-7 (rerank).** `provider = "fastembed"` создаёт `Reranker`; `top_n =
  documents.len()`, окно — те же bounds; ошибки — `Error::Internal`, без fallback.
- **FR-8 (фича и сборка).** `hybrid-local` независима от `hybrid-remote`;
  комбинация фич допустима. ORT грузится `ort-load-dynamic`; на старте
  `ort::init_from(path)` до первой сессии; порядок резолва: `ORT_DYLIB_PATH` →
  data-dir → ошибка. Веса в бинарь не входят.
- **FR-9 (ошибки).** Отсутствие провайдера, рантайма, модели, битой модели или
  несовпадение `dimensions` — `Error::Admission` с точной подсказкой
  (`docsbase connect …` / `docsbase models fetch …`). Никакого молчаливого
  перехода на lexical/exact из-за локального провайдера.

## 4. Non-functional requirements

- **NFR-1 (offline default).** `cargo tree` дефолтной сборки не содержит
  `fastembed`, `ort`, `tokenizers`, `hf-hub`, `ureq`, `reqwest`; CI-гейт.
- **NFR-2 (MSRV/quality).** Rust 1.88, `fmt`, clippy `--all-targets -D warnings`;
  без `unwrap`/`expect`/`panic!` в `src/**`.
- **NFR-3 (безопасность).** HTTPS+rustls, pinned revision, sha256, size caps,
  права кэша; рантайм — исполняемый код, поэтому только explicit connect;
  ONNX/токенизаторы считаются недоверенным вводом (ORT парсит, код не грузим).
- **NFR-4 (ресурсы).** Модель и рантайм не учитываются в `DenseCache.max_bytes`;
  `max_length` (default 512), batch size и intra/inter threads конфигурируемы,
  потоки не превышают доступные ядра; модель не грузится дважды.
- **NFR-5 (идемпотентность).** `connect`/`fetch` можно повторять; повторный
  вызов не качает заново и не ломает кэш; параллельные процессы — лок.
- **NFR-6 (время старта).** Конфиг с `provider = "fastembed"` и отсутствующей
  моделью падает на открытии проекта/поиске, а не в середине backfill.

## 5. Success criteria

- **SC-1.** Дефолтная сборка зелёная, гейт NFR-1 проходит; `hybrid-local`
  собирается и проходит clippy на Linux/macOS/Windows.
- **SC-2.** `connect fastembed` на чистой машине ставит runtime (sha256), пишет
  `[providers.fastembed]` в глобальный конфиг; повторный вызов — no-op.
- **SC-3.** `connect fastembed --check` без сети и без рантайма даёт понятный
  статус; без `connect` конфиг с fastembed падает admission-ошибкой.
- **SC-4.** `models fetch` скачивает pinned файлы, проверяет хеши, идемпотентен;
  офлайн — ошибка без частично установленной модели.
- **SC-5.** `models use` пишет проект или global по scope-правилам, не трогая
  `[providers]`; вне проекта без `--global` — ошибка.
- **SC-6.** Embedding+rerank через fastembed дают те же контракты, что OpenRouter:
  `model_id` участвует в кэше, `dimensions`/`normalize` валидируются, rerank
  меняет порядок и scores.
- **SC-7.** Живой тест (ignored) на реальной модели: `connect` + `fetch` + embed
  + rerank проходят; офлайн-тесты не ходят в сеть.
- **SC-8.** Документация: ADR-17, spec, `docs/CONFIG.md`, README, SECURITY
  (рантайм как внешний артефакт), CHANGELOG.

## 6. Open questions

- **OQ-1.** Точные sha256/URLы ORT 1.24.x по 6 платформам — фиксируются при T2.
- **OQ-2.** Начальный набор моделей (bge-small / e5-small / bge-reranker-base) —
  расширяется без изменения контрактов.
- **OQ-3.** Нужен ли `connect --models` как шорткат `fetch` (сейчас `should`).
- **OQ-4.** Живой CI-прогон модели: manual/nightly после T7, не per-PR.

## 7. Out of scope

GPU-провайдеры, квантизация на лету, кэш эмбеддингов между моделями, `docsbase
serve --models`, MCP-тулы управления моделями.
