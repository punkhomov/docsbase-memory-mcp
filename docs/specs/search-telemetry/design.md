# Search Telemetry — Design

> Спек: `requirements.md` (rev 0.2). Задачи: `tasks.md`.
> Предшественники: `ann-index/design.md` (тир ANN, `DenseRows`),
> `hybrid-providers/design.md` (стадии RRF/rerank, окно кандидатов).

## 1. Форма ответа

`search_docs_with` получает `explain: bool` из аргументов. При `false` путь
исполнения и сериализация **не меняются** (это гарантирует
`default_response_shape_is_the_bare_citation_array`, T0). При `true` тот же
`Vec<Value>` оборачивается: `{"hits": [...], "meta": {...}}`.

IPC не меняется: `Request`/`Response` несут непрозрачный JSON, форма задаётся
MCP-слоем и CLI. `PROTOCOL_VERSION` остаётся 1.

## 2. Трасса: где ставить `Instant`

Один аккумулятор `SearchTrace` создаётся в начале `search_docs_with` и передаётся
вниз по ссылке. Точки замера вложены, поэтому `sum(stages) <= total` выполняется
по построению, а не по удаче:

```
t0 = Instant::now()                     // вход в search_docs_with
├── bm25   : ReadIndex::open + index.search(query, fetch)
├── dense  : dense_candidates(...)      // embed_query + ANN/exact
├── rrf    : rrf_merge(...)             // только при Ranking::Rrf
├── rerank : apply_rerank(...)          // только при активном reranker
└── total  = t0.elapsed()               // в самом конце, после сборки rows
```

Стадия, которая не исполнялась, **не попадает** в словарь (отсутствие ключа ≠ 0):
`0` неотличим от «быстро», а `Option` в JSON превращается в `null` — оба варианта
хуже, чем просто отсутствие ключа.

`dense_candidates` получает `&mut SearchTrace`, чтобы записать причину отката
(`degraded`) в той же точке, где она возникла (`Building`, `TooLarge`,
`hits.len() < fetch`, ошибка поиска ANN). `eprintln!` остаётся: лог демона и
контракт ответа решают разные задачи.

## 3. `SearchMeta`

```rust
pub struct SearchMeta {
    pub score_scale: ScoreScale,        // bm25 | rrf | rerank
    pub stages: Vec<Stage>,             // в порядке исполнения
    pub stage_ms: StageMs,              // активные стадии + total, u64 мс
    pub window: usize,                  // фактический fetch после clamp
    pub index_generation: i64,          // repo::index_generation
    pub degraded: Vec<Degraded>,        // стабильные токены, пусто = норма
}
```

- `score_scale` выводится из **фактически исполненного** пути: `rerank_active &&
  reranker.is_some()` → `Rerank`; иначе `rrf_active` → `Rrf`; иначе `Bm25`.
  Вывод из конфига запрещён: `rerank: false` при включённом стадии — это `Rrf`.
- `window` — `fetch` после `limit.max(window).min(MAX_HITS)`
  (`daemon/tools.rs`), не запрошенный `window` и не `limit`.
- `index_generation` читается один раз на запрос.

## 4. `degraded` — токены, а не текст

```rust
pub enum Degraded { AnnBuilding, AnnInsufficient, AnnError, ExactFallback, NoRerankStage }
```

Строковые значения — часть контракта: `ann-building`, `ann-insufficient`,
`ann-error`, `exact-fallback`, `no-rerank-stage`. Добавлять можно, переименовывать
нельзя. Unit-тест фиксирует полное множество, чтобы случайное переименование
падало.

## 5. `AnnOutcome`: последний исход ANN

**Решение: запись живёт в `DenseCache`, а не в новом хранилище демона.**

Почему: `DenseCache` уже знает всё про тир (`entries`, `generation`, `digests`) и
уже проброшен и в `search_docs_with`, и в `route_search`. Если положить запись в
`Shared`, придётся менять публичную сигнатуру `search_docs_with` (её используют
десяток тестовых файлов) ради поля, которое принадлежит именно кэшу.

```rust
struct DenseCache {
    // ...
    outcomes: Arc<Mutex<HashMap<i64, AnnOutcome>>>,   // ≤ max_projects записей
}

pub struct AnnOutcome {
    pub state: AnnState,      // ready | building | too_large | fallback | stale
    pub reason: Option<String>,
    pub at: Instant,
}
```

- Путь поиска пишет исход в точке решения (`dense_candidates`).
- `status` читает его через `Option<&DenseCache>`; в CLI direct-режиме кэш есть
  (`LoadOnly`), в тестах можно передать `None` — тогда `ann.state == "disabled"`.
- TTL 1 час: запись старше → `stale`. Вытеснение — вместе с записями кэша
  (`max_projects`), плюс жёсткий предел: одна запись на проект.
- `vectors`/`estimated_bytes` берутся из резидентного `AnnIndex`
  (`ann.rs::count/dim/estimated_bytes`); если индекс не резидентен — `null`.
  Дисковых чтений на `status` нет.

`status` меняет сигнатуру: добавляется `dense: Option<&DenseCache>`. Это ломает
три тестовых вызова (`tests/cleanup.rs`), они обновляются в T5.

## 6. OTel-совместимость без зависимости

Таблица констант в `src/hybrid/meta.rs`:

| Стадия | `gen_ai.operation.name` | Атрибут длительности |
|---|---|---|
| bm25 | `retrieval` | `gen_ai.retrieval.bm25.duration_ms` |
| dense | `retrieval` | `gen_ai.retrieval.dense.duration_ms` |
| rrf | `retrieval` | `gen_ai.retrieval.fusion.duration_ms` |
| rerank | `rerank` | `gen_ai.rerank.duration_ms` |

Unit-тест утверждает, что у каждой стадии есть пара и пары уникальны. Экспортёр
не реализуется: появление `opentelemetry` тянет сетевой стек в офлайн-сборку.

## 7. Совместимость

- Дефолтный ответ не меняется — пиннится T0-тестом, который проходит до и после
  каждой задачи.
- `status` меняется аддитивно (новые ключи в объекте проекта); потребители,
  читающие `projects[i].status`, не затронуты.
- `baseline.json` не перегенерируется: `hits[]` не меняется ни на одном пути.
- `docs/MCP.md` обновляется в том же PR (T7), иначе гейт `docs` не поймает дрейф
  (он проверяет только наличие файлов).

## 8. Что осознанно не делается

- Отдельный `trace_id` — нет экспортёра, корреляция идёт по
  `index_generation` + времени.
- Перцентили и агрегаты в `status` — это уже сбор метрик, а не телеметрия одного
  запроса.
- `cost_usd` — требует канала usage в `Embedder`/`Reranker` и разбора ответа
  провайдера (`TransportResponse` сейчас несёт только
  `{status, body, retry_after}`); спек `search-cost`.
