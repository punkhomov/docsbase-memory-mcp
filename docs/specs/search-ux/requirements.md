# Search UX — Requirements (SPECIFICATION)

> Feature: `search-ux` — ответ поиска должен быть пригоден агенту с первого
> вызова: фрагмент текста, без пяти чанков одного файла, с фильтрами по пути и
> тегам. Всё — аддитивно и по умолчанию выключено.
> Контекст: `docs/specs/constitution.md`, `docs/specs/search-quality/`
> (форма выдачи, no-regression), `docs/specs/search-telemetry/` (`explain`,
> `meta`, правила токенов), `docs/specs/ann-index/` (FR-10 — заморозка формы
> `search_docs`), `docs/specs/search-benchmark/` (`duplicate_path_ratio`, FR-22).

## 1. Problem

Агент получает цитату `{chunk_id, path, heading_path, lines, score}` и, чтобы
понять, о чём чанк, обязан сделать второй вызов (`get_doc`/`read_neighbors`).
Три дефекта измерены нашим же корпусом:

1. **Фрагмента нет.** В ответе только координаты. Лишний round-trip на каждый хит
   и лишний расход контекста: агент либо читает весь файл, либо отвечает по
   заголовку.
2. **Один файл заполняет выдачу.** `duplicate_path_ratio` в baseline: `0.018`
   (identifier) … `0.582` (`ru_morph`), `0.510` (`long_doc_boundary`), `0.487`
   (`multi_relevant`). В половине срезов больше трети хитов — чанки одного файла.
3. **Нет фильтра по пути и тегам.** `docs.rel_path` уникально проиндексирован
   (`migrations.rs:31`), поэтому фильтр по префиксу — это SQL, а не новая схема.
   При этом `frontmatter_json` пишется (`repo.rs:112`, `job.rs:481`) и **никогда
   не читается**: теги в документе не влияют ни на что.

## 2. Goals / Non-goals

**Goals**
- G1: каждый хит несёт короткий фрагмент текста при `snippet: true`.
- G2: выдача не заполняется одним файлом при `dedupe: "path"`.
- G3: поиск сужается по префиксу пути и по тегам frontmatter.
- G4: всё аддитивно и по умолчанию выключено: запрос без новых ключей даёт
  побайтово прежний ответ, `baseline.json` не перегенерируется.
- G5: ни новой схемы tantivy, ни переиндексации — SQL по существующим индексам
  плюс пост-фильтрация расширенного окна кандидатов.

**Non-goals**
- MMR и диверсификация по векторам: дедуп по пути закрывает наблюдаемые 58%
  повторов и не требует векторов в момент ответа.
- Фильтры по заголовку/дате/языку, фасеты, DSL.
- Изменение ранжирования: дедуп и фильтры — пост-фильтры, порядок оставшихся
  хитов не меняется.
- Подсветка с разметкой (фрагмент отдаётся текстом).
- Кросс-проектный поиск (`scope` остаётся `project`).
- Кэш результатов, query-кэш, стоимость (`search-cost`).

## 3. Users / Actors

- **Coding-агент** (основной): один вызов — один взгляд на источники.
- **CLI-пользователь**: `docsbase search --snippet --path-prefix docs/`.
- **Владелец**: следит, чтобы `duplicate_path_ratio` и recall не разъехались.

## 4. Prioritized user stories

### P1 — U1: Фрагмент в хите
**As** агент, **I want** видеть кусок текста чанка, **so that** не делать второй
вызов на каждый хит.

- **Why P1:** прямой расход round-trip и контекста на каждом поиске; не меняет
  ранжирование и не требует окна кандидатов.
- **Independent test:** `search_docs {snippet: true}` даёт у каждого хита
  `snippet` — подстроку `chunks.text` этого чанка, ≤ 240 символов, и
  `snippet_truncated: true`, если текст длиннее.
- **Acceptance scenarios:**
  - Given `snippet: true`, When ответ получен, Then каждый `snippet` — подстрока
    текста своего `(doc_id, seq)` (сверка с БД, не по форме).
  - Given текст длиннее лимита, When фрагмент обрезан, Then обрезка по границе
    символа (не байта) и `snippet_truncated: true`.
  - Given `snippet` отсутствует или `false`, When ответ получен, Then полей
    `snippet`/`snippet_truncated` нет вовсе (не `null`).
  - Given `snippet: true` и `explain: true`, When ответ получен, Then конверт
    `{hits, meta}` содержит те же `snippet` в `hits[]`.

