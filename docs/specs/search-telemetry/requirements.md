# Search Telemetry — Requirements (SPECIFICATION)

> Feature: `search-telemetry` — то, что вызывающий обязан узнать о своём же
> поиске: на какой шкале `score`, куда ушло время, в каком состоянии индекс и
> почему тир деградировал.
> Контекст продукта: `docs/specs/docsbase-memory-mcp/{requirements,design}.md`,
> принципы: `docs/specs/constitution.md`, предшественники:
> `docs/specs/search-quality/` (форма выдачи), `docs/specs/ann-index/`
> (FR-9/FR-10/OQ-6 — диагностика ANN и заморозка формы), `docs/specs/hybrid-providers/`
> (стадии RRF/rerank, FR-11/FR-16 — окно кандидатов),
> `docs/specs/search-benchmark/` (FR-22 — бюджет латентности).

## 1. Problem

MCP-поверхность отдаёт агенту `{chunk_id, path, heading_path, lines, score}`, и по
этому ответу невозможно понять, что именно произошло:

1. **Шкала `score` не названа.** Один и тот же ключ несёт сырой BM25 (неограничен,
   зависит от корпуса), RRF-фьюжн (`1/(k+rank)`, порядок 0.01–0.03) и выход
   reranker'а (`0..1`). Агент не может ни поставить порог, ни сравнить два ответа
   после смены конфигурации — а именно это он и делает, когда «поиск стал хуже».
2. **Время не атрибутировано.** `search_docs` не говорит, сколько заняли BM25,
   dense, RRF и rerank (`grep -n "Instant" src/daemon/tools.rs` — ни одного замера
   на пути поиска). Медленный поиск невозможно ни объяснить, ни уложить в бюджет;
   офлайн-гейт `search-benchmark` меряет другой путь.
3. **Деградация невидима.** ANN может не обслуживать запрос (билд в процессе,
   граф неполный, `TooLarge`, ошибка загрузки) — читатель молча уходит в exact
   `stream_hashes`. Единственный сигнал — `eprintln!`
   (`src/daemon/tools.rs:122,128`), который агент не читает. `status` не отдаёт
   ни состояния ANN, ни `index_generation`: FR-9 `ann-index` объявлен как «should»,
   а OQ-6 закрыл его как log-only — этот спек переводит его в must (см. §13).
4. **Устаревание невидимо.** `index_generation` есть в БД (`migrations.rs:56`,
   `repo.rs:300`), а dense-кэш использует его как ключ записи (не хранит), но в
   `status` и в ответе поиска его нет: агент не отличает «документ действительно
   изменился» от «индекс отстал на одну синхронизацию».

Документация уже приведена в соответствие (`docs/MCP.md`: раздел Scores), но
документ не заменяет поле в ответе.

## 2. Goals / Non-goals

**Goals**
- G1: по ответу видно, на какой шкале `score` и какие стадии работали.
- G2: время разложено по стадиям, сумма не превышает общее.
- G3: `status` показывает `index_generation` и состояние ANN-тира, включая
  причину отката в exact.
- G4: форма ответа по умолчанию **не меняется** — новые поля приходят только по
  явному запросу (`explain: true`), `hits[]` побайтово прежний.
- G5: всё это — без новых крейтов, без сети в дефолтной сборке и без влияния на
  ранжирование (гейт `metrics_match_baseline` зелёный без перегенерации).

**Non-goals**
- Ломающее изменение публичной формы ответа: конституция (`constitution.md:50,123`)
  разрешает его только в мажорной версии, а текущая — `0.1.0-alpha.5`. Конверт
  `{hits, meta}` вместо массива отклонён (см. §10, челлендж 1).
- Стоимость запроса (`cost_usd`): требует канала usage в `Embedder`/`Reranker` и
  разбора usage в `TransportResponse` (`src/hybrid/wire.rs:14` — только
  `{status, body, retry_after}`), то есть изменения трейтов и wire. Отдельный
  спек `search-cost`.
- OpenTelemetry-экспорт, трейсы, коллектор, метрик-эндпоинт — отдельная итерация
  (ADR + сетевой стек).
