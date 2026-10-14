# Technical Design: hybrid-providers (итерация 1)

**Status:** Draft, ревизия 2.1 · **Дата:** 2026-10-14 (черновик — 2026-10-08)
**Spec:** `docs/specs/hybrid-providers/requirements.md` (ревизия 2)

**Delta T1-спайка (2026-10-14):** дефолт — `qwen/qwen3-embedding-8b` @1024
(`qwen3-embedding-0.6b` в каталоге отсутствует); qwen3-8b нормирует вектора
(`normalize=true` идемпотентен); `input_type` — минимальный эффект; rerank
подтверждён живьём (top_n клампится, пустые documents → 400 → пропуск вызова);
коды ошибок подтверждены (404/400, конверт `{"error":{message,code}}`).

**Delta ревизии 2:** base_url `/api/v1` без удвоения, `rustls-tls-webpki-roots`
(не manual-roots), ключ кэша с dim+normalize, `from_config` поверх untagged
TOML-формы, feature-off → `Admission` на композиции, backfill через sync-джобу,
fusion-контракт, transport-трейт для офлайн-фикстур, per-request `rerank`.

## 1. Overview

Тонкий OpenAI-совместимый HTTP-клиент к двум ручкам OpenRouter за cargo-feature
`hybrid-remote` (первый `[features]` в `Cargo.toml`). Реализации садятся на
существующий sync-контракт (`Embedder`), вводят `Reranker`; `VectorCache` не
меняется структурно — меняется композиция `model_id` (FR-5). Реализует FR-1…FR-15.

## 2. Global constraints

Из constitution 1.2.0 + design продукта (§2, §8):
- Дефолт — полный офлайн; сеть только при явно включённых ступенях.
- `hybrid-remote` — opt-in feature; дефолтная сборка: ноль сетевых крейтов, 40 MiB цел.
- `unwrap`/`expect` запрещены в lib-коде; `thiserror` в libs, `anyhow` на границе.
- Тяжёлое/сетевое — только через `spawn_blocking` (ADR-7); tokio-воркеры не блокировать.
- Ключи — только env; allowlist endpoint — ровно `https://openrouter.ai/api/v1`.

## 3. Architecture

```mermaid
flowchart LR
    J[index job / sync job / search] -->|spawn_blocking| H[src/hybrid]
    H --> E[OpenRouterEmbedder: Embedder]
    H --> R[OpenRouterReranker: Reranker]
    E --> W[transport: POST {base}/embeddings]
    R --> W2[transport: POST {base}/rerank]
    E --> C[(vectors.db via VectorCache)]
    W --> OR([openrouter.ai/api/v1])
    W2 --> OR
    Cfg[.docsbase.toml hybrid + OPENROUTER_API_KEY] --> H
```

Поток индексации: sync-джоба → `embed_missing` (весь проект, content-addressed)
→ cache-miss батч → `OpenRouterEmbedder::embed` → валидация (count, единый dim)
→ L2 (если `normalize`) → `put`; батчи последовательные, вызовы — только через
`hybrid::blocking` (I3). Поток поиска: BM25-список → окно `window` →
`rerank` → порядок провайдера, срез до `limit`. Backfill (FR-15): открытие
проекта (хук — `daemon::server`) → sync-джоба; `embed_coverage` — `status`.

## 4. Components