### P2 — U2: Дедуп по документу
**As** агент, **I want** видеть разные файлы, **so that** пять хитов не оказываются
одним документом.

- **Why P2:** эффект больше (58% повторов), но цена выше: нужен расширенный
  запрос кандидатов и замер recall, иначе легко обменять дубли на потерю
  релевантного.
- **Independent test:** на срезе `ru_morph` запрос с `dedupe: "path"` даёт
  `duplicate_path_ratio ≤ 0.20` и **не хуже** `hit` по ожидаемым путям, чем
  без дедупа (снимается существующей per-case метрикой `hit`, см. §9).
- **Acceptance scenarios:**
  - Given `dedupe: "path"`, When 8 чанков одного файла попадают в окно, Then в
    выдаче не больше `n_per_doc` из них, а остальные места занимают следующие
    кандидаты окна.
  - Given `dedupe: "none"` (по умолчанию), When ответ получен, Then состав и
    порядок идентичны текущим.
  - Given окно исчерпано дедупом, When выдача короче `limit`, Then это не ошибка
    и не выдуманный хит: возвращается меньше хитов (окно — `limit × 4`).

### P2 — U3: Фильтр по пути и тегам
**As** агент в подкаталоге, **I want** сузить поиск, **so that** не получать хиты
из чужих разделов.

- **Why P2:** ценно на больших проектах, но опирается на тот же механизм
  расширенного окна, что и дедуп.
- **Independent test:** 50 запросов с `path_prefix: "docs/"` дают ноль хитов вне
  префикса; `tags: ["api"]` даёт только документы с этим тегом.
- **Acceptance scenarios:**
  - Given `path_prefix: "docs/"`, When ответ получен, Then каждый `path`
    начинается с префикса, и фильтр применяется к **окну кандидатов**, а не к
    финальной выдаче.
  - Given `tags: ["api"]`, When ответ получен, Then каждый документ имеет этот тег
    в `frontmatter_json`; документы без frontmatter не проходят.
  - Given фильтр, который ничего не матчит, When ответ получен, Then `hits` пуст,
    `meta` отражает применённые фильтры, ошибки нет.

## 5. Functional requirements

- **FR-1** (must) `snippet: bool` (по умолчанию `false`). При `true` каждый хит
  получает `snippet` (≤ `SNIPPET_MAX_CHARS = 240` символов) и
  `snippet_truncated: true`, если текст обрезан. Фрагмент — префикс текста чанка,
  обрезанный по границе символа. Текст и цитата берутся **одним** JOIN-запросом
  (`repo::citation_with_text`), а не двумя запросами на хит.
- **FR-2** (must) `dedupe: "none"|"path"` (по умолчанию `"none"`), `n_per_doc`
  (`1..=10`, по умолчанию `1`; допустим только вместе с `dedupe: "path"`, иначе
  `Error::Protocol`). Дедуп применяется после `apply_rerank` и **до** усечения до
  `limit`, по `doc_id` из `chunk_id` (без SQL).
  Для добора окно расширяется **только там, где оно равно `limit`**:
  - лексический поиск (`fetch == limit`) при активном дедупе/фильтре:
    `fetch = (limit × SEARCH_UX_OVERSCAN).min(MAX_HITS)`, `SEARCH_UX_OVERSCAN = 4`;
  - fused/reranked (`fetch = max(limit, window)`, обычно 50): окно **не
    расширяется** — в нём уже `window` кандидатов на `limit` мест, и quadrupling
    дал бы вчетверо больше работы реранка без пользы.
  Окно исчерпано — выдача короче `limit`, это нормальный результат.
- **FR-3** (must) `path_prefix: string` (опционально). Фильтр — пост-фильтр по
  **окну кандидатов**, и путь читается тем же одним запросом, что и теги
  (FR-4): `SELECT id, rel_path, frontmatter_json FROM docs WHERE id IN
  (<кандидаты>)`. Сравнение — `rel_path.starts_with(&prefix)` в Rust, то есть
  **буквальное**: без нормализации, без авто-`/`, без регистронезависимости и без
  трактовки `%`/`_` как масок (SQL `LIKE` этого не гарантирует: он ASCII
  case-insensitive и принимает wildcards — поэтому SQL-предикат не используется).
  `docs/` и `docs` — разные префиксы.