- Изменение ранжирования: окно rerank, `rrf_k`, веса, адаптивные пороги
  (`search-cost`).
- Сниппеты, дедуп по пути, фильтры (`search-ux`).
- Логи как второй канал наблюдаемости (агент их не читает).
- Хранение истории метрик, агрегаты, перцентили в `status`.

## 3. Users / Actors

- **Coding-агент** (основной): читает JSON; должен решать, доверять ли порядку,
  стоит ли повторять запрос и почему поиск медленный.
- **CLI-пользователь**: `docsbase search --explain`, `docsbase status`.
- **Владелец/мейнтейнер**: по `stage_ms` и `ann` видит, обслуживает ли ANN
  реальный трафик и куда уходит время.

## 4. Prioritized user stories

### P1 — T1: Шкала и состав стадий
**As** агент, **I want** знать, что означает `score` в этом ответе, **so that**
я не сравниваю RRF-величины 0.016 с BM25-скором 12.4 и не строю на этом порог.

- **Why P1:** без этого ответ неинтерпретируем, а любая эвристика агента строится
  на сравнении чисел между вызовами.
- **Independent test:** для каждой из четырёх конфигураций (`Bm25`, `Bm25Rerank`,
  `Rrf`, `RrfRerank`) тест вызывает `search_docs {explain: true}` и утверждает
  `meta.score_scale` и `meta.stages`.
- **Acceptance scenarios:**
  - Given проект с `ranking = "rrf"` без reranker, When `search_docs {explain:
    true}`, Then `meta.score_scale == "rrf"` и
    `meta.stages == ["bm25","dense","rrf"]`.
  - Given активный reranker, When `explain: true`, Then
    `meta.score_scale == "rerank"` и `"rerank"` ∈ `meta.stages`.
  - Given лексический проект, When `explain: true`, Then
    `meta.score_scale == "bm25"` и `meta.stages == ["bm25"]`.
  - Given **любой** запрос без `explain`, When ответ получен, Then это ровно
    сегодняшний массив (`serde_json::to_string` совпадает побайтово).

### P1 — T2: Тайминги по стадиям
**As** владелец, **I want** видеть, куда ушло время запроса, **so that** я могу
отличить медленный BM25 от медленного провайдера и обосновать бюджет.

- **Why P1:** это единственный источник данных для `search-cost` и для разбора
  «поиск стал медленнее».
