# SPECIFICATION: hybrid-providers (итерация 1 — OpenRouter-only)

**Status:** Draft · **Дата:** 2026-10-08
**Constitution:** `docs/specs/constitution.md` (1.2.0)
**Связано:** ADR-12 (кэш `(model_id, chunk_hash)`), ADR-13 (OpenRouter-старт, флаги, 40 MiB — вес ПО)

## Problem

BM25-поиск (`index/tantivy_index.rs`) не ловит парафразы и семантически близкие формулировки
в русскоязычных доках. Hybrid-ступени (embedding + reranker) запроектированы (ADR-12/13,
`VectorCache`, `Config::hybrid`), но ни один провайдер не подключён: `Embedder` реализован
только fake'ами в тестах, rerank отсутствует как сущность.

## Goals / Non-goals

Goals (итерация 1):
- G1. Embedding чанков через OpenRouter (`POST /api/v1/embeddings`), дефолт
  `qwen/qwen3-embedding-0.6b`, с записью в существующий `VectorCache`.
- G2. Rerank BM25 top-k через OpenRouter (`POST /api/v1/rerank`), дефолт
  `cohere/rerank-v3.5`, `top_n = 10`.
- G3. Один ключ, один base URL, две ручки; sync-контракт без переделки кэша.
- G4. Дефолтная сборка остаётся полностью офлайн (SC-8, NFR-5 без изменений).

Non-goals (явно позже, отдельными задачами): прямые вендоры (Cohere/Voyage/…),
Ollama/llama.cpp, local (`fastembed`), `custom`-пресеты сверх base_url-override,
автоподбор `dimensions`, `return_logits`-rerank, streaming, async-трейт
(закрыто решением 2026-10-08 с триггером: веерные per-request запросы с дедлайном —
см. Challenges).

## Users / Actors

- U1. Оператор docsbase (владелец машины): включает hybrid в `.docsbase.toml`,
  кладёт `OPENROUTER_API_KEY` в env, платит за токены.
- U2. Daemon (`docsbase serve`): фоновая индексация батчами, per-request rerank поиска.
- U3. Разработчик следующей итерации: добавляет провайдера, не меняя контракт.

## User stories

- **P1.** Index с эмбеддингами. *Given* `hybrid=true`, валидный `OPENROUTER_API_KEY`,
  проект с N чанками; *When* выполняется `index_project`; *Then* все чанки получают
  вектора в `vectors.db` под ключом `openrouter/qwen/qwen3-embedding-0.6b@d1024`,
  повторный прогон сети не касается (cache hit 100%).
- **P2.** Поиск с rerank. *Given* `hybrid=true`, индекс с векторами; *When*
  `search_docs(query)`; *Then* BM25 top-k переупорядочены OpenRouter-rerank,
  ответ содержит цитаты как раньше (формат MCP-ответа не меняется).
- **P3.** Конфигурация оператором. *Given* `.docsbase.toml` с `[hybrid.*]` и env-ключом;
  *When* старт daemon; *Then* hybrid-ступени активны; *Given* ключа нет;
  *When* старт с `hybrid=true`; *Then* явная `Admission`-ошибка с инструкцией
  (никакого молчаливого fallback на BM25-only).
- **P4.** Офлайн-дефолт. *Given* `hybrid=false` (дефолт) или сборка без feature;
  *When* любой сценарий v1; *Then* ноль сетевых вызовов (`tests/offline.rs` зелёный),
  поиск работает как раньше.

## Functional requirements

- FR-1. `OpenRouterEmbedder: Embedder` — батч `POST {base}/v1/embeddings`
  `{model, input[], dimensions?, encoding_format?}` → `Vec<Vec<f32>>` по порядку входа.
- FR-2. `OpenRouterReranker: Reranker` — `POST {base}/v1/rerank`
  `{model, query, documents, top_n}` → `Vec<(original_index, score)>`, desc по score.
- FR-3. `provider = "openrouter"` — единственное допустимое значение в итерации 1;
  иное → `Admission`-ошибка («итерация 1: только openrouter»).
- FR-4. API-ключ — только из env (`OPENROUTER_API_KEY`); в toml-интерфейсе поля
  под ключ нет; ключ не попадает в логи (`NFR-8` маскирование) и в `status`.
- FR-5. `model_id` = `openrouter/<model>@d<dim>` для embeddings,
  `openrouter/<model>` для rerank (rerank не кэшируется, ключ не нужен).
- FR-6. Чанкинг `embed_many`: лимит числа текстов на запрос (дефолт 128 до P0-spike)
  + UTF-8 byte-budget; переполненный одиночный текст — отдельным запросом;
  склейка строго по порядку, `usage.tokens` суммируется.
- FR-7. Retry: 429/503/5xx — экспоненциальный backoff с капом и джиттером;
  прочие 4xx — сразу ошибка (без ретраев). Таблица кодов — из P0-spike.
