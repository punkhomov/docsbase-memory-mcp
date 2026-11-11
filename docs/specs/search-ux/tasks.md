# Tasks: search-ux (initial plan)

**Goal:** ответ поиска пригоден агенту с первого вызова: фрагмент текста, без
пяти чанков одного файла, с фильтрами по пути и тегам — всё аддитивно и выключено
по умолчанию.
**Architecture:** пост-фильтр окна кандидатов в `search_docs_with` (после
`apply_rerank`, до `truncate(limit)`); путь и теги — одним SQL по `id IN (окно)`;
сниппет — одним JOIN вместе с цитатой.
**Spec:** `docs/specs/search-ux/requirements.md` (rev 0.3)
**Design:** `docs/specs/search-ux/design.md`
**Status:** план; T0 — вход.

**Global constraints:**
- Rust edition 2024, MSRV 1.88; `cargo fmt`; clippy `--all-targets -- -D warnings`.
- Новых крейтов нет; default build offline (дерево-гейт NFR-1 продукта).
- В `src/**` нет `unwrap`/`expect`/`panic!`.
- **Дефолтный ответ побайтово прежний**; `baseline.json` не перегенерируется.
- Схема tantivy/БД не меняется, переиндексации у пользователя нет.

**Review focus:**
1. Пост-фильтр стоит до `truncate(limit)` и не меняет порядок выживших.
2. Окно расширяется только на лексическом пути; fused/reranked не трогается.
3. `path_prefix` буквален (`starts_with`), а не `LIKE`.
4. Сниппет — один JOIN на ответ, обрезка по границе символа.
5. Дефолт не изменился; замер дедупа — в отчёте, а не новый гейт.

---

## T0 — Characterization + дельты (вход)

**Goal:** зафиксировать, что дефолт не меняется, и что direct-режим сейчас теряет
`explain`; добавить датированные дельты в два замороженных спека.

**Files:**
- Modify: `tests/hybrid_ranking.rs` (`assert_bare_citation_array` + кейс «новые
  ключи не запрошены»).
- Modify: `tests/cli_read.rs` (RED: `--explain` в direct-режиме не отдаёт конверт).
- Modify: `docs/specs/ann-index/requirements.md`, `docs/specs/search-quality/requirements.md`
  (датированные дельты: аддитивные opt-in поля разрешены спеком `search-ux`).

**Steps:**
1. RED: `docsbase search --explain` без демона печатает массив, а не конверт —
   падает (подтверждает FR-8).
2. Расширить shape-тест: дефолтный ответ не содержит `snippet`/`meta.trimmed`.
3. Дельты в два спека одной строкой каждая, с датой.

**Acceptance:** RED-тест падает по ожидаемой причине; дельты на месте.

**Verify:** `cargo test --locked --test hybrid_ranking --test cli_read`

**Depends:** —

---

## T1 — Сниппет одним JOIN

**Goal:** FR-1: `snippet: true` даёт фрагмент ≤ 240 символов, обрезка по границе
символа, один запрос на ответ.

**Files:**
- Modify: `src/store/repo.rs` (`citations_with_text`).
- Modify: `src/daemon/tools.rs` (аргумент + сборка строк со сниппетом).
- Modify: `src/mcp/tools.rs`, `src/cli/mod.rs`, `src/cli/search.rs` (схема/флаг).

**Steps:**
1. RED: `snippet: true` → у хитов есть `snippet`, он подстрока `chunks.text`.
2. `citations_with_text` с row values, нарезка по 400 пар.
3. Обрезка `char_indices`; `snippet_truncated` при обрезке.
4. Без `snippet` — прежний путь (`citation_for`), полей нет.

**Acceptance:** SC-2 (подстрока, лимит, флаг, отсутствие полей без ключа).

**Verify:** `cargo test --locked --test hybrid_ranking --test cli_read --test mcp_docs`

**Depends:** T0

---

## T2 — Окно, `path_prefix`, `tags`, `meta.filters/trimmed`

**Goal:** FR-2 (окно), FR-3, FR-4, FR-5: фильтры по окну, один SQL на метаданные,
счётчики в `meta`.

**Files:**
- Modify: `src/store/repo.rs` (`doc_meta`).
- Modify: `src/daemon/tools.rs` (расширение `fetch`, стадия пост-фильтра, `meta`).
- Modify: `src/hybrid/meta.rs` (`FiltersMeta`, `TrimmedMeta`).
- Modify: `src/mcp/tools.rs`, CLI-флаги.