| Компонент | Сборка | Ответственность | FR |
|---|---|---|---|
| `hybrid::config` | ungated | `HybridCfg`/`EmbeddingCfg`/`RerankerCfg`; `from_config(&HybridCfg, feature_on: bool) -> Result<HybridStages>`; feature-off+enabled → `Admission`; provider != openrouter → `Admission` | FR-3, FR-14 |
| `hybrid` (mod) | ungated | трейт `Reranker` (без `model_id` — rerank не кэшируется, YAGNI); `blocking` — единственный санкционированный sync-вход (маркер + thread-local) | FR-10 |
| `hybrid::wire` | `hybrid-remote` | DTO, `trait Transport` (reqwest + mock), маппинг кодов, retry/backoff, чанкинг, заголовки | FR-1, FR-2, FR-6, FR-7, FR-9 |
| `hybrid::openrouter` | `hybrid-remote` | `OpenRouterClient { base_url, api_key (redacted Debug), t }`; `embed`, `rerank`; L2; композиция `model_id` | FR-1, FR-2, FR-5, FR-8 |
| `config` (extend) | ungated | untagged `HybridToggle` (bool \| таблица) → десериализация в `hybrid::config::HybridCfg`; `Config.hybrid: bool` + `Config.hybrid_cfg` | FR-13 |
| `index::job` (modify) | ungated | `embed_missing` с remote-embedder (батчи последовательные); backfill-прогон; `embed_coverage` | FR-1, FR-15 |
| `daemon::server` (modify) | ungated | открытие проекта: feature/hybrid-проверка, запуск backfill-джобы, проброс `Arc<HybridStages>` в search-путь | FR-14, FR-15 |
| `daemon::tools` (modify) | ungated | `search_docs_with(stages, …)` — seam для тестов; rerank BM25-окна за флагом; `score` = `relevance_score` | FR-11, FR-12 |
| `cli/search.rs` (modify) | ungated | direct-режим использует тот же `search_docs_with`-seam | FR-11 |
| `mcp/tools` (modify) | ungated | опциональный `rerank: bool` в схеме `search_docs` | FR-12 |

`base_url = https://openrouter.ai/api/v1` (дефолт, поле для будущего
`custom`-пресета); пути вызовов — `{base}/embeddings`, `{base}/rerank`. Allowlist
проверяет origin + path-prefix ровно (`NFR-3`).

## 5. Module interfaces

```rust
// src/hybrid/config.rs — UNGATED (компилируется в обеих сборках; FR-14 тестируем в дефолте)
pub struct HybridCfg { pub enabled: bool, pub embedding: Option<EmbeddingCfg>, pub reranker: Option<RerankerCfg> }
pub struct EmbeddingCfg { pub provider: String, pub model: String, pub dimensions: u32, pub normalize: bool }
pub struct RerankerCfg { pub provider: String, pub model: String, pub window: u32 }
pub struct HybridStages { pub embedder: Option<Arc<dyn Embedder>>, pub reranker: Option<Arc<dyn Reranker>> }
pub fn from_config(cfg: &HybridCfg, feature_on: bool) -> Result<HybridStages>;
pub fn cache_model_id(cfg: &EmbeddingCfg) -> String;   // FR-5, ungated → тестируем в дефолте

// src/hybrid/mod.rs — UNGATED
pub trait Reranker: Send + Sync {   // model_id нет: rerank не кэшируется (YAGNI)
    fn rerank(&self, query: &str, docs: &[String], top_n: usize) -> Result<Vec<(usize, f32)>>;
}
pub mod blocking {
    // Единственный санкционированный sync-вход (I3): ставит thread-local
    // маркер и восстанавливает его после выхода. Daemon уже гоняет все
    // tool-вызовы через spawn_blocking (daemon::server::route), watcher —
    // собственный поток, direct CLI — главный поток: отдельные
    // run_async/run_sync не нужны (архитектурная сверка, DELTA-T2).
    pub fn enter<T>(f: impl FnOnce() -> Result<T>) -> Result<T>;
    pub fn in_context() -> bool;
}

// src/hybrid/wire.rs — feature-gated: DTO + transport
pub struct TransportResponse { pub status: u16, pub body: String }
pub trait Transport: Send + Sync {
    fn post_json(&self, url: &str, headers: &[(String, String)], body: &str) -> Result<TransportResponse>;
}
pub fn post_with_retry(transport: &dyn Transport, url: &str,
                       headers: &[(String, String)], body: &str) -> Result<TransportResponse>;
pub fn embed_batches(texts: &[String]) -> Vec<Range<usize>>;  // ≤128 текстов / 512 KiB
// src/hybrid/openrouter.rs — feature-gated
pub struct OpenRouterClient { /* base_url, api_key (redacted Debug), transport: Box<dyn Transport> */ }
impl OpenRouterClient {
    pub fn new(base_url: &str, api_key: &str, transport: Box<dyn Transport>) -> Result<Self>; // allowlist
    pub fn from_env() -> Result<Self>;                                                         // OPENROUTER_API_KEY
    pub fn embed_texts(&self, model: &str, dimensions: u32, normalize: bool, texts: &[String]) -> Result<Vec<Vec<f32>>>;
    pub fn rerank(&self, model: &str, query: &str, docs: &[String], top_n: usize) -> Result<Vec<(usize, f32)>>;
}
pub struct OpenRouterEmbedder { /* client, cfg, model_id */ }   // impl Embedder; with_client/new
pub struct OpenRouterReranker { /* client, cfg */ }             // impl Reranker;  with_client/new
pub struct ReqwestTransport { /* reqwest::Client + fallback runtime */ } // impl Transport
// DTO: обязательные поля required, deny_unknown_fields не ставим (дрейф провайдера —
// явная ошибка парсинга только на несовместимости обязательных полей); ответы
// валидируются (count/dim/index-range), сортируются повторно.
// model_id (FR-5): format!("openrouter/{model}@d{dim}@{post}"), post ∈ {"l2","raw"}

// src/daemon/tools.rs — seam для интеграционных тестов (SC-2, T4)
pub fn search_docs_with(stages: &HybridStages, index: &ReadIndex, db: &Db,
                        query: &str, limit: usize, rerank: bool) -> Result<SearchPayload>;
```

