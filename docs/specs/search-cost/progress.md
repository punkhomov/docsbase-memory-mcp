# Search Cost — Progress Ledger

- Spec rev 0.1 (2026-10-31): фаза 1 (кэш вектора запроса) выделена отдельно от
  `cost_usd`/`--rerank`, потому что она не требует изменений трейтов провайдера.
- Task T1: complete (2026-10-31). `DenseCache::query_vector`/`store_query_vector`/
  `query_cache_stats`, ключ `(model_id, blake3(text))`, лимиты 256 записей /
  4 MiB, LRU, промах не кэширует ошибку. `dense_candidates` сначала смотрит кэш;
  `status.query_cache` отдаёт hits/misses/entries/bytes (нули, если кэш
  недоступен).
  Tests: `a_repeated_query_does_not_call_the_provider_twice` (счётчик вызовов:
  два поиска → один вызов; другой текст и другая модель → промахи),
  `the_query_cache_evicts_at_its_cap`, `status_reports_query_cache_counters`.
  Verify: `--test hybrid_ranking` 28 passed, `--test status` 6 passed, полная
  default-сьюта 64 `test result: ok`, `search_bench` 69 passed с зелёным
  `metrics_match_baseline`, clippy 0, fmt чисто.
  Ruling: кэш живёт в `DenseCache`, а не в отдельном слое демона — бенчмарк
  конструирует тот же `DenseCache`, поэтому получает кэш бесплатно, и отдельная
  тестовая обёртка для мемоизации не нужна.
  Ruling: ключ без `project_id` — вектор запроса не зависит от проекта; при
  требовании изоляции ключ расширяется.