**Steps:**
1. RED: 50 запросов с `path_prefix` → ноль хитов вне префикса; `tags` — AND.
2. Расширение `fetch` по правилам design §2 (лексика ×4, fused/rerank — нет).
3. `doc_meta` одним запросом; `starts_with` для префикса; терпимый разбор тегов.
4. `meta.filters` и `meta.trimmed` (только при `explain`; `0` при выключенном);
   `empty_response` тоже собирает их (`filters: null`, `trimmed` из нулей).
5. Пустой вход батчей (`doc_meta`, `citations_with_text`) — пустая мапа **без
   SQL**: `IN ()` невалиден, а пустой результат — нормальный исход.

**Acceptance:** SC-4, SC-5 (валидация), SC-7 (≤ 2 доп. запроса).

**Verify:** `cargo test --locked --test hybrid_ranking --test mcp_docs`

**Depends:** T1

---

## T2b — Диагностический замер дедупа (владелец SC-3)

**Goal:** FR-11/SC-3: числа для дедупа снимаются тестом, а не руками.

**Files:**
- Modify: `tests/search_bench.rs` (новый `#[test]` на срезе `ru_morph`).

**Steps:**
1. Прогнать срез дважды: как есть и с `dedupe: "path"` (через существующий seam
   `search_docs_with`, аргументы того же вида, что в харнессе).
2. Агрегировать `bench::evaluate_case` по кейсам: средний
   `duplicate_path_ratio` и число `hit` (per-case, на своём `k`).
3. Утверждать: `duplicate_path_ratio ≤ 0.20` и `hit` не ниже базового прогона.
   Числа печатать (`--nocapture`) и записать в леджер.
4. Baseline и гейт не трогать: тест диагностический, в `metrics_match_baseline`
   не входит.

**Acceptance:** тест зелёный, числа в леджере, `baseline.json` без изменений.

**Verify:** `cargo test --locked --test search_bench dedupe_slice_diagnostics -- --nocapture`

**Depends:** T3

---

## T3 — Дедуп по документу

**Goal:** FR-2 (дедуп): `dedupe: "path"` + `n_per_doc`, порядок выживших не
меняется.

**Files:**
- Modify: `src/daemon/tools.rs` (retain по `doc_id` до `truncate`).

**Steps:**
1. RED: 8 чанков одного файла → при `n_per_doc = 1` в выдаче один.
2. `retain` с `HashMap<doc_id, usize>`; `trimmed.deduped` = число отброшенных.
3. `n_per_doc` без `dedupe` → `Error::Protocol`; `0`/`>10` → ошибка.

**Acceptance:** SC-3 (порядок и состав), SC-5 (валидация).

**Verify:** `cargo test --locked --test hybrid_ranking`

**Depends:** T2

---

## T4 — CLI/direct-режим

**Goal:** FR-8: флаги и полная передача аргументов, включая `explain`.

**Files:**
- Modify: `src/cli/mod.rs` (`Search` args), `src/cli/search.rs` (проброс JSON).

**Steps:**
1. GREEN для RED-теста из T0: direct-режим передаёт `explain` и новые ключи.
2. Карта флагов по design §7; `--tag` повторяемый.

**Acceptance:** одинаковый JSON на обоих швах (daemon и direct).

**Verify:** `cargo test --locked --test cli_read --test smoke`

**Depends:** T2, T3

---

## T5 — Документация

**Goal:** FR-10: `docs/MCP.md` и `CHANGELOG` описывают ключи, поля и флаги.

**Files:**
- Modify: `docs/MCP.md` (таблица `search_docs`, поля `hits[]`, `meta`).
- Modify: `CHANGELOG.md` (Added).

**Acceptance:** `grep search_docs docs/MCP.md` соответствует живой схеме; CHANGELOG
содержит новые ключи.

**Verify:** гейт `docs` локально; `cargo test --locked --test mcp_docs`

**Depends:** T1, T2

---

## T6 — Замер и полный прогон

**Goal:** NFR-1, NFR-6, SC-1, SC-3, SC-7.

**Files:** нет новых; отчёт и леджер.

**Steps:**
1. `search_bench`: дефолтные четыре конфигурации — `metrics_match_baseline`
   зелёный, `baseline.json` не изменён.
2. Числа дедупа берутся из T2b (диагностический тест), а не из нового прогона:
   сверить, что они записаны в леджер.