**DELTA-T2 (2026-10-14, реализовано):** `blocking::enter` заменяет пару
`run_async`/`run_sync` (см. выше); `Transport` возвращает `{status, body}`
(нужны коды для FR-7); ретрай — 4 попытки, 250 мс ×2 (кап 4 с) + джиттер ≤50 мс;
`ReqwestTransport` переиспользует ambient runtime на blocking-потоках демона и
владеет current-thread runtime в CLI/тестах; ключ закрыт `ApiKey(***)` в `Debug`.

Инварианты: I1 — ответ короче/длиннее запроса или с разным dim → `Internal`,
в кэш ничего не пишется (батч целиком). I2 — ключ никогда в `Debug`/логах
(`redacted`). I3 — `embed`/`rerank` вызываются только через `hybrid::blocking`
(thread-local маркер; прямой вызов из любого контекста → явная `Internal`;
`run_sync` дополнительно падает вне runtime-контракта, `run_async` делает
`spawn_blocking`); тесты покрывают оба отказа (прямой вызов из async-контекста,
вызов `run_sync` внутри runtime).

## 6. Data flow

**Embed (фон).** Miss-батч (≤128 текстов / byte-budget) → один POST
`{model, input[], dimensions, input_type:"search_document"}` → проверка
count+dim → L2 (если `normalize`) → `put` под ключом FR-5. Баты последовательные
(параллелизм — оптимизация следующих итераций по данным замеров, NFR-1); вызовы —
только через `hybrid::blocking::run_async` (I3). Ошибка сети → ошибка job'а
(без частичной записи), job помечает проект, не валит daemon.

**Backfill (FR-15).** Открытие проекта (хук — `daemon::server`) с hybrid →
sync-джоба (FR-17) поверх всех чанков; `embed_missing` берёт только cache-miss
(content-addressed). `embed_coverage = cached/total` в `status`; эвикция LRU
может держать покрытие < 100% (R7) — повторный sync до-embed'ит выпавшее. Поиск
в процессе — обычный BM25(+rerank).