- **FR-4** (must) `tags: [string]` (опционально, AND по всем тегам). Источник —
  `docs.frontmatter_json` тех же кандидатов (тот же запрос, что в FR-3).
  Строгость разделена: **аргумент запроса** валидируется строго (не массив или не
  строки → `Error::Protocol`, FR-7), а **данные документа** читаются терпимо (не
  массив/не строки → у документа нет тегов, поиск не падает). `tags: []` —
  отсутствие фильтра.
- **FR-5** (must) Применённые фильтры отражаются в `meta.filters`
  (`{path_prefix, tags, dedupe, n_per_doc}`; отсутствующие ключи — `null`) и
  `meta.trimmed = {filtered_out, deduped}` — сколько кандидатов отброшено по
  каждой причине. Поля присутствуют **только** при `explain: true`; счётчики
  равны `0`, когда соответствующий механизм не активен (не `null`: ноль —
  измеренный факт, `null` — «не считали»). `limit = 0` сохраняет прежнюю форму
  (`[]`, а при `explain` — конверт с `window: 0` и нулевыми счётчиками). Новых токенов в `meta.degraded` не вводится: фильтр — не
  деградация тира, и смешивать их нельзя (правило `search-telemetry` FR-4 —
  добавление разрешено, но смысл `degraded` — «стадия не обслужила запрос»).
- **FR-6** (must) Дефолтный ответ не меняется: без новых ключей `hits[]`
  побайтово прежний (`default_response_shape_is_the_bare_citation_array`
  расширяется кейсами «сниппет/дедуп/фильтр не запрошены»). Гейт
  `metrics_match_baseline` зелёный **без** перегенерации `baseline.json`.
- **FR-7** (must) Валидация в `parse_search_args` (стиль существующих
  `limit`/`rerank`/`explain`): неверный тип любого нового ключа —
  `Error::Protocol`; `n_per_doc` вне `1..=10` — `Error::Protocol`;
  `n_per_doc` без `dedupe: "path"` — `Error::Protocol`; `tags: []` — no-op;
  `limit = 0` — пустой результат в той же форме (сниппет не запрашивается).
- **FR-8** (must) CLI и MCP несут один и тот же набор: `--snippet`, `--dedupe
  <none|path>`, `--n-per-doc <N>`, `--path-prefix <P>`, `--tag <T>` (повторяемый,
  собирается в массив). Direct-режим (`src/cli/search.rs`) передаёт **все**
  аргументы, включая уже существующий `explain` (сейчас он теряется — это
  исправляется в этом же спеке).
- **FR-9** (must) Датированные дельты: `ann-index` FR-10 и `search-quality`
  §2/§11 (оба замораживают форму `search_docs`) получают запись о том, что
  аддитивные opt-in поля и ключи разрешены этим спеком.
- **FR-10** (must) `docs/MCP.md` и `CHANGELOG` описывают новые ключи, поля
  `snippet`/`snippet_truncated`, `meta.filters`/`meta.trimmed` и флаги CLI.
- **FR-11** (must) Замер обязателен и живой: отдельный диагностический тест в
  `tests/search_bench.rs` (не пятая конфигурация в `baseline.json`) прогоняет
  срез `ru_morph` дважды — как сейчас и с `dedupe: "path"` — и сравнивает
  агрегаты, посчитанные существующим `bench::evaluate_case` по каждому кейсу
  (`CaseMetrics.hit` и `CaseMetrics.duplicate_path_ratio`). Числа пишутся в
  леджер; baseline и гейт не меняются. Новой метрики не вводим (`hit` уже есть).

## 6. Non-functional requirements

- **NFR-1 Performance.** `snippet: true` — один JOIN-запрос на весь ответ (не на
  хит); `path_prefix` и `tags` — общий запрос по `id IN (окно)`. Батчи режутся
  по 400 id, поэтому бюджет записывается формулой, а не словом «один»:
  `⌈fetch / 400⌉ + ⌈limit / 400⌉` запросов на ответ (в штатных режимах —
  `fetch ≤ 200` для лексики с дедупом и `window = 50` для fused, `limit ≤ 100`
  — это 2 запроса). На хит запросов по-прежнему не приходится.
  Про работу ранжирования: расширение окна касается только лексического пути и
  составляет `limit × 4`, но `ReadIndex::search` внутри уже расширяет запрос
  (`limit × 4 + 16` с доводкой), поэтому суммарно BM25 может выполнить до ~16×
  работы от `limit` — это признаётся явно, а не замалчивается. Работа реранка не
  растёт: fused/reranked-окно не расширяется (FR-2). Потолок — p95 без регресса
  сверх `search-benchmark` FR-22 (host-relative slack), замер обязателен; жёсткая
  цифра в спеке не фиксируется, её даёт замер (SC-7).
