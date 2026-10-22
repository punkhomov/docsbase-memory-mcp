# Tasks: hybrid-providers (итерация 1)

**Goal:** OpenRouter embed + rerank за флагом, дефолт офлайн.
**Architecture:** `src/hybrid/` — wire-клиент (`Transport`-трейт), sync-трейты
(`Embedder` существующий + новый `Reranker`).
**Spec:** `docs/specs/hybrid-providers/{requirements,design}.md` (ревизия 2.2)
**Global constraints:** constitution 1.2.0; `cargo fmt`, `clippy -D warnings`;
offline-дефолт; сеть только за `hybrid-remote` + hybrid enabled; ключ только env.

**Review focus:**
1. Схема OpenRouter 1-в-1 с фикстурами (дрейф = явная ошибка, не молчание).
2. Ничего не пишется в кэш при невалидном батче (целый батч или ничего).
3. Ключ нигде в логах/Debug/status; base_url только из allowlist.
4. Дефолтная сборка без `reqwest` в дереве; feature-off + hybrid=true → Admission.
5. Ключ кэша включает dim и normalize (SC-1).

## T1 — P0-spike: живые факты OpenRouter (код не храним)

**Goal:** закрыть OQ-1 (модели, лимиты, нормированность, коды ошибок).
**Files:** временные (`/tmp`, не в репо); выход — фикстуры `tests/fixtures/openrouter/*.json`.
**Требование:** `OPENROUTER_API_KEY` в env у исполнителя (оператор); без ключа T1 blocked.
**Steps:**
1. `GET /api/v1/embeddings/models`: наличие `qwen/qwen3-embedding-0.6b` (dim/ctx),
   кандидаты на fallback (`qwen3-embedding-8b`/`pplx-embed-v1-0.6b`/`voyage-4-lite`);
   наличие `cohere/rerank-v3.5` (и `cohere/rerank-4-pro` — зафиксировать ctx).
2. Embed-батч 3–5 RU/EN текстов (mix languages): длина вектора (= dim?), L2-норма
   (дефолт FR-8 подтверждается или флипается `DELTA`-маркером), поведение
   `dimensions=1024` (MRL truncation), `input_type=search_document` vs без него.
3. Rerank 4 доков (и кейс пустого `documents`): форма `results[].{index,
   relevance_score}`, семантика `top_n` (возвращает ли ровно N; что при N > числа
   документов), сортировка, `usage.search_units`.
4. Провоцировать ошибки (плохая модель, пустой input, перелимит): фактическая
   таблица кодов → уточнить политику FR-7 (413/524/529/`Retry-After`).
5. Записать реальные ответы как фикстуры `tests/fixtures/openrouter/{embeddings,rerank,errors}/*.json`
   (это хранимый артефакт).
6. Выход: таблица фактов (модели/dim/limits/нормированность/retry) — в
   requirements, `DELTA`-маркером.
**Acceptance:** OQ-1 закрыт числами; фикстуры в репо.
**Verify:** ручной прогон, ревью таблицы. **Depends:** ничего. **Blocks:** T2.

## T2 — Клиент: `src/hybrid/` + wire-код

**Goal:** `OpenRouterEmbedder` + `OpenRouterReranker`, `Transport`, чанкинг, retry, L2, ключ FR-5, I3-гард.
**Files:**
- Create: `src/hybrid/mod.rs` (**ungated**: трейт `Reranker` без `model_id`,
  `blocking::{run_async,run_sync}`, реэкспорты)
- Create: `src/hybrid/config.rs` (**ungated**: `HybridCfg`/`EmbeddingCfg`/`RerankerCfg`,
  `from_config(cfg, feature_on)`, `HybridStages`)
- Create: `src/hybrid/wire.rs` (gated: DTO, `Transport`, retry/backoff, чанкинг)
- Create: `src/hybrid/openrouter.rs` (gated: клиент, L2, ключ FR-5, заголовки FR-9/OQ-2)
- Modify: `src/lib.rs` (`pub mod hybrid` всегда; `wire`/`openrouter` — за `#[cfg(feature)]`)
- Modify: `Cargo.toml` (`[features] hybrid-remote = ["dep:reqwest"]`; reqwest
  `default-features = false, features = ["rustls-tls-webpki-roots", "json"]`)
- Modify: `docs/specs/docsbase-memory-mcp/design.md` (§7/§8 + ADR-14 — **в этом же
  PR, до/вместе с `reqwest`**: конституция требует ADR + строку зависимостей)
- Test: `tests/hybrid_openrouter.rs` (`#![cfg(feature = "hybrid-remote")]`,
  Transport-мок на фикстурах T1)
**Interfaces:**
- Consumes: факты T1, `Embedder` (`src/vector_cache.rs:39`).
- Produces: `Reranker`-трейт; `OpenRouterClient` с `base_url`-полем.
**Steps (TDD):**
1. Красный: тесты чанкинга (128 + byte-budget, одиночный переполненный), склейки
   порядка, L2-нормы, ключа FR-5 (`@d1024@l2`), `blocking`-контракта (прямой вызов →
   `Internal`; `run_sync` внутри runtime → ошибка), redact ключа, маппинга кодов
   ошибок (фикстуры T1), `from_config` feature-off+enabled → `Admission` (ungated).