**Rerank (запрос).** Финальный BM25-список → окно `window = max(limit,
cfg.window)`, но ≤ числа хитов; ноль кандидатов → rerank не вызывается (пустой
ответ как у BM25). Один POST `{model, query, documents, top_n: window}` (documents —
`text` чанков) → `results[].{index, relevance_score}` → маппинг на исходные чанки
→ порядок провайдера → срез до `limit` → join с SQLite как раньше; `score` =
`relevance_score` (FR-11). `rerank=false` (per-request или конфиг) — strict
BM25-порядок. Оба режима тестируются через seam `search_docs_with` с мок-стадиями.

## 7. Dependencies + rationale

| Крейт (feature `hybrid-remote`) | Зачем | Альтернатива — почему нет |
|---|---|---|
| `reqwest` (`default-features = false`, `rustls-tls-webpki-roots`, `json`) | async HTTP, пул, таймауты; sync-обёртка через `block_on` в `spawn_blocking` | `rustls-tls-manual-roots` — **без рутов не верифицирует ничего** (блокер ревью, исправлено); `native-roots` — зависимость от системного стора; `ureq` — второй стек; `rig`/`async-openai` — чужие API-дизайны |
| `serde`/`serde_json` | уже в дереве | — |

`tokio` уже есть. `fastembed`/`usearch`/`sqlite-vector-rs` — будущие итерации.
`webpki-roots` — статические Mozilla-руты, офлайн-сборка (CI собирает `--locked`).

## 8. Required / Forbidden stack

- Required: `reqwest` — только под `hybrid-remote`, только rustls (static-pie, без OpenSSL).
- Forbidden: ключ в toml/CLI-флагах/логах; base URL вне allowlist; sync-вызов
  embedder из async-контекста напрямую (I3); тихий fallback на BM25 при ошибке ступени.

## 9. Error handling

| Ошибка | Поведение |
|---|---|
| Нет `OPENROUTER_API_KEY` при hybrid enabled | `Admission` с инструкцией (SC-3) |
| Сборка без feature при hybrid enabled | `Admission` «пересоберите с `--features hybrid-remote`» (FR-14) |
| 400 (bad request; напр. input не того типа) | `Internal` с телом (обрезанным), без ретраев |
| 401/403 (ключ/доступ) | `Internal` с hint про `OPENROUTER_API_KEY`, без ретраев |
| 402 (payment required) | `Internal` с балансовым hint, без ретраев |
| 404 (модель не найдена) | `Internal` с именем модели, без ретраев |
| 413 (payload too large) | `Internal`, без ретраев (батч-лимиты должны исключать) |
| 429 (`Retry-After`) / 5xx / 524 / 529 / таймаут | backoff с джиттером, N попыток (из спайка), затем `Internal` |
| Schema-mismatch ответа | `Internal` «несовместимая схема OpenRouter», батч не пишется |
| `provider != openrouter` | `Admission` (итерация 1) |

## 10. TOML-форма (FR-13)

`FileConfig.hybrid: Option<HybridToggle>`:

```rust
#[derive(Deserialize)]
#[serde(untagged)]
enum HybridToggle { Bool(bool), Table(HybridTable) }

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct HybridTable { #[serde(default = "def_true")] enabled: bool,
                     #[serde(default)] embedding: Option<HybridEmbedding>,
                     #[serde(default)] reranker: Option<HybridReranker> }
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct HybridEmbedding { #[serde(default = "def_provider")] provider: String,
    #[serde(default = "def_emb_model")] model: String,
    #[serde(default = "def_dim")] dimensions: u32, #[serde(default = "def_true")] normalize: bool }
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct HybridReranker { #[serde(default = "def_provider")] provider: String,
    #[serde(default = "def_rr_model")] model: String, #[serde(default = "def_window")] window: u32 }
```

