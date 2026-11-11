# Search UX — Design

> Спек: `requirements.md` (rev 0.3). Задачи: `tasks.md`.
> Предшественники: `search-telemetry/design.md` (конверт `explain`, `meta`),
> `search-quality/design.md` (форма цитат), `ann-index/design.md` (тир ANN).

## 1. Где врезается пост-фильтр

Конвейер `search_docs_with` (`src/daemon/tools.rs`) сегодня:

```
fetch → ReadIndex::search (bm25) → [dense → rrf] → [apply_rerank] → truncate(limit) → citation_for × N → rows
```

Новый конвейер:

```
fetch(+overscan) → bm25 → [dense → rrf] → [apply_rerank]
      → POST-FILTER (path_prefix → tags → dedupe)   ← новое, до truncate
      → truncate(limit) → citations(+text) → rows
```

Ключевое: пост-фильтр стоит **после** `apply_rerank` и **до**
`ordered.truncate(limit)`. Поэтому порядок выживших кандидатов не меняется
(реранк уже расставил их), а дедуп видит всё окно, а не только `limit` строк.

## 2. Окно кандидатов

```rust
let filtering = dedupe != Dedupe::None || path_prefix.is_some() || !tags.is_empty();
let fetch = if rerank_active || rrf_active {
    // Окно уже шире limit (обычно window=50); расширять его = вчетверо больше
    // работы реранка без пользы (requirements FR-2).
    limit.max(window as usize).min(MAX_HITS)
} else if filtering {
    // Лексический путь: fetch == limit, дедупу неоткуда брать кандидатов.
    limit.saturating_mul(SEARCH_UX_OVERSCAN).max(limit).min(MAX_HITS)
} else {
    limit
};
```

Формула действует **после** двух существующих предусловий, которые не трогаются:
раннего `if limit == 0 { return empty_response(...) }` и клампа
`limit ≤ MAX_HITS` в `parse_search_args`. Без клампа `.min(MAX_HITS)` мог бы
опустить `fetch` ниже `limit`; с ним — не может.

`SEARCH_UX_OVERSCAN = 4` — константа рядом с `MAX_HITS`, калибруется замером.
Внутри `ReadIndex::search` уже есть собственное расширение (`limit × 4 + 16`),
поэтому суммарная работа BM25 может достигать ~16× от `limit`; это записано в
NFR-1 и проверяется замером, а не постулируется.

## 3. Один запрос метаданных на окно

Путь и теги нужны только для кандидатов окна (≤ `fetch` ≤ `MAX_HITS`).

```rust
// repo.rs
pub struct DocMeta { pub rel_path: String, pub frontmatter_json: Option<String> }
pub fn doc_meta(conn: &Connection, ids: &[i64]) -> Result<HashMap<i64, DocMeta>>;
```

Реализация: `SELECT id, rel_path, frontmatter_json FROM docs WHERE id IN (…)`,
нарезая `ids` по 400 (лимит переменных SQLite).

**Пустой вход — пустая мапа без SQL.** `IN ()` — синтаксическая ошибка SQLite, а
пустое окно (фильтр всё отсёк, или `limit = 0`) — самый частый исход, который
обязан быть успешным: `if ids.is_empty() { return Ok(HashMap::new()) }`.
Запросов на ответ: `⌈fetch / 400⌉`, на хит — ноль.

- `path_prefix`: `meta.rel_path.starts_with(&prefix)` — буквально. Никакого
  `LIKE`: он ASCII-case-insensitive и трактует `%`/`_` как маски, а требование —
  буквальный префикс (requirements FR-3).
- `tags`: теги документа парсятся из `frontmatter_json` терпимо:
  `Value::Object` → `tags` массив строк → множество; всё остальное → пустое
  множество (документ не проходит AND-фильтр, поиск не падает).

## 4. Дедуп

`doc_id` уже есть в `chunk_id` (`chunk_id_parts`), поэтому дедуп не требует ни
одного запроса:

```rust
let mut per_doc: HashMap<i64, usize> = HashMap::new();
ordered.retain(|(chunk_id, _)| {
    let (doc_id, _) = chunk_id_parts(*chunk_id);
    let seen = per_doc.entry(doc_id).or_insert(0);
    if *seen >= n_per_doc { false } else { *seen += 1; true }
});
```

`retain` сохраняет относительный порядок — это и есть «порядок не меняется»
(requirements FR-2). Окно исчерпано — выдача короче `limit`; добирать нечем и
незачем: следующий кандидат вне окна, а его ранжирование уже не проверено.

## 5. Сниппет одним JOIN

`repo::citation_for` — уже `JOIN docs`; добавляется одна колонка:

```rust
pub fn citations_with_text(conn: &Connection, keys: &[(i64, u32)])
    -> Result<HashMap<(i64, u32), (Citation, String)>>;
```

SQL: `SELECT c.doc_id, c.seq, d.rel_path, c.heading_path, c.line_start,
c.line_end, c.text FROM chunks c JOIN docs d ON d.id = c.doc_id
WHERE (c.doc_id, c.seq) IN ((?1,?2),(?3,?4),…)` — row values (SQLite ≥ 3.15,
bundled), нарезка по 400 пар (800 параметров, влезает даже в legacy-лимит 999).

