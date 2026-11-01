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
нарезая `ids` по 400 (лимит переменных SQLite). Один запрос на фильтр-стадию,
не на хит.

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
bundled), нарезка по 400 пар. Один запрос на ответ, а не на хит.

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