- **NFR-2 No new crates.** Обрезка по символам — `char_indices`; теги — разбор
  `serde_json` (уже в дереве). Дефолтная сборка остаётся офлайн.
- **NFR-3 No reindex.** Ни одна часть не требует новой схемы tantivy или
  переиндексации: `docs.rel_path` и `docs.id` уже проиндексированы,
  `frontmatter_json` уже пишется.
- **NFR-4 Portability.** Linux/Windows/macOS зелёные; тест с `\r\n` и с
  многобайтными символами (обрезка не рвёт UTF-8 и пару CRLF).
- **NFR-5 Security/Privacy.** Фрагмент не выходит за пределы текста чанка; ничего
  не логируется.
- **NFR-6 Evidence.** Дедуп и фильтры принимаются только с замером на
  `search_bench`; «стало лучше» без прогона не принимается.

## 7. Data model

- Схема БД не меняется.
- `snippet` — префикс `chunks.text`, читается одним JOIN вместе с цитатой.
- `tags` — из `docs.frontmatter_json` (ожидаемая форма `{"title":…, "tags":[…]}`,
  её пишет `serialize_metadata`; читатель обязан терпеть любую форму).
- `meta.filters` / `meta.trimmed` — новые поля конверта `explain`
  (расширение `search-telemetry` FR-2, аддитивно).

## 8. Interfaces

- MCP `search_docs {query, limit?, scope?, rerank?, explain?, snippet?, dedupe?,
  n_per_doc?, path_prefix?, tags?}`.
- CLI `docsbase search [--snippet] [--dedupe none|path] [--n-per-doc N]
  [--path-prefix P] [--tag T]... [--explain]`; direct-режим передаёт все ключи.
- `meta` при `explain: true` получает `filters` и `trimmed`; форма остального
  `meta` не меняется.
- `PROTOCOL_VERSION` не меняется.
- Тестовые: `--test hybrid_ranking` (четыре конфигурации: дефолт без новых
  полей, snippet, дедуп, фильтры), `--test search_bench` (замер и
  `duplicate_path_ratio`), `--test mcp_docs` (схема), `--test cli_read`
  (флаги и direct-режим).

## 9. Measurable success criteria

- **SC-1** Дефолт: без новых ключей `hits[]` побайтово прежний; baseline не
  перегенерирован; `metrics_match_baseline` зелёный.
- **SC-2** Фрагмент: `snippet` — подстрока текста своего чанка; ≤ 240 символов;
  `snippet_truncated` корректен; без ключа полей нет; один JOIN на ответ.
- **SC-3** Дедуп: на конфигурации «с дедупом» отчёт `search_bench` даёт
  `duplicate_path_ratio ≤ 0.20` на `ru_morph`, а существующая per-case метрика
  `hit` (`bench.rs::evaluate_case`, попадание ожидаемого пути в `k` кейса) не ниже
  базовой по тому же срезу. Замер — диагностика в отчёте, отдельного CI-гейта не вводится
  (гейт остаётся `metrics_match_baseline` для дефолтной конфигурации).
- **SC-4** Фильтры: `path_prefix` не пропускает чужое; `tags` — AND; пустой
  результат даёт `hits: []` + `meta.filters`, а не ошибку.
- **SC-5** Границы: `n_per_doc` без `dedupe` → `Error::Protocol`; `tags: []` —
  no-op; мусор в `frontmatter_json` — «нет тегов»; `limit = 0` — прежняя форма.
- **SC-6** Офлайн/зависимости: дерево-гейт зелёный, новых крейтов нет.
- **SC-7** Цена: ≤ 2 дополнительных запроса на ответ; расширенное окно не ломает
  бюджет `search-benchmark` FR-22 (замер в отчёте).

## 10. Assumptions & challenges

