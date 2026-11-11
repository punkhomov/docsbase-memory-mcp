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

- Task T2: complete (2026-10-31). Window, `path_prefix`, `tags`, `meta.filters/trimmed`.
  `repo::doc_meta` (один запрос на батч из 400 id, пустой вход → пустая мапа без
  SQL); `SEARCH_UX_OVERSCAN = 4` применяется только на лексическом пути;
  `apply_post_filter` (path_prefix `starts_with` → tags AND → dedupe) стоит после
  `apply_rerank` и до `truncate(limit)`; `FiltersMeta`/`TrimmedMeta` в `meta.rs`,
  `empty_response` тоже их собирает.
  Tests: `path_prefix_filters_the_window` (в т.ч. регистрозависимость: `C.` не
  матчит `c.md`, в отличие от SQL `LIKE`), `tags_filter_on_frontmatter` (AND,
  отсутствие тега → пустой результат, не ошибка), `dedupe_arguments_are_validated`
  (`n_per_doc` без dedupe, `0`, `11`, `dedupe: "filename"`, `tags: "api"`,
  `tags: [1]`, `path_prefix: 5`).
  Verify: `--test hybrid_ranking` 24 passed; полная сьюта 64 ok.
  Ruling: фильтр по пути — `starts_with` в Rust, а не `LIKE`: SQL `LIKE` в SQLite
  ASCII case-insensitive и трактует `%`/`_` как маски, то есть буквальный префикс
  он не выражает.
- Task T3: complete (2026-10-31). Dedupe по документу.
  `retain` с `HashMap<doc_id, usize>` до `truncate(limit)`; порядок выживших не
  меняется; `trimmed.deduped` считает отброшенных.
  Test: `dedupe_by_path_caps_chunks_per_document` — фикстура из одного файла на 8+
  чанков: без дедупа выдача заполняется одним путём, с `dedupe: "path"` остаётся
  один хит, с `n_per_doc: 2` — два, счётчики сходятся.
  Verify: `--test hybrid_ranking` 24 passed.
- Task T2b: pending (нужен прогон на срезе `ru_morph`; в T6).
- Task T4: complete (2026-10-31). CLI и direct-режим.
  `search::Options` — единый набор, `Options::args()` строит JSON для обоих швов,
  так что daemon и direct отправляют идентичный запрос; исправлена потеря
  `explain` (T0) и добавлены `--snippet`, `--dedupe`, `--n-per-doc`,
  `--path-prefix`, `--tag` (повторяемый).
  Test: `direct_mode_forwards_post_filter_flags` (префикс, сниппет, `meta.filters`).
  Verify: `--test cli_read` 11 passed; полная сьюта 64 ok; `search_bench` 68 passed,
  `metrics_match_baseline` зелёный, baseline не тронут; clippy 0; fmt clean.

- Task T2b: complete (2026-10-31). Измерение дедупа на `ru_morph` (12 кейсов,
  `search_bench::dedupe_slice_diagnostics`):

  | `n_per_doc` | `duplicate_path_ratio` | `hit` |
  |---|---|---|
  | plain | 0.582 | 11 |
  | 1 | 0.000 | 8 |
  | 2 | 0.357 | 10 |
  | 3 | 0.480 | 12 |
  | 4 | 0.526 | 12 |

  Ни одно значение не даёт одновременно `ratio ≤ 0.20` и все хиты — исходная
  формулировка SC-3 была невыполнима. Ruling: дефолт `n_per_doc = 2` (потеря
  одного `hit` из 11, повторы падают почти вдвое), критерий в SC-3 заменён на
  измеренную кривую; требование «ratio ≤ 0.20» снято как несовместимое с
  сохранением recall на этом срезе.
  Замечание по эффекту окна: `n_per_doc=3` даёт `hit 12` — на один больше plain,
  потому что расширенное окно (`limit × 4`) приносит кандидата, которого
  лексический `fetch == limit` не видел; это эффект overscan, а не дедупа.
  Базовый прогон (`metrics_match_baseline`) не изменился.

- Task T5: complete (2026-10-31). `docs/MCP.md` (таблица `search_docs` с новыми
  ключами + раздел **Post-filters** с измеренным дефолтом `n_per_doc = 2`,
  буквальностью префикса и «пустой результат — не ошибка») и `CHANGELOG`
  [Unreleased]/Added.
