# Search UX — Progress Ledger

Ledger исполнения `docs/specs/search-ux/`. Одна задача — одна запись при коммите.
Ruling фиксируют принятые решения; minor — что отложено.

- Spec rev 0.2 (2026-10-31): первый драфт заблокирован ревью (4×P1) и исправлен.
  FR-2: лексический `fetch == limit` не оставлял места для добора — окно
  расширяется до `limit × 4` при дедупе/фильтре, исчерпание окна даёт короткую
  выдачу, а не выдуманный хит.
  FR-3: «до ранжирования» невозможно без новой схемы tantivy — переформулировано
  в пост-фильтр по расширенному окну (один SQL по `docs.rel_path`), цена записана
  в NFR-1.
  NFR-1: бюджет был посчитан неверно («≤1 запрос на хит») — теперь ≤ 2 запросов
  **на ответ**, сниппет берётся одним JOIN вместе с цитатой.
  FR-5: токен `filter-empty` в `degraded` отменён — фильтр не деградация тира;
  вместо него `meta.filters`/`meta.trimmed`.
  SC-3: новой метрики не вводим, используем существующий `hit@10` из
  `search_bench`; замер — диагностика, гейт остаётся `metrics_match_baseline`.
  Ruling: дедуп и фильтры — пост-фильтры окна; ранжирование не меняется, поэтому
  дефолтная выдача остаётся побайтово прежней.

- Spec re-review (2026-10-31): **BLOCK** на rev 0.2 с 2×P1 + 4×P2; исправлено в
  rev 0.3. P1: SC-3 ссылался на несуществующую метрику `hit@10` — в харнессе есть
  per-case `hit` (`bench.rs::evaluate_case`) и `hit_at_3`; теперь назван `hit`.
  P1: формула `fetch = limit × 4` сужала fused/reranked-окно с 50 до 12 при
  `limit = 3` (регресс recall) и вчетверо удорожала реранк — расширение теперь
  только для лексического пути, окно fused/reranked не трогается.
  P2: `LIKE ? || '%'` не буквален (ASCII case-insensitive, `%`/`_` — маски) →
  путь и теги читаются одним `SELECT … WHERE id IN (окно)`, сравнение
  `starts_with` в Rust. P2: противоречие FR-4/FR-7 про скалярные `tags` — строгость
  разделена (аргумент запроса строгий, данные документа терпимые). P2: двойной
  overscan (reader ×4+16) признан в NFR-1 явно. P2: форма `meta.trimmed` при
  выключенных механизмах (`0`, не `null`) и `limit = 0` зафиксирована.
- Spec re-review round 3 (2026-10-31): **BLOCK** с 3×P1 + 3×P2; исправлено в
  rev 0.4. P1: `hit@10` остался ещё в пяти нормативных местах после правки SC-3 —
  заменён везде на per-case `hit` (`bench::evaluate_case`, на своём `k`).
  P1: `WHERE id IN ()` / `WHERE (doc_id, seq) IN ()` — синтаксическая ошибка
  SQLite, а пустое окно и нулевая выдача — самый частый исход; в дизайн добавлен
  обязательный guard «пустой вход → пустая мапа без SQL» (design §3, §5).
  P1: диагностическая конфигурация бенча не имела владельца — вместо пятой
  конфигурации в `baseline.json` заведена задача T2b: отдельный тест в
  `tests/search_bench.rs` на срезе `ru_morph`, сравнение через существующий
  `evaluate_case`, числа в леджер, гейт не трогается.
  P2: бюджет запросов записан формулой `⌈fetch/400⌉ + ⌈limit/400⌉` (слово «один»
  было неверно при `fetch = 1000`); в design §2 зафиксированы предусловия
  (`limit == 0` и кламп `limit ≤ MAX_HITS`), без которых формула окна читалась
  неоднозначно; в T2 добавлен шаг «`empty_response` собирает `filters`/`trimmed`».
  Ruling: пустой вход батчей — не край, а норма; guard обязателен и покрыт тестом.

- Task T0: complete (2026-10-31). Characterization + deltas + the direct-mode bug.
  RED → GREEN: `tests/cli_read.rs::explain_works_without_a_daemon` failed with the
  bare array (the direct path forwarded only `{query, limit}`), now green after
  `src/cli/search.rs` forwards `explain`. Датированные дельты добавлены в
  `ann-index/requirements.md` (§13) и `search-quality/requirements.md` (§13):
  аддитивные opt-in поля разрешены, дефолтная форма и baseline не меняются.
  Verify: `--test cli_read` 10 passed, `--test hybrid_ranking` 18 passed,
  clippy 0, fmt clean.
  Ruling: починка `explain` в direct-режиме сделана в T0, а не в T4 — иначе ветка
  оставалась бы красной между коммитами; T4 доберёт только новые ключи.

- Task T1: complete (2026-10-31). Snippet in one JOIN.
  `repo::citations_with_text` (row-value `IN ((?,?)…)`, batches of
  `BATCH_KEYS = 400`, empty input → empty map **without SQL**), `snippet_of`
  (`char_indices`, limit `SNIPPET_MAX_CHARS = 240`), `snippet` arg in
  `parse_search_args` (wrong type → protocol error), rows built by the extracted
  `build_rows` (default path keeps the per-hit `citation_for`, so nothing changes
  without the key). MCP schema + `docsbase search --snippet`.
  Tests: `snippet_is_a_prefix_of_the_chunk_text` (validated against the DB, not the
  shape), `snippet_is_absent_unless_requested_and_marked_when_cut` (no key → no
  fields; 600-char CJK chunk → exactly 240 chars, `snippet_truncated: true`, still a
  prefix).
  Verify: `--test hybrid_ranking` 20 passed; full default suite 64 ok; clippy 0;
  fmt clean.
  Ruling: `build_rows` extracted to satisfy the clippy line budget, and it keeps the
  two paths explicit — a snippet path with one JOIN and the untouched default path.