| # | Челлендж | Резолюция |
|---|---|---|
| 1 | Лексический `fetch == limit`: дедупу неоткуда добирать | Окно расширяется до `limit × 4` при дедупе/фильтре (FR-2); исчерпание окна — короткая выдача, а не выдуманный хит |
| 2 | `path_prefix` «до ранжирования» невозможен: у `ReadIndex::search` нет фильтра по пути | Честная семантика: пост-фильтр по расширенному окну (FR-3); цена названа в NFR-1, возможность — в G5 |
| 3 | Дедуп может уронить recall, когда один файл — единственный ответ | `n_per_doc` ≥ 1, окно × 4, SC-3 требует неухудшения `hit`; дефолт — `"none"` |
| 4 | `tags` — свободный текст, мусор в JSON | Терпимое чтение: не массив/не строки → нет тегов (FR-4, SC-5) |
| 5 | Обрезка по байтам рвёт UTF-8/CRLF | `char_indices`, тест на кириллицу, эмодзи и `\r\n` (NFR-4) |
| 6 | Бюджет запросов легко посчитать неверно (было «≤1 на хит») | Переформулировано: ≤ 2 **на ответ**; сниппет — JOIN (NFR-1) |
| 7 | `filter-empty` как `degraded` смешивает фильтр с деградацией тира | Не вводим токен: `meta.filters`/`meta.trimmed` (FR-5) |
| 8 | Дельты в двух замороженных спеках легко забыть | FR-9 + чек-лист в `tasks.md`; расширение существующего shape-теста дефолта |

## 11. Out of scope

- MMR, семантическая диверсификация.
- Фильтры по дате/заголовку/языку, фасеты, DSL.
- Подсветка с разметкой.
- Кэш результатов, query-кэш, стоимость (`search-cost`).
- Кросс-проектный `scope`.
- Новая метрика recall по уникальным файлам (используется существующий `hit`).

## 12. Open questions

1. `SEARCH_UX_OVERSCAN = 4` — стартовое значение. **Default:** 4, калибруется
   замером на `search_bench` без перегенерации baseline.
2. `SNIPPET_MAX_CHARS = 240`. **Default:** 240; калибруется тем же замером.
3. Дефолт `n_per_doc`. **Resolved замером (2026-10-31): 2.** `1` теряет 3 из 11
   ожидаемых попаданий на `ru_morph`, `2` — одно и даёт `ratio 0.357` против
   `0.582`; `3` возвращает все хиты, но почти не убирает повторы.
4. Считать ли `snippet` от начала чанка или вокруг совпадения. **Default:** от
   начала (дешевле, детерминированно, без токенайзера); «вокруг совпадения» —
   только если появится запрос от пользователей.

## 13. Deltas

- **2026-10-31 — дефолт `n_per_doc` и критерий SC-3 заменены замером.**
  Замерено `tests/search_bench.rs::dedupe_slice_diagnostics` (срез `ru_morph`,
  12 кейсов): plain `duplicate_path_ratio 0.582, hit 11`; `n_per_doc=1` —
  `0.000 / 8`; `=2` — `0.357 / 10`; `=3` — `0.480 / 12`; `=4` — `0.526 / 12`.
  Ни одно значение не даёт одновременно `ratio ≤ 0.20` и сохранения всех `hit`,
  поэтому: (а) дефолт в FR-2 — **2**, а не 1; (б) критерий SC-3 — измеренная
  кривая, а не фиксированный порог `≤ 0.20`: `ratio` обязан падать, потеря
  ожидаемых попаданий — не больше одного. Формулировки FR-2/SC-3 выше сохранены
  как история; действуют значения из этой дельты.
- **2026-10-31 — стоимость новых путей измерена.** `dedupe_slice_diagnostics`
  печатает latency: plain 0.97 мс/запрос, `snippet+dedupe` 0.94 мс/запрос
  (12 кейсов, `ru_morph`), то есть NFR-1/SC-7 подтверждены числом, а не гейтом.

### Delta 2026-11-09 — #61: блочные списки тегов

- **FR-12:** `tags:` с последующими строками `- value` сохраняется как тот же
  массив тегов, что и прежний `tags: a, b`. Поддерживаются одинаковый отступ
  элементов (включая YAML indentless sequence), LF/CRLF, парные кавычки,
  пустые строки и комментарии. Запятые/двоеточия внутри строкового элемента
  не разделяют его; пустые элементы не становятся тегами. Списки других ключей
  и вложенные элементы не добавляются в `tags`. Полный YAML (flow sequences,
  maps, anchors, multiline scalars, escape sequences) остаётся вне контракта.