2. Зелёный: DTO, `embed_texts`/`rerank`, retry, заголовки FR-9.
3. Проверить allowlist (base_url вне списка → ошибка).
4. `clippy -D warnings`, `fmt`; коммит.
**Acceptance:** FR-1/2/5/6/7/8/9/10; SC-5; OQ-2 закрыт (константы заголовков в PR).
**Verify:** `cargo test --locked --features hybrid-remote --test hybrid_openrouter` → ok;
`cargo test --locked` (дефолт, ungated-часть) → ok; `cargo tree` (no-default) без reqwest.
**Depends:** T1.

## T3 — Конфиг: untagged `hybrid` + feature-off

**Goal:** TOML-формы (FR-13), резолв конфига, Admission-ошибки (FR-3/14).
**Files:**
- Modify: `src/config/mod.rs` (untagged `HybridToggle` → десериализация прямо в
  `hybrid::config::HybridCfg`; резолв в `Config.hybrid`/`Config.hybrid_cfg`;
  `deny_unknown_fields` сохраняется)
- Modify: `src/cli/mod.rs` (`docsbase config` JSON: hybrid-ступени)
- Test: extend `tests/config.rs` (bool-форма совместима; таблица; `enabled`
  отсутствует → true; обе секции опциональны; `window` вне 10..=100 → Admission;
  чужой provider → Admission; feature-off + enabled → Admission; JSON `docsbase
  config` содержит ступени)
**Interfaces:** Consumes: `from_config`-контракт T2. Produces: `HybridCfg`.
**Steps:** тесты → реализация → зелёный → коммит.
**Acceptance:** FR-13/14; SC-3. **Verify:** `cargo test --locked --test config` → ok.
**Depends:** T2 (контракт).

## T4 — Встраивание: backfill + поиск + per-request rerank

**Goal:** embed → sync/backfill, rerank → поверх BM25 top-k (G1/G2, FR-11/12/15).
**Files:**
- Modify: `src/index/job.rs` (remote-embedder за hybrid; backfill недостающих;
  `embed_coverage` в статус проекта; вызовы через `hybrid::blocking::run_async`)
- Modify: `src/daemon/tools.rs` (выделить seam `search_docs_with(stages, …)`;
  rerank окна `max(limit, cfg.window)` ≤ числа хитов, FR-11; `score` = `relevance_score`)
- Modify: `src/daemon/server.rs` (открытие проекта: feature/hybrid-проверка,
  запуск backfill sync-джобы, проброс `Arc<HybridStages>` в search-путь)
- Modify: `src/mcp/tools.rs` (опциональный параметр `rerank` в схеме `search_docs`)
- Modify: `src/cli/search.rs` (direct-режим через тот же `search_docs_with`)
- Test: cache/`embed_missing` (SC-1, SC-6), `tests/hybrid_search.rs` с fake-стадиями
  через seam (SC-2, `rerank=false`), пустой BM25-список → rerank не вызывается
**Acceptance:** SC-1/2/6; I1 (невалидный батч — ничего не записано).
**Verify:** `cargo test --locked --features hybrid-remote` → ok. **Depends:** T2, T3.

## T5 — CI-джоба feature-сборки + регрессия

**Goal:** дефолт untouched (G4, SC-4), feature-путь собирается и тестируется в CI.
**Files:**
- Modify: `.github/workflows/ci.yml` (+ job `linux-hybrid`: `cargo test --locked
  --features hybrid-remote`; без сети — только фикстуры)
- Без кода продукта.
**Steps:**
1. `cargo test --locked` (дефолт) → ok.
2. `cargo test --locked --test offline` → ok.
3. `cargo tree` дефолтной сборки без `reqwest` (проверка).
4. Ручной `#[ignore]` live-smoke за ключом (1 батч + 1 rerank) — отчёт в PR.
5. Live-замер rerank on/off на golden (recall@k/nDCG) — числа в PR (NFR-1,
   конституция: ранжирование меняется только с замером).
**Acceptance:** SC-4; `search_golden`/`tokenizer` зелёные. **Depends:** T4.

## T6 — Доки: ADR-14 + таблицы

**Goal:** зафиксировать решения (A1–A5) и зависимости.
**Files:**
- Modify: `docs/specs/docsbase-memory-mcp/design.md` (ADR-13 — dated delta:
  пин `model_id` расширен до dim/normalize, ревизия весов — принятое ограничение
  A3/R2; ADR-14 — краткая копия §13 этой спеки, если не вошла в T2)
- Modify: `docs/specs/hybrid-providers/requirements.md` (OQ-1 → факты, `DELTA`-маркером)
- Modify: `docs/CONFIG.md` (hybrid-секции, `window`, примеры), README (короткий
  раздел opt-in + `OPENROUTER_API_KEY`)
**Acceptance:** traceability: §7 → FR; триггер async-пересмотра записан.
**Verify:** ревью диффа. **Depends:** T5.

## T7 — Dense-скан: `hybrid/dense.rs` (TDD)