- **Independent test:** поддельный провайдер блокируется на **гейте** (не на
  `sleep` — см. `docs/daemon-test-flakes.md`, Flake #9); тест отпускает гейт после
  того, как убедился, что поиск в полёте, и утверждает, что
  `stage_ms[<стадия>] >= задержка` с допуском `max(5 мс, 25 %)`.
- **Acceptance scenarios:**
  - Given любой `explain`-поиск, When ответ получен, Then `meta.stage_ms`
    содержит запись для каждой активной стадии и `total`.
  - Given стадия не активна, When ответ получен, Then её ключа в `stage_ms` нет
    (а не `0`, который неотличим от «быстро»).
  - Given любой `explain`-поиск, When сумма стадий сравнивается с `total`, Then
    `sum(stages) <= total` (замеры вложены, накладные расходы не приписываются
    стадиям).

### P1 — T3: Состояние индекса и ANN в `status`
**As** владелец, **I want** видеть в `status`, какой тир обслуживает поиск и
почему, **so that** молчаливый откат в exact перестаёт быть невидимым.

- **Why P1:** FR-9 `ann-index` объявлен и не реализован; сейчас единственный
  сигнал — `eprintln!` в лог демона.
- **Independent test:** тест поднимает проект, где ANN обслуживает поиск, и
  проект, где он не может (граф неполный / `TooLarge`), и утверждает разные
  `ann.state` с непустым `reason` во втором случае.
- **Acceptance scenarios:**
  - Given проект, где ANN обслужил последний поиск, When `status`, Then
    `projects[i].ann.state == "ready"`, `ann.vectors > 0`,
    `ann.reason == null`.
  - Given ANN откатился в exact, When `status`, Then `ann.state` —
    `"building"|"too_large"|"fallback"` и `ann.reason` непуст.
  - Given проект без hybrid-стадии, When `status`, Then `ann.state ==
    "disabled"` и `ann.reason` непуст (`"hybrid off"`).
  - Given любой проект, When `status`, Then `projects[i].index_generation` равно
    `repo::index_generation`.

### P2 — T4: Та же информация в CLI
**As** CLI-пользователь, **I want** получить те же поля, **so that** разбор
инцидента не требует поднимать агент.

- **Why P2:** ценно, но MCP — основной потребитель.
- **Independent test:** `docsbase search --explain` печатает конверт; без флага —
  прежний массив; `docsbase status` несёт `ann` и `index_generation`.
- **Acceptance scenarios:**
  - Given `docsbase search "q"`, When без `--explain`, Then вывод побайтово
    прежний.
  - Given `docsbase search "q" --explain`, When проект гибридный, Then в JSON есть
    `meta` с теми же полями, что у MCP.

## 5. Functional requirements

- **FR-1** (must) `search_docs` принимает `explain: bool` (по умолчанию `false`).
  При `explain: true` ответ — конверт `{hits, meta}`; при `false` — сегодняшний
  массив, побайтово. Аргумент аддитивен: старые клиенты его не шлют и видят
  прежнюю форму. `PROTOCOL_VERSION` не меняется (IPC-кадр непрозрачен, форма
  задаётся MCP-слоем).
- **FR-2** (must) `meta` содержит: `score_scale`
  (`"bm25"|"rrf"|"rerank"`), `stages` (активные, в порядке исполнения),
  `stage_ms` (только активные стадии + `total`, `u64` миллисекунды от
  монотонного `Instant`), `window` (фактический fetch после clamp до `MAX_HITS`,
  см. `hybrid-providers` FR-11/FR-16), `index_generation` (`i64` из
  `repo::index_generation`), `degraded` (массив стабильных токенов).
- **FR-3** (must) `stage_ms` содержит только реально исполненные стадии;
  `sum(stage_ms[active]) <= total`. Допуск на замер — `max(5 мс, 25 %)` в тестах;
  накладные расходы не приписываются ни одной стадии.
- **FR-4** (must) `degraded` перечисляет причины, а не свободный текст: стабильные
  токены `"ann-building"`, `"ann-insufficient"`, `"ann-error"`,
  `"exact-fallback"`, `"no-rerank-stage"`. Список — часть контракта: только
  добавление новых токенов, без переименований. Пустой массив — нормальный ответ.
- **FR-5** (must) `status` (MCP и CLI) отдаёт `projects[i].index_generation` и
  `projects[i].ann = {state, vectors, estimated_bytes, reason}`;
  `state` ∈ `disabled|loading|building|ready|too_large|fallback|stale`;
  `reason` — непустая строка для любого состояния, кроме `ready`.
- **FR-6** (must) Состояние ANN для `status` берётся из **сохранённого последнего
  исхода**, а не из повторного билда: путь поиска пишет `{state, reason, at}` в
  ограниченное хранилище (TTL + не более одной записи на проект), `status` его
  читает. Нет записи и hybrid выключен → `disabled`; hybrid включён, записи нет →
  `loading` с `reason: "no search yet"`. `vectors`/`estimated_bytes` берутся из
  `AnnIndex` (`ann.rs`), когда он резидентен, иначе `null`.
- **FR-7** (must) Форма ответа без `explain` не меняется: `hits[]` побайтово равен
  текущему, гейт `metrics_match_baseline` зелёный **без** перегенерации
  `baseline.json`. Все существующие потребители массива (`47` вхождений
  `.as_array()` в `19` файлах `tests/`) не трогаются.
- **FR-8** (must) Границы: `limit = 0` возвращает пустой результат в той же форме,
  что и раньше (`[]`, а при `explain` — `{hits: [], meta}` с `window: 0`);
  ошибки (`Error::Query`, `Error::Project`, `Error::Protocol`) **не** несут `meta`;
  пустая выдача при `explain` даёт `meta.score_scale` по активной конфигурации.
- **FR-9** (must) `docs/MCP.md` и `CHANGELOG` описывают `explain`, поля `meta` и
  новые поля `status` в том же изменении. `docs/MCP.md` — перечень изменённых
  артефактов, не «упоминание в §7».
- **FR-10** (should) Имена полей совместимы с OTel GenAI conventions
  (`gen_ai.operation.name = retrieval`); соответствие фиксируется таблицей
  констант в прод-коде и unit-тестом «каждое имя стадии имеет OTel-пару».
  Экспортёр — вне спека.

## 6. Non-functional requirements

- **NFR-1 Performance.** Накладные расходы телеметрии ≤ 1 мс p95 на поиск
  (замер до/после на `search_bench`); бюджет p95 поиска задаётся
  `search-benchmark` FR-22 и здесь не переопределяется (включая slack-формулу
  host-relative).
- **NFR-2 No new crates.** Дефолтная сборка остаётся офлайн; сетевых крейтов не
  добавляется; дерево-гейт (NFR-1 продукта) не трогается.
- **NFR-3 Maintainability.** Каждый FR покрыт автотестом; имена стадий и токены
  `degraded` — константы, а не строки по месту.
- **NFR-4 Portability.** Linux x86_64, Windows x64, macOS aarch64 — CI зелёный;
  тайминги в тестах с допуском `max(5 мс, 25 %)`, гейт вместо `sleep`
  (`docs/daemon-test-flakes.md`, Flake #9).
- **NFR-5 Security/Privacy.** В `meta` нет содержимого документов и запроса; в
  `ann.reason` нет путей, кроме уже публичных в `status`.
- **NFR-6 Evidence.** Любое изменение публичной формы (включая добавление
  `explain`) фиксируется в `CHANGELOG`, `docs/MCP.md` и в спеке; «молчаливых»
  изменений контракта нет.

## 7. Data model

- **`SearchMeta`** (прод-код, сериализуемая): `{score_scale: enum, stages:
  [enum], stage_ms: {<stage>: u64, total: u64}, window: usize,
  index_generation: i64, degraded: [enum]}`.
- **`AnnState`** (прод-код, для `status`): `{state: enum, vectors:
  Option<usize>, estimated_bytes: Option<u64>, reason: Option<String>}`.
- **`AnnOutcome`** (новое, хранилище последнего исхода): `{project_id, state,
  reason, at: Instant}`; одна запись на проект, TTL, вытеснение по LRU. Источник —
  путь поиска (`dense_candidates`), потребитель — `status`.
- **`index_generation`** уже есть в `projects` (`migrations.rs:56`) и
  `repo::index_generation`; выносится в JSON без изменения схемы БД.

## 8. Interfaces

- MCP `search_docs {query, limit?, scope?, rerank?, explain?}` →
  `[...]` (по умолчанию, без изменений) или `{hits: [...], meta: {...}}` при
  `explain: true`. Элемент `hits[]` не меняется.
- MCP `status` → добавляются `projects[i].index_generation` и
  `projects[i].ann`; остальные поля не меняются (аддитивно).
- CLI `docsbase search --explain` → тот же конверт; без флага — прежний массив.
  `docsbase status` → те же новые поля.
- `PROTOCOL_VERSION` не меняется.
- Тестовые: `cargo test --test hybrid_ranking` (шкала/стадии/форма по умолчанию),
  `--test mcp_frontend` (конверт при `explain`), `--test status`
  (`ann`/`index_generation`), `--test search_bench` (baseline без перегенерации),
  `--test cli_read` (`--explain` и прежний вывод).

## 9. Measurable success criteria

- **SC-1** Форма по умолчанию: для всех четырёх конфигураций ответ без `explain`
  побайтово равен ответу до изменения (фикстура-снапшот), гейт зелёный без
  перегенерации `baseline.json`.
- **SC-2** Шкала: при `explain` для всех четырёх конфигураций `meta.score_scale`
  верен, `meta.stages` совпадает с фактически исполненными стадиями.
- **SC-3** Тайминги: `sum(stage_ms[active]) <= total`; стадия с гейтованной
  задержкой видна в своём ключе с допуском `max(5 мс, 25 %)`; неактивной стадии в
  `stage_ms` нет.
- **SC-4** Статус: `projects[i].index_generation == repo::index_generation`;
  `ann.state` различает `ready` и откат; `reason` непуст при откате и при
  `disabled`.
- **SC-5** Границы: `limit = 0`, пустая выдача, `Error::Query` — форма и отсутствие
  `meta` соответствуют FR-8.
- **SC-6** Офлайн и зависимости: дерево-гейт зелёный, новых крейтов нет,
  `cargo test --locked` в дефолтной сборке проходит.
- **SC-7** Документация: `docs/MCP.md` описывает `explain`, `meta` и новые поля
  `status`; `CHANGELOG` фиксирует добавление `explain` (аддитивно, не ломающее).

## 10. Assumptions & challenges

| # | Челлендж | Резолюция |
|---|---|---|
| 1 | Хочется отдавать `meta` всегда — «одна форма, один код-путь» | Отклонено: `constitution.md:50,123` разрешает ломающее изменение только в мажоре, а версия `0.1.0-alpha.5`; `search-quality` §2/§11 и `ann-index` FR-10 замораживают форму. Поэтому `explain` opt-in: дефолт не меняется, 47 потребителей массива не трогаются |
| 2 | Замер времени искажает сам себя | Замеры вложенные: `sum(stages) <= total`, накладные не приписываются стадиям; допуск `max(5 мс, 25 %)`; NFR-1 — ≤1 мс p95 |
| 3 | `status.ann` требует состояния, которого демон не хранит | Новый `AnnOutcome` (одна запись на проект, TTL/LRU), пишется путём поиска, читается `status`; `vectors`/`estimated_bytes` — только если `AnnIndex` резидентен, иначе `null` |
| 4 | Тест таймингов может флейкать (sleep-синхронизация) | Гейт вместо `sleep` (Flake #9); допуск host-relative; проверяется присутствие и порядок величин, не точное значение |
| 5 | OTel-имена без OTel-зависимости | Таблица констант + unit-тест пары; экспортёр — отдельный спек/ADR |
| 6 | Телеметрия может «протечь» в baseline | FR-7/SC-1: дефолтная форма побайтово стабильна, гейт без перегенерации — обязательный регресс-тест |
| 7 | `degraded`-токены могут стать де-факто контрактом без правил | §5 FR-4: список — часть контракта, только добавление, без переименований; тест фиксирует множество |

## 11. Out of scope

- `cost_usd` и канал usage в провайдерах (`search-cost`).
- OTel/tracing-крейты, коллектор, дашборды, метрик-эндпоинт.
- Изменение окна rerank/весов/`rrf_k` и любые изменения ранжирования (`search-cost`).
- Сниппеты, дедуп, фильтры (`search-ux`).
- Хранение истории метрик, агрегаты по времени, перцентили в `status`.
- Логи как канал наблюдаемости.

## 12. Open questions

1. Гранулярность `stage_ms`: только стадии верхнего уровня или ещё подшаги
   (BM25 fetch vs top-k, dense embed vs ANN search). **Default:** верхний уровень;
   подшаги — по запросу при разборе конкретной деградации.
2. `ann.vectors` для нерезидентного индекса: `null` или чтение `meta.json` из
   `AnnStore`. **Default:** `null` (без дискового чтения на каждый `status`).
3. Нужен ли `meta.trace_id` для корреляции с логом демона. **Default:** нет, пока
   нет экспортёра; `index_generation` + время покрывают разбор.
4. TTL для `AnnOutcome`. **Default:** 1 час; `stale` — если запись старше TTL.

## 13. Deltas

- **2026-10-31 — supersede `ann-index` OQ-6/FR-9.** `ann-index` объявил диагностику
  ANN в `status` как «should», а OQ-6 закрыл вопрос как log-only. Этот спек
  переводит её в must (`FR-5`/`FR-6`) с сохранённым последним исходом. Форма
  `search_docs` при этом **не** меняется (FR-1/FR-7), поэтому `ann-index` FR-10
  остаётся в силе без правок.