**Пустой вход — пустая мапа без SQL** (та же причина, что в §3): `snippet: true`
при нулевой выдаче не должен превращаться в `Error::Internal`.
Запросов на ответ: `⌈limit / 400⌉`, а не по одному на хит.

Без `snippet` используется прежний `citation_for` по одному вызову на хит —
трогать дефолтный путь нельзя (requirements FR-6).

Обрезка:

```rust
fn snippet(text: &str) -> (String, bool) {
    match text.char_indices().nth(SNIPPET_MAX_CHARS) {
        Some((cut, _)) => (text[..cut].to_owned(), true),
        None => (text.to_owned(), false),
    }
}
```

`char_indices` гарантирует границу символа (NFR-4); `\r\n` не рвётся, потому что
режется по символам, а не по байтам.

## 6. `meta` и валидация

`SearchMeta` получает два поля (аддитивно, только в конверте `explain`):

```rust
pub struct SearchMeta {
    // …как в search-telemetry…
    pub filters: Option<FiltersMeta>,   // {path_prefix, tags, dedupe, n_per_doc}
    pub trimmed: Option<TrimmedMeta>,   // {filtered_out, deduped}
}
```

Счётчики считаются в той же стадии, где отбрасываются кандидаты (`0` при
выключенном механизме). Токенов в `meta.degraded` не добавляется: фильтр не
деградация тира (requirements FR-5).

`parse_search_args` расширяется по образцу `limit`/`rerank`/`explain`:
`snippet: bool`, `dedupe: enum`, `n_per_doc: u64` (1..=10, требуется
`dedupe=path`), `path_prefix: String`, `tags: Vec<String>`. Любой неверный тип —
`Error::Protocol`; `tags: []` — no-op.

## 7. CLI и direct-режим

Флаги → тот же JSON, что у MCP:

| CLI | MCP |
|---|---|
| `--snippet` | `snippet: true` |
| `--dedupe <none\|path>` | `dedupe` |
| `--n-per-doc <N>` | `n_per_doc` |
| `--path-prefix <P>` | `path_prefix` |
| `--tag <T>` (повторяемый) | `tags: [T…]` |
| `--explain` | `explain: true` |

`src/cli/search.rs` сегодня теряет `explain` в direct-режиме (передаёт только
`{query, limit}`) — исправляется здесь же, иначе `--explain` работает только при
поднятом демоне (requirements FR-8).

## 8. Совместимость и доказательства

- Дефолт: без новых ключей `fetch == limit` (лексика) или `max(limit, window)`
  (fused/rerank), пост-фильтр не активен, цитаты берутся прежним путём —
  `hits[]` побайтово прежний. Расширяется существующий
  `default_response_shape_is_the_bare_citation_array` кейсами «ключи не
  запрошены».
- `baseline.json` не перегенерируется: дефолтная конфигурация не меняется.
- Замер: пятая конфигурация отчёта (`Bm25 + dedupe:path`) не входит в гейт
  (`Baseline.configs` — `BTreeMap`, гейт идёт по фиксированному списку четырёх),
  поэтому `duplicate_path_ratio` и `hit` печатаются как диагностика, а не как
  новый гейт (requirements SC-3).

## 9. Что осознанно не делается

- MMR: требует векторов в момент ответа и второго прохода по окну; дедуп по
  документу закрывает измеренные 58% повторов.
- Фильтр в tantivy-схеме: потребовал бы переиндексации у каждого пользователя
  (NFR-3), а окно и SQL решают задачу без неё.
- `hit_at_3` как метрика для дедупа: у кейсов свой `k`, поэтому используется
  per-case `hit`.

## Delta 2026-11-09 — #61: извлечение тегов и обновление legacy metadata

`frontmatter::parse_block` читает flat-пары одного корневого отступа, игнорируя
вложенные ключи. Пустое значение `tags:` включает чтение следующей
последовательности строковых элементов с одним отступом; комментарии/пустые
строки пропускаются, соседний ключ завершает список. Вложенный индикатор
`- - value` не становится тегом, строка `"- value"` в кавычках сохраняется.
Это ограниченный парсер без новых зависимостей, не реализация полного YAML.

`repo::doc_states` загружает также `frontmatter_json`. `prepare_doc` разбирает
frontmatter перед проверкой no-op: пропуск возможен при совпадении source hash
и сериализованных метаданных. Расхождение запускает обычный план индексации
с сохранением doc id и hash, R2-маркером и публикацией `index_generation`.
Новая версия парсера тем самым исправляет старые записи на следующей полной
синхронизации; никакого глобального marker, новой схемы или tokenizer bump нет.

## Delta 2026-11-09 — #62: отсутствующий документ и отказ фильтра

