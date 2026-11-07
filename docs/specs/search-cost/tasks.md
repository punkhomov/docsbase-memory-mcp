# Tasks: search-cost (phase 1)

**Goal:** повторный поиск с тем же текстом запроса и моделью не обращается к
провайдеру; поведение выдачи не меняется; попадания видны в `status`.
**Spec:** `docs/specs/search-cost/requirements.md` (phase 1)
**Status:** T1 complete (2026-10-31); фазы 2–3 (`--rerank`, `cost_usd`) — позже.

## T1 — Кэш вектора запроса в `DenseCache` + счётчики (complete)

**Files:** `src/hybrid/dense.rs` (`QueryCache`, `QUERY_CACHE_MAX_*`,
`query_vector`/`store_query_vector`/`query_cache_stats`), `src/daemon/tools.rs`
(`dense_candidates` спрашивает кэш; `status.query_cache`), `tests/hybrid_ranking.rs`,
`tests/status.rs`.

**Steps:**
1. Ключ `(model_id, blake3(text))`; `project_id` не входит — вектор не зависит от
   проекта.
2. Промах не вставляет: сначала `embed_query`, потом `store_query_vector`, чтобы
   ошибка эмбеддинга не кэшировалась.
3. Вытеснение LRU при `> 256` записей или `> 4 MiB`.
4. `status.query_cache = {hits, misses, entries, bytes}`; без кэша — нули.

**Acceptance:** SC-1 (один вызов на два поиска), SC-2 (`hits[]` идентичен, гейт
зелёный), SC-3 (счётчики и лимиты).

**Verify:**
```sh
RUSTUP_TOOLCHAIN=1.88.0 cargo test --locked --test hybrid_ranking --test status
RUSTUP_TOOLCHAIN=1.88.0 cargo test --locked --test search_bench
```