**Goal:** косинусный top-`fetch` по переданному набору `(chunk_id, вектор)`.
**Files:** Create `src/hybrid/dense.rs`; Modify `src/hybrid/mod.rs`.
**Acceptance:** unit: порядок и скор на детерминированных векторах; `normalize=false`
(косинус без нормировки), пустой набор, меньше кандидатов чем fetch, стабильный
тай-брейк при равенстве (chunk_id возр.).
**Verify:** `cargo test` (unit). **Depends:** —.

## T8 — RRF: `hybrid/rrf.rs` (TDD)

**Goal:** `rrf_merge` по FR-16 (k=60, равные веса, тай-брейки).
**Files:** Create `src/hybrid/rrf.rs`; Modify `src/hybrid/mod.rs`.
**Acceptance:** unit: ручной пример математики k=60; вхождение в оба списка >
одного; тай-брейки (вхождения, ранг, BM25-скор, id); пустые списки.
**Verify:** unit. **Depends:** —.

## T9 — Поколение индекса + `repo::chunk_hashes` + dense-кэш

**Goal:** маркер ревизии и ленивый кэш (DELTA-7).
**Files:** Modify `src/store/mod.rs` (схема+миграция), `src/store/repo.rs`,
`src/index/job.rs` (инкремент в R2-транзакции), `src/daemon/server.rs`/`Shared`,
`src/hybrid/dense.rs`.
**Acceptance:** тест: поколение растёт ровно с успешным применением плана (сбой
провайдера/коммита не инкрементит); `chunk_hashes` согласован с покрытием; кэш:
hit / rebuild при новом поколении / cap-деградация.
**Verify:** `cargo test`. **Depends:** T7.

## T10 — Конфиг `ranking` + wiring поиска (FR-17/FR-18)

**Goal:** сквозной путь lexical/rrf через `search_docs_with` и MCP.
**Files:** Modify `src/hybrid/config.rs`, `src/config/mod.rs`, `src/daemon/tools.rs`,
`src/daemon/server.rs` (seam), `src/mcp/tools.rs` (docs), `src/cli/search.rs`.
**Acceptance:** `tests/hybrid_ranking.rs`: lexical не меняется; rrf; rrf+rerank
(relevance, top_n=window); rerank=false при rrf → RRF-порядок; rrf без embedding →
Admission; неизвестное значение → ошибка конфига; частичное/нулевое покрытие;
fetch=окно. MCP-схема без изменений.
**Verify:** `cargo test --test hybrid_ranking`. **Depends:** T7–T9.

## T11 — Доки + live-замер (BM25 vs RRF vs RRF+rerank)

**Goal:** ADR-дельта, CONFIG.md (ranking, семантика score), README; live-замер
на golden — отчёт в ledger.
**Files:** Modify `docs/specs/docsbase-memory-mcp/design.md` (ADR-14 delta),
`docs/CONFIG.md`, `README.md`/`README.ru.md`, `tests/hybrid_live.rs`.
**Acceptance:** замер прогнан; выводы и цифры в `progress.md`; доки консистентны.
**Verify:** live `#[ignore]` прогон. **Depends:** T10.

## T12 — Итоговое ревью ветки + фикс-раунд

**Goal:** свежий ревьюер (whole-branch), фиксы Critical/Important, scoped re-review.
**Depends:** T11.

## T13 — VectorCache capacity/pinning for ANN (DELTA-9)

**Status:** partial (2026-10-22): default `max_bytes` raised to 1 GiB with a
guard test; `[hybrid.cache] max_bytes` is parsed, validated and threaded
through backfill/coverage/dense; per-project lease/pin remains open.

**Goal:** дать ANN-build стабильные 100% coverage на 75k×2048: настраиваемый
byte limit + per-project lease/pin.

**Files:**
- Modify: `src/vector_cache.rs` (`CacheLimits`/config), `src/hybrid/jobs.rs`
  (backfill lease), `src/hybrid/dense.rs` (build lease), `docs/CONFIG.md`.
- Test: `tests/vector_cache.rs`, `tests/hybrid_ann.rs`, scale test T10 in
  ann-index.

**Steps (TDD):**
1. RED: 75k×2048 backfill с дефолтом 256 MiB → coverage <100%; после lease/pin
   + 1 GiB — coverage=100%.
2. GREEN: configurable `max_bytes` (default 1 GiB); lease/pin, который
   запрещает eviction векторов проекта на время backfill+ANN build; release
   после `READY`/fallback.
3. Coverage gate: `cached < total` → ANN не строится, причина в status/логе.
4. Не менять публичный `search_docs`/MCP; `[hybrid.cache]` — единственное
   новое исключение, задокументировать.
5. Если ключ становится публичным — записать ADR-16 (capacity/pinning
   policy) в `docs/specs/docsbase-memory-mcp/design.md`.

**Acceptance:** DELTA-9; ann-index Delta 0.2/T10 coverage=100%; eviction
другого проекта не ломает активный build.

**Verify:** `cargo test --locked --test vector_cache`;
`cargo test --locked --test hybrid_ann`; ann-index scale test.

**Depends:** T4/T9; blocks ann-index T9/T10.