Резолв: `hybrid = true` → `HybridCfg { enabled: true, обе ступени с дефолтами }`;
таблица — как есть; отсутствующая секция ступени = ступень выключена; `enabled`
отсутствует → `true` (наличие таблицы = намерение включить). Слияние
глобального/проектного конфига — замена значения целиком (существующее
precedence). Существующие конфиги с `hybrid = true` остаются валидными
(`tests/config.rs` не меняется в части bool-формы). Валидация `window` —
10..=100 (вне границ — `Admission`).

## 11. Testing strategy

- Unit: чанкинг (count+bytes, переполненный одиночный), склейка порядка, сумма
  usage, L2-норма (property: норма ≈ 1), композиция `model_id` (FR-5), I3-гард,
  redact ключа в `Debug`, untagged TOML (bool + таблица + секции).
- Wire (без сети): `tests/hybrid_openrouter.rs` на `Transport`-моке с фикстурами
  реальных ответов OpenRouter (embeddings, rerank, каждая ветка ошибок);
  golden на маппинг `results[].index`.
- Gating: `tests/hybrid_openrouter.rs` — `#![cfg(feature = "hybrid-remote")]`,
  иначе дефолтный `cargo test --locked` не собирается.
- Integration: fake-embedder в `embed_missing` + счётчик вызовов: второй прогон —
  0 вызовов (SC-1); backfill и `embed_coverage` (SC-6); feature-off и отсутствие
  ключа (SC-3); seam `search_docs_with` с fake-стадиями: rerank меняет
  синтетический порядок, `rerank=false` возвращает исходный (SC-2); `blocking`-
  контракт (прямой вызов → `Internal`; `run_sync` внутри runtime → ошибка).
- Live (не в CI): `#[ignore]` smoke за `OPENROUTER_API_KEY` — 1 embed-батч + 1
  rerank; live-замер rerank on/off на golden (recall@k/nDCG) — числа в PR (NFR-1);
  ручной прогон перед merge.
- Regression: `search_golden`, `tokenizer`, `offline` — зелёные; `cargo tree`
  дефолтной сборки без `reqwest` (SC-4).

## 12. Risks & trade-offs

| # | Риск | Митигация |
|---|---|---|
| R1 | Дрейф схемы OpenRouter | фикстуры фиксируют схему; mismatch — явная ошибка |
| R2 | Тихий апдейт весов провайдера (кэш не инвалидируется) | принято (A3); позже пин провайдера/ревизии |
| R3 | Ненормированность векторов | Спайк: qwen3-8b нормирует (1.000), pplx raw; L2-флаг в ключе (FR-5/FR-8) |
| R4 | 503/529/429 при bulk-заливке | backoff + `Retry-After`; батчи последовательные (параллелизм — по данным замеров) |
| R5 | Блокировка executor'а | I3-гард с явной ошибкой |
| R6 | `webpki-roots` устарели для нового CA | руты обновляются редко; при инциденте — `native-roots` как fallback-конфиг |
| R7 | Эвикция LRU при backfill (50k×1024d f32 ≈ 195 MiB от 256 MiB) | `embed_coverage` в `status`; повторный sync до-embed'ит; кап настраиваемый |

## 13. Alternatives considered (ADR-14, кратко)

- **Прямой Cohere** вместо OpenRouter-rerank: точнее позиционирование, но второй
  ключ/биллинг/схема. Отклонено до итерации 2 (трейт `Reranker` уже позволяет).
- **`ureq` вместо `reqwest`:** проще для чистого sync, но tokio уже в дереве, а
  `reqwest` пригодится будущим async-путям; вес — только в feature.
- **Async-трейт:** отклонён (анализ 2026-10-08): выигрыша нет, редизайн дорог;
  триггер пересмотра — веерные per-request запросы с дедлайном.
- **`rustls-tls-manual-roots`:** отклонён ревью (нет рутов) — заменён на
  `webpki-roots`; `native-roots` — фоллбэк-вариант, не дефолт.
- **`encoding_format=base64`:** отложено (меньше payload, но decode-код и тесты);
  итерация 1 — `float`.
