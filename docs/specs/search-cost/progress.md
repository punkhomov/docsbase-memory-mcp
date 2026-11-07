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

- Review panel (2026-10-31, retroactively — the first commit landed without review):
  mimo / muse / deepseek, последовательно, одинаковый бриф, дерево чистое после
  каждого. Вердикты: 3/3 `OK WITH NOTES`, ни одного P0.
  Утверждения проверены родителем командами (поэтому верификатор не запускался,
  как разрешает протокол):
  - **P1 (3/3)**: SC-4 недостижим — `run_config_timed` создаёт `DenseCache` на
    каждую конфигурацию. Исправлено: `DenseCache::sharing_query_cache()` +
    `run_config_timed_with_query_cache(..., Some(&cache))`; делится только кэш
    запросов, строки чанков остаются холодными (иначе поехал бы замер холодного
    скана). Тест `a_shared_query_cache_is_reused_without_sharing_rows`.
  - **P2 (2/3)**: равенство ответов в тесте кэша было вакуумным —
    `CountingEmbedder` объявлял `fake/counting@d2@l2`, а вектора чанков лежали под
    `fake/model@d2@l2`, поэтому dense-список был пуст и ответ не зависел от
    вектора запроса. Исправлено: тот же model id + утверждение, что
    dense-only документ (`b.md`) присутствует.
  - **P2 (2/3)**: LRU-обновление и байтовый бюджет не были покрыты. Добавлены
    `a_query_cache_hit_refreshes_recency` и `the_query_cache_respects_its_byte_budget`.
  - **P2 (2/3)**: doc-комментарии съехали (док `DenseCache` оказался на
    `type QueryKey`, док теста `explain` — на тесте кэша). Восстановлены.
  - **P2 (1/3)**: не было `design.md`. Написан.
  - **P2 (1/3, принято как ограничение)**: нет single-flight — два одновременных
    одинаковых запроса оба платят. Корректность не страдает; записано в §7 дельт
    и в `design.md` §6.
  Verify: `--test hybrid_ranking` 31 passed; полная default-сьюта 64; combo clippy 0;
  `search_bench` 69 passed, `metrics_match_baseline` зелёный, baseline не тронут.

- Correction (2026-10-31): предыдущая запись утверждала, что ассерт про dense-only
  документ и док-комментарий теста `explain` восстановлены — **оба изменения не
  применились** (правка не совпала с отформатированным текстом), и проверка это
  поймала. Плюс сам ассерт оказался слабым: в фикстуре все чанки лежат в одном
  направлении, поэтому подмена вектора запроса его не ловила — это доказано
  нуль-контролем (саботаж `embed_query` → тест зеленел).
  Исправлено по-настоящему:
  - `LyingEmbedder` с тем же `model_id`, но ортогональным вектором: если кэш
    обслуживает третий поиск, «лжец» не вызывается (`calls == 0`) и ответ не
    меняется. Нуль-контроль в обратную сторону: если убрать чтение кэша из
    `dense_candidates`, тест падает на «second call is served from the cache».
  - док-комментарий `explain`-теста возвращён.
  Счётчики в тесте уточнены (hits = 2: второй поиск и поиск «лжеца»).
  Verify: `--test hybrid_ranking` 31 passed; полная сьюта 64; clippy 0; fmt чисто.
  Ruling: утверждение «кэш не меняет ответ» проверяется не сравнением двух
  одинаковых прогонов (это ничего не доказывает), а тем, что подставной эмбеддер с
  тем же model_id не вызывается вовсе.