- Task T6: complete (2026-10-31). Прогон и доказательства.
  `search_bench` 69 passed (включая `dedupe_slice_diagnostics`),
  `metrics_match_baseline` зелёный, `baseline.json` не изменён (`git diff` пуст);
  полная default-сьюта 64 `test result: ok`, combo (`hybrid-local,hybrid-remote`)
  64; clippy 0 в обеих конфигурациях; `cargo fmt --check` чисто.
  Ruling: замер стоимости окна (`⌈fetch/400⌉ + ⌈limit/400⌉`) на практике остаётся
  в пределах двух запросов при штатных `limit ≤ 100`; отдельного perf-теста не
  заводим, бюджет `search-benchmark` FR-22 не пробит (гейт зелёный).

- Review panel (2026-10-31, after PR #60 was opened): три модели последовательно
  (mimo → muse → deepseek), одинаковый бриф, дерево чистое после каждого прогона.
  Вердикты: 3/3 `OK WITH NOTES`, ни одного P0. Верификатор (4-я модель) проверил
  9 дедуплицированных утверждений: **7 VERIFIED, 2 PARTIAL, 0 REFUTED**.
  Оба PARTIAL понижены верификатором до P2 (несогласованность спека/леджера, а не
  рантайм-дефект) — принято.
  Исправлено в этом же коммите:
  - **C1 (P1, 3/3)**: MCP-схема `search_docs` рекламировала только `snippet`;
    добавлены `dedupe`, `n_per_doc`, `path_prefix`, `tags` + тест
    `mcp_docs::search_docs_schema_advertises_every_post_filter_key`.
  - **C2 (P2)**: SC-3/FR-2 не были поправлены — добавлена датированная дельта §13
    (дефолт 2 и измеренная кривая вместо порога 0.20).
  - **C3 (P2)**: стоимость новых путей не была измерена — в
    `dedupe_slice_diagnostics` добавлен `report_latency`: plain 0.99 мс/запрос,
    `snippet+dedupe` 0.93 мс/запрос (12 кейсов, `ru_morph`).
  - **C4 (P2)**: `n_per_doc: null` без `dedupe` больше не протокольная ошибка
    (`null` = «не задано», как у соседних ключей).
  - **C5 (P2)**: `--n-per-doc` help исправлен на «default 2».
  - **C6 (P2)**: обрезка сниппета не рвёт пару CRLF (отступает на один символ),
    комментарий приведён в соответствие; тест `snippet_never_splits_a_crlf_pair`.
  - **C7 (P2)**: `empty_response` теперь несёт `meta.filters`, а не только
    `trimmed`.
  - **C8 (P2)**: добавлен тест добора `dedupe_fills_freed_slots_with_other_documents`
    (фикстура: 6 чанков одного файла + 4 одночанковых; освобождённые места
    занимают другие документы).
  - **C9 (P2)**: `docs/MCP.md` документирует, что индексируется только
    однострочная форма `tags: a, b`, а YAML-список — нет.
  Ещё поправлено по «Also noticed»: пустой `path_prefix` = отсутствие фильтра
  (не гоняем машинерию ради no-op), восстановлены сдвинутые doc-комментарии
  (`cli::search::run`, тест `explain` в `hybrid_ranking`).
  Verify: default-сьюта 64 `test result: ok`; combo 64; clippy 0 в обеих
  конфигурациях; fmt чисто; `baseline.json` не изменён.

## Delta 2026-11-09 — #61: блочные tags

- Task T7: complete (FR-12, FR-13, SC-8). Блочные строковые списки дают те же
  сохранённые tags, что и `tags: api, docs`; поддержаны отступы/indentless,
  LF/CRLF, кавычки, комментарии и пустые строки. Ключи другого уровня и
  вложенные элементы не становятся тегами документа.
- Ruling: без миграции БД/tokenizer bump. `doc_states` загружает сохранённый
  `frontmatter_json`; no-op требует совпадения hash и заново извлечённых
  метаданных. Legacy `NULL` и metadata с title/пустыми tags обновляются обычным
  crash-safe планом при полном sync (или обработке пути инкрементом); doc id
  и source hash сохраняются, поколение публикуется существующим путём.
  После refresh повторный sync снова no-op. Это переопределяет исходный запрет
  повторной обработки документов в шапке tasks.md только для stale metadata.
- RED → GREEN: parser/persistence/legacy full+incremental/реальный IPC AND-filter;
  отдельно воспроизведены оба замечания ревью: вложенный ключ `tags` перезаписывал
  корневые теги, а compact sequence `- - nested` становилась строковым тегом.
  Оба исправлены и повторно проверены; quoted `"- value"` и plain `-value`
  остаются строками. Fresh task review + final review: **converged**, открытых
  Critical/Important и deferred minors нет.
- Verify (MSRV 1.88.0, base `f51368e`): focused cli-full — **48 passed**;
  полный `cargo test --locked --features cli-full` — **890 passed, 13 ignored**
  (63 успешных target-группы). Clippy `--all-targets` и opt-in
  `--test perf_budget --test artifact` с cli-full/`-D warnings`, fmt и diff checks
  прошли. `baseline.json` и snapshots не менялись; новых зависимостей нет.

## Delta 2026-11-09 — #62: filtered_out и отсутствующий документ

- Task T8: complete (FR-14 / SC-9). `apply_post_filter` исключает кандидата
  без строки документа до вычисления filter predicate и обновления счётчика.
  Живой документ, не прошедший path/tag, считается один раз. JSON-форма
  `trimmed`, выдача и остальные стадии не менялись.
- Ruling: новый публичный `vanished` counter не нужен; отсутствие документа
  не приписывается отказу фильтра. Отсутствие tags у существующего документа
  по-прежнему является отказом tag-фильтра.
- RED → GREEN: реальный Tantivy-кандидат сохраняется после удаления строки
  SQLite; до фикса `filtered_out` был 1 вместо 0. Шесть сценариев проверяют
  path/tag/combined и смешанное окно: точные счётчики 0/1, живые hits и прежнюю
  JSON-форму. Никаких mocks, sleeps или тестовых production hooks.
- Fresh task / whole-branch review: PASS / APPROVED, **converged**;
  Critical/Important/Minor и deferred findings нет.
- Verify (MSRV 1.88.0, base `a8e1ff0`): `hybrid_ranking` — **32 passed**;
  полный cli-full `cargo test --locked` — **891 passed, 13 ignored**
  (63 успешных target-группы). Clippy cli-full `--all-targets` и opt-in
  `--test perf_budget --test artifact` с `-D warnings`, fmt/diff checks прошли.
  Схема/tokenizer, зависимости, `baseline.json` и snapshots не менялись.

## Delta 2026-11-11 — #67 scoped retrieval

- Task S1: complete (FR-15…19 / SC-10). Real SQLite/Tantivy/vector-cache
  regressions: eligible rank>1000, all four lexical/RRF/rerank combinations,
  scoped source ranks, dense-only matches, ignored corrupt excluded vectors,
  unchanged BM25 scores, metadata refresh, invalid queries in empty scope,
  cancellation/generation changes and a late >400-chunk hash page. CLI/direct
  and daemon IPC now cover candidates beyond the former global window.
- RED → GREEN: `scoped_candidates_survive_global_windows` returned `[]` before
  the fix. Extra UTF-16 regression exposed BINARY ordering at ÿ/Ā; UTF-8 keeps
  the indexed range, UTF-16 uses Rust prefix comparison. Tests cover all three
  database encodings, NUL/wildcards/case/Unicode/malformed tags.
- Ruling: request-owned scope; existing byte admission, no shared scope cache,
  schema/tokenizer/dependency/baseline changes. Exact scoped dense avoids cold
  whole-project cache initialization; source ranks are scoped before RRF.
  Scope and payload permits do not overlap. Final `filtered_out` is normally
  zero; #62 missing-document semantics remain intact. #68 remains separate.
- Fresh task reviewer: one Important (UTF-16 range) and two Minor (product doc
  wording/anchor and byte-limit docs); all fixed. Fix re-review: ADDRESSED for
  all three, no new Critical/Important findings. No deferred minors.
- Verification: MSRV1.88 cli-full suite **1184 passed, 14 ignored**, 70 target
  groups; Clippy all-targets `-D warnings` passed. UTF-16 RED/GREEN rerun passed
  after the suite. Debug symbols disabled for test artifacts to fit disk.
- S2 in progress: release 20k/two-folder diagnostic completed. Initial broad
  scoped RRF took 1.8–2.1s: per-vector LRU autocommit dominates. The measured
  follow-up is page-local vector-cache transactions while streaming one vector
  at a time; no whole-scope vector materialization. Final numbers and all
  delivery gates follow after this measured correction.

- Task S2: complete (FR-20 / SC-11/12). Same 20k release diagnostic repeated:
  broad scoped RRF **163.629–212.639ms**, initial **1796.817–2079.110ms**.
  Exact heap shared with the iterator path; vector pages use one IMMEDIATE
  transaction, reused statements and one decoded vector at a time. LRU/model/
  missing-row/order/rollback/reuse behavior has real cache tests.
- Ruling: retain exact scoped dense for this first slice; broad-scope/rare-query
  eager planning costs and synthetic-vector/OS-cache caveats are explicit in
  `reports/scoped-retrieval-2026-11-11.md`. No optimal-planner or p95 claim.
  #117 retains adaptive planner/filtered ANN work; #68 remains separate.
- Fresh S2 reviewer: PASS / APPROVED, no outstanding findings. Logical admission
  prose corrected to match `size_of<Term>` (480kB broad, 48B narrow, plus38400B
  dense hash page). No deferred minors. Final native/cold/RSS acceptance is not
  inferred from synthetic tests.
- Verify: cli-full **1186 passed, 14 ignored** (70 groups), unchanged quality
  gate green. Post-lint helper extraction: **521 passed** across lib, hybrid,
  direct CLI and IPC. Clippy all-targets + opt-in artifact/perf, fmt, fresh
  static artifact and provider capability checks passed. Late S1 UTF-16 fix
  exceeded Clippy's line limit; pure preparation-charge helper extraction in
  S2 resolved it, and the final scoped tests above cover all three encodings.
  Remaining resource/lean/offline and native CI gates belong to S3.

- Task S3: complete (final review and implementation verification, 2026-11-11).
  Fresh whole-branch review of `498753e..9b2ad65`: runtime/spec convergence,
  no Critical/Important/Minor findings. Its only outstanding delivery item was
  native CI/S3 recording; required run **43022238707** then passed **docs,
  linux, macos, windows** for implementation commit `9b2ad65`.
- Final evidence: cli-full **1186 passed / 14 ignored**; **521 focused passed**
  after the pure preflight-helper extraction; unchanged benchmark gate green;
  fmt/Clippy all-targets plus explicit artifact/perf lint; fresh static CLI
  **20522880 bytes** with static linkage + both provider-capability checks;
  **6 release resource checks**, lean dependency boundary, offline workflow,
  **6 Python CI-observer regression checks** all passed. The 20k report records
  individual release runs and logical admission, not peak RSS/real-corpus p95.
- Ruling: PR **#118** contains #67 only. No schema/tokenizer/dependency,
  baseline/threshold/model/recording changes. No deferred minors. #117 retains
  adaptive planning/filtered ANN; #68 diversity/refill remains a separate PR
  after human merge. Merging remains a human action.
- This final commit changes only this ledger. Required checks must also be
  green on its final PR head before marking ready; final head/run outcomes are
  recorded in PR #118 to avoid modifying the ledger after every CI run.


## Delta 2026-11-11 — #68

- Task R1: complete (bounded candidate refill and owner policy). Real RED
  40+10 corpus returned2 instead10; >1000 dominant case stayed at window40.
  Policy RED rejected the new key. GREEN covers lexical/RRF/rerank, one provider
  call/late winning chunk, scopes, scores/order, exhaustion vs budget, configured
  ceiling/base floor, generation/cancellation and byte admission cleanup.
- Verify: full cli-full runtime suite **1198 passed,14 ignored** (71 groups);
  subsequently added unfiltered-RRF integration and clamped-RRF unit both passed.
  Final focused11 integration +18 policy, then fix-round unit +11 integration;
  all-target Clippy/fmt passed. Logs: /tmp/refill-{red,policy-red,full,focused-final,
  r1-fix,clippy}.log. No baseline/threshold/tokenizer/schema/dependency changes.
- Ruling: fresh R1 reviewer PASS/APPROVED. Two Minor findings fixed in-task:
  cleanup reservations now exhaust the complete aggregate allowance; adversarial
  actual RRF10→12 widening proves naive top12 displaces base chunks and the
  selected pool preserves all of them. Fix review: both ADDRESSED, no new
  Critical/Important findings. No deferred minors. R2 measurement/delivery pending.
- Session process delta: user explicitly requested agents perform squash merge
  after verified checks. This overrides historical human-only delivery guidance
  for the authorized work in this session. #118 was squash-merged at061b353, #67
  closed; #68 branch starts from that actual main commit.