3. Замер p95 до/после для `snippet` и расширенного окна; сравнить с бюджетом
   `search-benchmark` FR-22 (host-relative).
4. Полная сюита, combo, clippy, fmt.

**Acceptance:** SC-1 (дефолт побайтово), SC-3 (числа в леджере), SC-7 (бюджет не
пробит), baseline не тронут.

**Verify:**
```sh
RUSTUP_TOOLCHAIN=1.88.0 cargo test --locked
RUSTUP_TOOLCHAIN=1.88.0 cargo test --locked --test search_bench
RUSTUP_TOOLCHAIN=1.88.0 cargo clippy --locked --all-targets -- -D warnings
cargo fmt --check
```

**Depends:** T1–T5

---

## Порядок и оценка

| Task | FR/SC | Оценка |
|---|---|---|
| T0 characterization + дельты | SC-1, FR-9 | 0.3 d |
| T1 сниппет | FR-1 / SC-2 | 0.5 d |
| T2 окно + фильтры + meta | FR-2..FR-5 / SC-4, SC-5, SC-7 | 0.7 d |
| T3 дедуп | FR-2 / SC-3 | 0.4 d |
| T2b диагностика дедупа | FR-11 / SC-3 | 0.2 d |
| T4 CLI | FR-8 | 0.4 d |
| T5 доки | FR-10 | 0.2 d |
| T6 замер + прогон | NFR-1, NFR-6 | 0.3 d |

Итого ≈ 2.8 d. T0 обязателен: без него «дефолт не изменился» — обещание.

## Что этот спек НЕ делает

| Работа | Где |
|---|---|
| MMR / семантическая диверсификация | отдельный спек при доказанной потребности |
| Фильтры по дате/заголовку/языку, DSL | не планируется |
| Подсветка с разметкой | не планируется |
| query-кэш, стоимость, окно rerank | `search-cost` |
| Кросс-проектный `scope` | не планируется |

## T7 — #61: блочные tags и обновление старых метаданных (2026-11-09)

**Requirements:** FR-12, FR-13, SC-8 из датированной дельты; прежний AND-фильтр
FR-4 и строки citation сохраняются. New crates: none.

**Files:** `src/index/frontmatter.rs`, `src/index/job.rs`, `src/store/repo.rs`;
`tests/frontmatter.rs`, `tests/index_job.rs`, `tests/index_incremental.rs`,
`tests/mcp_docs.rs`; `docs/MCP.md`, `CHANGELOG.md` и этот спек.

**Steps:** RED для block lists, persisted metadata, legacy refresh и IPC tags;
минимальный парсер списка; проверка derived metadata перед no-op; документация.

**Acceptance:** две формы дают одинаковые tags; соседние/вложенные списки не
захватываются; legacy metadata исправляется без изменения исходных файлов;
повторный sync пропускает документ; `search_docs` находит block-list документ
и исключает partial/untagged документы по AND. Схема БД и baseline не меняются.

**Verify:** focused frontmatter/index_job/index_incremental/mcp_docs;
полная cli-full suite; clippy --all-targets; fmt; frozen baseline diff.

## T8 — #62: не считать vanished документы отказами фильтра (2026-11-09)

**Requirements:** FR-14 / SC-9 из датированной дельты. New crates: none.

**Files:** `src/daemon/tools.rs` (`apply_post_filter`), `src/hybrid/meta.rs`
(комментарий счётчика), `tests/hybrid_ranking.rs`, `docs/MCP.md`, `CHANGELOG.md`
и этот спек. Search orchestration / concurrency вне этой задачи.

**Steps:** RED через настоящий `search_docs_with`: удалить строку `docs`,
проверить, что кандидат остаётся в Tantivy; проверить path/tag/combined и
смешанное окно с настоящим отказом. Затем early-return для missing metadata
перед вычислением filter predicate и обновлением счётчика.

**Acceptance:** счётчики `0`/`1` по SC-9, прежние hits и JSON-форма trimmed.
**Verify:** hybrid_ranking; full cli-full suite; clippy/fmt; frozen baseline diff.

## Delta 2026-11-11 — #67

Implementation contracts: [S1 / S2](scoped-retrieval-plan.md).
S1 owns FR-15…19 / SC-10 (scope/candidates/state/regressions); S2 owns SC-11,
product docs, quality/portability evidence, final review and ready PR.
New crates: none. No baseline/snapshot/tokenizer/schema changes. #68 is separate.
