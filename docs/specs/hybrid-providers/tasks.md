# Tasks: hybrid-providers (итерация 1)

**Goal:** OpenRouter embed + rerank за флагом, дефолт офлайн.
**Architecture:** `src/hybrid/` — один wire-клиент, sync-трейты (`Embedder` существующий + новый `Reranker`).
**Spec:** `docs/specs/hybrid-providers/{requirements,design}.md`
**Global constraints:** constitution 1.2.0; `cargo fmt`, `clippy -D warnings`; offline-дефолт;
сеть только за `hybrid-remote` + `hybrid=true`; ключ только env.

**Review focus:**
1. Схема OpenRouter 1-в-1 с фикстурами (дрейф = явная ошибка, не молчание).
2. Ничего не пишется в кэш при невалидном батче (целый батч или ничего).
3. Ключ нигде в логах/Debug/status.
4. Дефолтная сборка без `reqwest` в дереве.

## T1 — P0-spike: живые факты OpenRouter (код не храним)

**Goal:** закрыть OQ-1 (модели, лимиты, нормированность, коды ошибок).
**Files:** временные (curl/python-скрипт в `/tmp`, не в репо).
**Steps:**
1. `GET /embeddings/models`: наличие `qwen/qwen3-embedding-0.6b` (dim/ctx/resizable),
   наличие `cohere/rerank-v3.5` (или v4?) в rerank-роутере.
2. Embed-батч 3–5 текстов: сверить `data[].embedding` (длина = dim?), L2-норму векторов.
3. Rerank 4 доков: форма `results`, поведение `top_n`, сортировка.
4. Провоцировать ошибки (плохая модель, перелимит): таблица кодов → retry-политика.
5. Записать ответы как фикстуры `tests/fixtures/openrouter/*.json` (это — хранимый артефакт!).
6. Выход: таблица фактов (модели/dim/limits/нормированность/retry) — в requirements OQ-1.
**Acceptance:** OQ-1 закрыт числами; фикстуры в репо.
**Verify:** ручной прогон, ревью таблицы. **Depends:** ничего. **Blocks:** T2.

## T2 — Клиент: `src/hybrid/` + wire-код

**Goal:** `OpenRouterEmbedder` + `OpenRouterReranker`, чанкинг, retry, L2, I3-гард (FR-1/2/6/7/8/9/10).
**Files:**
- Create: `src/hybrid/mod.rs` (трейт `Reranker`, `from_config`, реэкспорт)
- Create: `src/hybrid/openrouter.rs` (клиент, DTO, чанкинг, retry, L2)
- Modify: `src/lib.rs` (`pub mod hybrid`, за `#[cfg(feature = "hybrid-remote")]`)
- Modify: `Cargo.toml` (`[features] hybrid-remote = ["dep:reqwest"]`, optional dep `reqwest`)
- Test: `tests/hybrid_openrouter.rs` (wire на фикстурах T1: схемы, ошибки, порядок, L2, I3)
**Interfaces:**
- Consumes: факты T1 (лимиты, retry-таблица, L2-дефолт), `Embedder` (`vector_cache.rs:39`).
- Produces: `Reranker`-трейт; `OpenRouterClient` с `base_url`-полем.
**Steps (TDD):**
1. Красный: тесты чанкинга (128 + byte-budget, одиночный переполненный), склейки порядка,
   суммы usage, L2-нормы, I3-гварда (вызов внутри runtime → ошибка), redact ключа.
2. Зелёный: DTO (4 поля + flatten extra), `embed_texts`/`rerank`, retry по таблице T1,
   заголовки `HTTP-Referer`/`X-Title` (значения — OQ-2, дефолт предложить в PR).
3. Wire-тесты на фикстурах T1 (все ветки ошибок).
4. `clippy -D warnings`, `fmt`; коммит.
**Acceptance:** FR-1/2/6/7/8/9/10; SC-5.
**Verify:** `cargo test --locked --features hybrid-remote --test hybrid_openrouter` → ok;
`cargo tree -e normal --no-default-features | grep -c reqwest` → 0.
**Depends:** T1.

## T3 — Конфиг `[hybrid.*]`

**Goal:** TOML-секции + валидация (FR-3/4/5).
**Files:**
- Modify: `src/config/mod.rs` (`HybridEmbedding {provider, model, dimensions}`,
  `HybridReranker {provider, model, top_n}`, валидация provider==openrouter, top_n>0)
- Test: extend `tests/config_runtime.rs` (SC-3: нет ключа → `Admission`; чужой provider → `Admission`)
**Interfaces:** Consumes: `from_config`-контракт T2. Produces: `HybridConfig`.
**Steps:** тесты → реализация → зелёный → коммит.
**Acceptance:** FR-3/4/5; SC-3.
**Verify:** `cargo test --locked --test config_runtime` → ok. **Depends:** T2 (контракт).

## T4 — Встраивание: кэш + поиск

**Goal:** embed → `embed_missing`, rerank → поверх BM25 top-k (G1/G2).
**Files:**
- Modify: `src/index/job.rs` (remote-embedder за `hybrid`-флагом; `model_id@d`)
- Modify: search-путь `src/daemon/tools.rs` (rerank top-k за флагом; k ≥ top_n)
- Test: extend cache-тесты (SC-1: второй прогон — 0 HTTP через счётчик транспорта);
  synthetic rerank-тест (SC-2)
**Interfaces:** Consumes: T2 (клиент), T3 (`HybridConfig`).
**Steps:** тесты → подключение → зелёный → коммит.
**Acceptance:** SC-1, SC-2; I1 (невалидный батч не пишет ничего — тест).
**Verify:** `cargo test --locked --features hybrid-remote --test index_job` → ok (+ новый rerank-тест).
**Depends:** T2, T3.

## T5 — Регрессия и гейты

**Goal:** дефолт untouched (G4, SC-4).
**Files:** без кода (проверки) + `tests/fixtures/openrouter/` уже из T1.
**Steps:**
1. `cargo test --locked` (вся матрица, без features) → ok.
2. `cargo test --locked --test offline` → ok.
3. `cargo tree` дефолтной сборки без `reqwest`.
4. Ручной `#[ignore]` live-smoke за `OPENROUTER_API_KEY` (1 батч + 1 rerank) — отчёт в PR.
**Acceptance:** SC-4; `search_golden`/`tokenizer` зелёные.
**Verify:** команды выше + приложить выводы. **Depends:** T4.

## T6 — Доки: ADR-14 + таблицы

**Goal:** зафиксировать решения (A1–A5) и зависимости.
**Files:**
- Modify: `docs/specs/docsbase-memory-mcp/design.md` (§7: +`reqwest` строка с rationale; §8: `hybrid-remote`)
- Modify: `docs/specs/hybrid-providers/requirements.md` (OQ-1 → факты, `ADDED`-маркером)
- (ADR-14 живёт в design §12 продукта + §12 этого design — дублировать кратко в обоих)
**Acceptance:** traceability: каждая строка §7 → FR; триггер async-пересмотра записан.
**Verify:** ревью диффа. **Depends:** T5 (числа замеров NFR-1 вписать сюда).