В `apply_post_filter` отсутствие `doc_id` в `meta_by_id` сразу удаляет кандидата
из окна без изменения `trimmed.filtered_out`. Для существующего документа
проверяются прежние path/tag-предикаты; счётчик увеличивается один раз при
отказе любого из них. Отсутствующие/некорректные tags остаются отказом
tag-предиката. Дополнительных SQL-запросов, публичного counter и нового
degradation reason нет; дедуп и построение цитат используют прежние пути.

## Delta 2026-11-11 — #67 scoped retrieval

This supersedes window-only path/tag filtering. Execution plan and completion
contracts: [scoped-retrieval-plan.md](scoped-retrieval-plan.md); research and
alternatives: [#117](https://github.com/punkhomov/docsbase-memory-mcp/issues/117).

1. The existing short lexical phase begins a deferred metadata transaction.
   Status/root/generation/pending validation and scope preflight/copy share that
   snapshot. The project read lease still spans opening/searching Tantivy, but
   SQL is released first (same #66 ownership boundary). Scope is request-owned.
2. SQLite `UNIQUE(project_id, rel_path)` serves a BINARY `[prefix, successor)`
   range, preserving literal UTF-8 prefix semantics including NUL, `%`, `_`,
   maximum Unicode suffixes and the surrogate gap. No `LIKE`, path
   normalization, assumptions about clustered IDs or schema changes. For UTF-8 databases without
   tag filtering both scope queries are covering-index reads. UTF-16 databases
   use a project scan and Rust `starts_with`, since their BINARY ordering does
   not follow UTF-8 successors. Tag AND uses the
   existing tolerant serde_json predicate; duplicate JSON keys are last-wins.
3. Before allocating IDs, reserve a logical preparation allowance through
   `PayloadBudget`: range count × (`i64` ID + `Term` slot + encoded `i64` key),
   maximum stored JSON row × `size_of<Value>` when parsing tags, and one
   `BATCH_KEYS` page of `(chunk_id, String)` plus 64-byte BLAKE3 hex hashes for
   dense requests. UTF-16 text lengths are bounded after UTF-8 conversion.
   This bounds admitted logical work/data, not allocator/FST/SQLite RSS. No
   independent default/budget/cache is added. Scope is dropped before final
   payload admission, so these two phases never evade the per-request limit
   via simultaneous independent permits. Concurrent phases share the aggregate
   retained allowance. Oversized scope fails explicitly, without truncation.
4. `ReadIndex::search_scoped` intersects the parsed query with `TermSetQuery`
   on existing indexed `doc_id`. `ConstScoreQuery(0)` prevents a +1 score from
   changing BM25 or long-chunk penalties. The original query validates even
   when the ID set is empty. Corpus statistics and raw/final score stopping
   logic stay unchanged. The lexical window expands ×4 only for document caps.
5. Filtered dense searches bypass whole-project `DenseCache`/ANN loading and
   use existing query-embedding singleflight, then exact streaming top-k over
   eligible chunks. IDs are sorted and partitioned into batches of 400;
   `(doc_id, seq)` keyset pages retain at most 400 hashes, with one sentinel row
   to detect exhaustion without an empty trailing query. Every page validates
   original generation/pending/config under short admission/SQL/project leases.
   SQL text is borrowed while hashing; only hashes leave the phase. Vector IO,
   missing-vector handling, cosine scoring and the bounded heap retain no
   metadata/lease. Generation change fails explicitly before response capture.
6. RRF consumes scoped source ranks; truncate eligible fusion to the existing
   provider window, capture payload, rerank once, defensive final predicate,
   document cap, limit, citations. `meta.window` is planned window size;
   `filtered_out` counts observed final rejections, normally zero. Empty scope
   is not a degradation. Exact scoped dense is deliberate, not ANN failure.

No scope/result cache means tag/path changes are read anew on every request.
Query-vector identity remains model/query: scope cannot change an embedding.
No per-scope graph, bitmap mapping or cross-generation mutable state is created.
The existing unfiltered dense cache/generation/digest lifecycle is unchanged.

Tradeoffs: eager document term sets cost O(path range size) preparation even for
rare queries; tag-only scope parses project metadata. Broad filtered dense is
O(eligible chunks × vector dimension) plus eligible vector IO and can be slower
than ANN. Measurements belong in the ledger; a measured adaptive/lazy planner
and bounded filtered ANN remain follow-ups under #117. A short response due to
per-document caps remains possible: diversity/refill belongs to separate #68.

### Measured correction: page-local vector cache transactions

The initial release diagnostic found 1.8–2.1s for 10k eligible two-dimensional
vectors. `VectorCache::get` committed each LRU touch separately. `visit_page`
now reserves an IMMEDIATE transaction before reads, reuses SELECT/UPDATE
statements and commits once per supplied hash page. Missing rows still reach
the caller for cancellation checks. Each BLOB is decoded and scored before the
next lookup; no vector page is materialized. On callback/decode error or unwind,
RAII rolls back LRU touches. Only vector-cache resources span this local scan;
metadata/config/project admission already ended, and the cache transaction ends
before the next metadata page. No provider is invoked by the visitor. The exact
heap is shared with `top_k_iter`; cosine/tie/error semantics remain unchanged.