- FR-8. Client-side L2-нормализация флагом пресета (Qwen3 отдаёт ненормированные
  вектора; факт подтверждает P0-spike; дефолт флага — по итогам спайка).
- FR-9. Заголовки OpenRouter: `HTTP-Referer`, `X-Title` (идентификация приложения).
- FR-10. Трейты остаются sync (`Embedder::embed`, новый `Reranker::rerank`);
  async только внутри реализации, ожидание — в `spawn_blocking` (ADR-7).

## Non-functional requirements (ISO 25010)

- NFR-1 Performance: bulk-индексация 50k чанков завершается без деградации daemon
  (параллелизм батчей ×8–16, keep-alive); per-request rerank top-50 не хуже p95 +2 с
  к BM25-базе (замер в PR, не гейт).
- NFR-2 Reliability: батч валидируется целиком до записи в кэш (как ADR-12:
  «полу-поколение» запрещено); сетевой сбой ступени — явная ошибка операции,
  не тихий BM25-fallback.
- NFR-3 Security: дефолт офлайн; сеть только при `hybrid=true`; allowlist —
  только `https://openrouter.ai/api/v1/*` (нарушение — ошибка, не warning);
  ключ только env (FR-4).
- NFR-4 Maintainability: один wire-код на оба вызова; новый провайдер позже —
  только конструктор пресета, без touching wire-кода.
- NFR-5 Portability: `hybrid-remote` — cargo-feature; дефолтный статический
  артефакт без сетевых крейтов, 40 MiB-бюджет (NFR-2 продукта) не затронут.

## Data model

Без новых таблиц. `vectors.db`: ключ `(model_id, chunk_hash)` (существует);
`dim` в строке сверяется с ответом (MRL-несоответствие → `Internal`-ошибка,
не молчаливая перезапись). Rerank-скоры не персистятся.

## Interfaces

```rust
// переиспользуется из src/vector_cache.rs (без изменений)
pub trait Embedder: Send + Sync {
    fn model_id(&self) -> &str;
    fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>>;
}
// новый трейт, src/hybrid/mod.rs
pub trait Reranker: Send + Sync {
    fn model_id(&self) -> &str;
    fn rerank(&self, query: &str, docs: &[String], top_n: usize) -> Result<Vec<(usize, f32)>>;
}
```

```toml
[hybrid.embedding]
provider = "openrouter"
model = "qwen/qwen3-embedding-0.6b"
dimensions = 1024
[hybrid.reranker]
provider = "openrouter"
model = "cohere/rerank-v3.5"
top_n = 10
```

## Measurable success criteria

- SC-1. P1 на фикстуре ≥100 чанков: второй прогон — 0 HTTP-вызовов (счётчик в fake-транспорте).
- SC-2. P2: rerank меняет порядок синтетического BM25-top-k предсказуемо (тест на фикстуре).
- SC-3. P3: отсутствие ключа при `hybrid=true` → `Admission` с инструкцией (тест).
- SC-4. P4: `cargo test --locked --test offline` зелёный; дефолтный бинарь без `reqwest`
  в `cargo tree` (CI-check).
- SC-5. Wire-тесты на записанных фикстурах OpenRouter (схемы, ошибки, L2, порядок) — без сети в CI.

## Assumptions & challenges

- A1. Схемы OpenRouter стабильны на итерацию (проверяется P0-spike; дрейф — явная ошибка парсинга, не молчание).
- A2. `qwen/qwen3-embedding-0.6b` и `cohere/rerank-v3.5` доступны в роутере (подтверждает спайк; fallback — соседние ID тем же PR).
- A3. Challenge: OpenRouter не версионирует веса провайдеров → тихий апдейт веса не инвалидирует
  кэш. Принято как известное ограничение итерации 1 (дешёвый риск: качество обычно растёт;
  дорогой контроль — пин провайдера/ревизии — отложен).
- A4. Challenge: sync вместо async. Отклонено с обоснованием (батч+параллелизм дают весь
  выигрыш; редизайн не окупается). Триггер пересмотра: веерные per-request запросы с дедлайном.
- A5. Challenge: почему не напрямую Cohere (эталон rerank)? Один ключ/биллинг/схема и
  нормализация OpenRouter перевешивают; прямой адаптер — позже за тем же трейтом.

## Out of scope

См. Non-goals. Дополнительно: смена формата MCP-ответов, ANN-индекс (`usearch`) —
поиск остаётся BM25 top-k + rerank; мультимодальные документы OpenRouter-rerank.

## Open questions

- OQ-1 (закрывает P0-spike): точные лимиты docs/call, resizable-флаг qwen3-0.6b,
  нормированность векторов, таблица кодов ошибок. Без ответов — блокируется T2.
- OQ-2: значение `HTTP-Referer`/`X-Title` для нашего приложения (предложение:
  URL репо + `docsbase-memory-mcp`).