- **FR-13:** полная синхронизация обновляет устаревшие метаданные ранее
  проиндексированных byte-identical файлов; инкрементальная — при обработке
  соответствующего пути. После обновления следующий запуск снова no-op.
  Изменения проходят существующий crash-safe путь публикации поколения;
  schema/tokenizer version и ranking не меняются. Это уточняет прежний NFR-3:
  ручная очистка индекса не нужна, затронутые документы обрабатываются повторно.
- **SC-8:** тесты доказывают равенство сохранённых метаданных двух форм,
  восстановление legacy `NULL` и `{"title":"Guide","tags":[]}` при совпадающем
  source hash, отсутствие повторного обновления и работу AND-фильтра через
  реальный `search_docs` IPC-вызов (с правильными строками citation).

### Delta 2026-11-09 — #62: точный счётчик отказов фильтра

- **FR-14:** `meta.trimmed.filtered_out` считает кандидатов, для которых
  существует строка `docs` и не выполнен `path_prefix` или AND по `tags`.
  Кандидат без строки документа исключается из выдачи без увеличения счётчика:
  отсутствие документа не является отказом фильтра. Отсутствующие/некорректные
  tags у существующего документа по-прежнему считаются отказом активного
  tag-фильтра. Два несовпадения одного кандидата считаются один раз.
  Форма `meta.trimmed = {filtered_out, deduped}` остаётся прежней; новых
  публичных счётчиков нет. Это уточнение FR-5, без изменения самой выдачи.
- **SC-9:** настоящий поиск по Tantivy/SQLite после удаления документа только
  из SQLite сохраняет stale lexical candidate, возвращает правильные живые
  документы и `filtered_out == 0` при активном path/tag/combined фильтре.
  При добавлении реально не подходящего живого кандидата счётчик равен `1`,
  независимо от количества несовпавших фильтров. JSON-форма счётчиков прежняя.

### Delta 2026-11-11 — #67: top-k within path/tag scope

- **FR-15:** path prefix and tag AND define eligible documents before each
  source limit. Preserve corpus BM25 statistics; dense top-k is exact over
  eligible cached vectors, including dense-only matches. RRF uses scoped
  per-source ranks. Rerank receives the eligible bounded window once.
  Validate syntax/literal queries even when the scope is empty.
- **FR-16:** preserve literal case-sensitive UTF-8 `starts_with` semantics,
  including `%`, `_`, NUL and Unicode; tolerant stored JSON tags; query errors
  and existing dedupe. No index migration or tokenizer change.
- **FR-17:** scope and candidates use one metadata generation. Revalidate
  generation/pending/config in later read phases. Admit logical preparation
  bytes before allocating IDs using existing search byte limits, then release
  scope before payload admission. No scope cache/new hidden default. Check
  cancellation/deadline between rows/pages. Limits are allowances, not RSS.
- **FR-18:** filtered dense streams eligible chunk hashes in bounded pages,
  with no SQL/project/config lease over vector IO/provider work and no
  whole-project dense cache initialization or per-scope ANN graph. This first
  path trades broad-scope ANN latency for exactness; filtered ANN is deferred.
- **FR-19:** `meta.window` is the planned eligible chunk window (possibly
  larger than the actual list). `trimmed.filtered_out` counts only observed
  final defensive post-filter rejections, normally zero with eager scopes;
  excluded corpus documents are not counted. Missing metadata is not a
  rejection (#62). This supersedes the old post-window filter/SQL-count
  guarantees (FR-3/4, NFR-1, SC-7/9). Dedupe stays bounded pending #68.
- **SC-10:** real lexical/dense searches find eligible candidates beyond old
  ×4/40/1000 windows, preserve BM25 scores/scoped RRF, and pass the unchanged
  quality gate. Cover empty/malformed scope, stale generations, cancellation
  and admission cleanup.
- **SC-11:** measure 20k two-folder broad/narrow scopes vs a query-first
  reference, cold/warm latency and memory accounting. No optimal-planner or
  filtered-ANN-recall claim.

### Delta 2026-11-11 — #67 measured vector IO correction

- **FR-20:** eligible vector pages decode one vector at a time and share one
  short vector-cache write transaction for LRU touches, with prepared statements
  reused within the page. Reserve the write transaction before reads (avoid WAL
  upgrade races); commit/rollback ends before the next metadata phase. Failed
  decoding, visitor/cancellation error or unwind must release the transaction;
  no partial search response is published. This does not retain a vector page
  or alter score/tie/missing-vector/model/cache-key semantics.
- **SC-12:** real cache tests verify model isolation, missing rows, visitation
  order/LRU, rollback and reuse after error; existing exact-streaming tests and
  scoped generation/cancellation/late-page regressions remain green. Repeat the
  same 20k release measurement; report the initial and corrected results.


## Delta 2026-11-11 — #68: bounded document-cap refill

Эта дельта заменяет отказ от добора из FR-2/NFR-1 для `dedupe=path`.
Без dedupe поведение, query semantics, scoring и baseline не меняются.

- **FR-21:** Начать с прежнего scoped base window. Вместимость пула
  `capacity(P)=Σ_doc min(count_doc,n_per_doc)`; пока она меньше limit,
  расширять source/retained windows ×SEARCH_UX_OVERSCAN до
  `max(base_window,search.dedupe_max_candidates)`. Не удалять дубликаты
  перед rerank. Сохранить все кандидаты base pool; новые места заполнять
  лучшими кандидатами в текущем порядке. После одной rerank-операции —
  прежний cap и chunk-count limit, с сохранением порядка surviving hits.
- **FR-22:** SearchSpec владеет global-only `dedupe_max_candidates`, default1000,
  целое 1..=MAX_HITS(1000), provenance Count. Это потолок дополнительного
  добора, не уменьшение обычного configured/limit base window. Один retained
  пул <= effective ceiling; source lists <= ceiling каждая, fusion временно
  <=2×ceiling. Byte admission scope/payload не перекрывается; превышение
  bytes/deadline/cancel — прежняя typed error, без partial response.
- **FR-23:** RRF использует scoped source ranks. Для cap-запроса dense list
  захватывается один раз до effective ceiling и используется префиксами:
  exact scan/ANN/provider-query не повторяются при lexical widening; это
  также фиксирует один approximate ANN result для запроса. Base pool строится
  из этих же source lists. Переупорядочивание fusion не удаляет base chunks.
  Без cap прежний dense fetch сохраняется. Ни новый relevance threshold,
  ни MMR/ACL, ни score calibration разных rerank batches не вводятся.
- **FR-24:** `explain.meta.refill` только для ненулевого cap-запроса содержит
  `stop_reason`: satisfied (пул и результат вместили limit), source-exhausted
  (оба активных источника доказанно закончились и вся fusion сохранена),
  candidate-budget (остались неизвестные/неудержанные кандидаты), либо
  post-filter-shortfall (финальная защита/отсутствующая citation сократила
  подготовленный достаточный пул). Поля candidate_limit, candidates, capacity,
  rounds, dense_window объясняют достигнутый бюджет; window — последний
  planned retained/source prefix. Полное окно без sentinel не доказывает
  exhaustion. Trimmed считает только финальный пул один раз; stage_ms —
  сумма работы стадии по всем раундам. Это не total match count/pagination.
- **FR-25:** Каждый новый lexical phase повторно проверяет прежнюю generation,
  pending и config admission. Scope строится один раз; SQL/reader/project/config
  leases не держатся через vector IO/providers, старые candidates не смешиваются
  с новым поколением. Новый cache/derived artifact не создаётся; настройки
  добора меняют только request policy (restart), не vector/query cache identity.
- **SC-13:** Реальный индекс с 40 сильными chunks одного документа +10 слабыми
  документами возвращает10 chunks при default cap2, лексика/RRF/оба rerank пути.
  Поздний chunk может стать лучшим после ровно одного rerank. Один полезный
  документ даёт cap2 + source-exhausted, без filler. >1000 доминирующих chunks
  дают cap2 + candidate-budget. Проверить явный небольшой policy ceiling,
  scopes, RRF score/rank, обычный путь без cap, state changes, byte cleanup.
- **SC-14:** Повторить ru_morph diagnostic, unchanged-baseline gate, MSRV1.88
  cli-full suite, fmt/clippy и native docs/linux/macos/windows. Отдельно измерить
  release no-refill/refill/budget сценарии без paid/live API.
