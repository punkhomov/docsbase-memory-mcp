# Search Benchmark — Requirements (SPECIFICATION)

> Feature: `search-benchmark` (спека 1, ревизия 2, 2026-10-19).
> DELTA-1 (2026-10-19): adjudication `absent`, рубрикатор + второе ревью,
> лексический контроль semantic-среза, saturation/config-contribution гейт
> (FR-26…FR-32; SC-11…SC-13).
> DELTA-2 (2026-10-19): баунд чанков FR-1 500–1000 → 200–400 (замер: 62
> дока дают 132 чанка при `MAX_CHUNK_CHARS=1500`; масштаб покрывают внешние
> бенчи как primary, внутренний корпус — глубину: ловушки, границы).
> Контекст: `docs/specs/docsbase-memory-mcp/{requirements,design}.md`,
> `docs/specs/search-quality/` (текущий golden-гейт),
> `docs/specs/hybrid-providers/` (BM25 + dense + RRF + rerank),
> принципы: `docs/specs/constitution.md` (1.3.0).
> Заменяет golden-гейт `search-quality` только после parity (FR-16); до этого
> оба harness'а сосуществуют.

## 1. Problem

Текущий golden-набор (`tests/fixtures/search-quality/`, 12 кейсов) больше не
различает улучшения и регрессии:

1. **Насыщен.** Все шесть классов (`ru`, `en`, `identifiers`, `phrases`, `cjk`,
   `arabic`) имеют `hit_rate = 1.0` и `nDCG = 1.0`. Любое изменение либо
   проходит, либо ломает уже пойманный кейс; «стало лучше» измерить нечем.
2. **Не покрывает hybrid.** RRF и rerank не входят в гейт: live-замеры —
   `#[ignore]`, без сети в CI. Регрессия fused-ранжирования или rerank-окна
   не ловится.
3. **Нет hard negatives.** Почти все кейсы имеют один ожидаемый документ и
   пустой `absent`; near-duplicate ловушки, ложные лексические совпадения и
   перефразировки отсутствуют.
4. **Path-level разметка.** Проверяется только документ, не цитата: ошибка
   чанка/строк и ответ на границе чанка не видны, хотя агент получает
   `lines` и читает именно их.
5. **Нет операционных метрик.** Стоимость, токены и latency гибридных стадий
   не измеряются; решения о `window`, RRF и rerank принимаются без цифр.
6. **Live-измерения не воспроизводимы.** Нет записанных векторов и
   rerank-скоров; повторный замер зависит от сети, ключа и дрейфа провайдера.

Следствие: конституционное правило «изменение ранжирования только с замером
на golden» (NFR-6) выполняется формально, но не защищает от регрессий.

## 2. Goals / Non-goals

**Goals**

- G1: детерминированный offline-гейт для четырёх конфигураций: `bm25`,
  `bm25+rerank`, `rrf`, `rrf+rerank`.
- G2: дискриминирующий корпус: hard negatives, перефразировки, mixed-language,
  multi-relevant, long-doc/boundary, плюс отдельная parity-проверка текущих
  12 кейсов.
- G3: разметка path + lines + grades и метрики точности цитаты.
- G4: live-замер качества, latency и стоимости на реальном OpenRouter.
- G5: версионируемые fail-closed фикстуры и baseline; промах фикстуры — ошибка,
  а не skip.
- G6: воспроизводимость на Linux/Windows/macOS без сети в offline-режиме.

**Non-goals**

- Новые retrieval-ступени (ANN, локальные модели, query expansion, тюнинг
  RRF-весов/`k`) — отдельные итерации.
- Изменение публичных контрактов MCP/CLI или формата `search_docs`.
- Автоматическая LLM-разметка релевантности.
- Порог «нет ответа»/relevance cutoff: продукт его пока не имеет; `absent`
  ловушки проверяются, отдельный no-answer-класс — вне спеки.
- Production-телеметрия и UI-дашборд.
- Замена `perf_budget`/`search_golden`: бенчмарк дополняет их, а не дублирует
  бюджетные проверки.

## 3. Users / Actors

- **Мейнтейнер поиска** — принимает решения о токенайзере, бустах, RRF и
  rerank по цифрам; добавляет кейсы и регенерирует baseline.
- **Ревьюер PR** — проверяет, что изменение не ухудшило ни один срез и не
  спрятало регрессию за средним.
- **CI** — гоняет offline-гейт на каждом PR; live-прогон — отдельно, по
  расписанию/вручную.
- **Release-инженер** — сверяет live-отчёт (latency/стоимость) перед релизом.

## 4. Prioritized user stories

### P1 — Offline no-regression gate

**As** мейнтейнер, **I want** детерминированный прогон всех четырёх конфигураций
на фиксированном корпусе, **so that** регрессия ранжирования или цитат
блокирует PR без сети и ключа.

- **Why P1:** это основной защитный механизм; без него остальные метрики —
  отчётность.
- **Independent test:** тест дважды строит отчёт на одном корпусе и получает
  идентичный результат; подмена буста/ранжирования ломает гейт.
- **Acceptance scenarios:**
  - Given offline-корпус и replay-фикстуры, When запускается гейт, Then все
    четыре конфигурации дают отчёт с per-case и per-slice метриками.
  - Given baseline без изменений, When гейт запускается дважды, Then отчёты
    побайтно идентичны.
  - Given намеренно сломанный буст `title`, When гейт запускается, Then он
    падает с указанием среза и кейса.

### P2 — Дискриминирующие срезы и hard negatives

**As** мейнтейнер, **I want** видеть метрики по классам запросов и отдельные
ловушки, **so that** улучшение одного класса не маскирует деградацию другого.

- **Why P2:** именно этого не хватает текущему насыщенному набору.
- **Independent test:** каждый срез имеет не менее 8 кейсов; near-duplicate
  ловушка ловится как `forbidden_hit`, а перефразировка требует dense-канала.
- **Acceptance scenarios:**
  - Given hard-negative кейс, When запрещённый документ попадает в top-k,
    Then кейс и гейт падают независимо от среднего nDCG.
  - Given перефразировка без общих лексических терминов, When включён `rrf`,
    Then ожидаемый документ поднимается в top-k; на `bm25` кейс может быть
    negative-control и помечен как таковой.
  - Given срез с менее чем 8 кейсами, When фикстуры валидируются, Then
    валидация падает с перечнем недобора.

### P3 — Live-замер качества, latency и стоимости

**As** release-инженер, **I want** прогон на реальном провайдере с отчётом
quality/latency/стоимость, **so that** дрейф модели и цена решения видны до
релиза.

- **Why P3:** offline replay не показывает реальную latency и стоимость.
- **Independent test:** live-тест под `#[ignore]` и feature `hybrid-remote`
  пишет JSON-отчёт с моделью, временем, usage и стоимостью; не входит в PR-гейт.
- **Acceptance scenarios:**
  - Given `OPENROUTER_API_KEY`, When запускается live-прогон, Then отчёт
    содержит все конфигурации, p50/p95, токены/`search_units` и оценку
    стоимости.
  - Given превышен cost cap, When прогон доходит до лимита, Then он
    останавливается с явной ошибкой и не тратит дальше.
  - Given live-регрессия, When отчёт сравнивается с предыдущим, Then разница
    видна как warning, но PR-гейт не блокируется.

### P4 — Запись и версионирование фикстур

**As** мейнтейнер, **I want** явно перезаписывать векторы, rerank-скоры и
baseline, **so that** смена модели или чанкера была осознанным действием.

- **Why P4:** без record mode fail-closed фикстуры станут необслуживаемыми.
- **Independent test:** record mode на неизменном коде даёт пустой diff; после
  смены model pin — ожидаемый diff только в фикстурах/baseline.
- **Acceptance scenarios:**
  - Given неизменные корпус/запросы/модель, When запускается record mode,
    Then фикстуры и baseline не меняются (кроме timestamp в отчёте).
  - Given новый model pin, When запускается record mode, Then старые фикстуры
    не переиспользуются молча, а baseline требует явного обновления.

### P5 — Отчёт и CI-интеграция

**As** ревьюер, **I want** машинно-читаемый отчёт и понятную команду запуска,
**so that** я вижу, какой срез и кейс изменились.

- **Why P5:** без диагностики отчёт бесполезен для ревью.
- **Independent test:** отчёт содержит per-case ранги, метрики, срезы и дельты;
  CI-job падает только на hard-условиях.
- **Acceptance scenarios:**
  - Given регрессия, When гейт падает, Then сообщение содержит конфигурацию,
    срез, query id, baseline/current значения.
  - Given успешный прогон, When CI-job завершён, Then отчёт доступен
    артефактом.

## 5. Functional requirements

- **FR-1 (must) Корпус.** Основной offline-корпус живёт в
  `tests/fixtures/search-bench/corpus/`, содержит 60–85 замороженных `.md`
  документов (200–400 чанков — DELTA-2: 500+ недостижимы авторским корпусом
  при текущем чанкинге, масштаб отдаём внешним бенчам; внутренний корпус
  оптимизируем на глубину): синтетические ловушки и копии реальных
  документов проекта. Каждый документ описан в `corpus/MANIFEST.json`
  (`path`, `origin`, `license`, `frozen_at`, `blake3`); реальные копии после
  заморозки не редактируются. Legacy-parity использует отдельный
  `tests/fixtures/search-bench/legacy-corpus/` — побайтовую копию текущего
  корпуса `search-quality` (10 `.md` документов, без `baseline.json`), чтобы
  IDF расширенного корпуса не менял старые top-3.
- **FR-2 (must) Запросы.** `queries.json` содержит 120–200 кейсов основного
  корпуса: `id`, `query`, `class`, `k`, `expected[{path, lines?, grade}]`,
  `absent[{path, lines?, reason}]`, `source`, `notes`, `reviewed_by`.
  `grade`: 2 — прямой ответ, достаточный самостоятельно; 1 — поддерживающий/
  частичный (нужен ещё чанк); 0 — только для absent. `k = 10`.
  `reason` обязателен для каждого absent; `reviewed_by` — минимум 2 имени для
  `semantic`, `hard_negative` и любого кейса с absent, минимум 1 — для
  остальных. `notes` непустой для grade 1 и для semantic/hard_negative.
  Legacy-parity описывается отдельным `legacy.json` (12 кейсов, `k = 3`) и не
  входит в основной baseline-срез.
- **FR-3 (must) Срезы.** Обязательные классы основного корпуса: `ru_morph`,
  `en_morph`, `identifier`, `phrase`, `cjk`, `arabic`, `semantic`,
  `mixed_lang`, `hard_negative`, `multi_relevant`, `long_doc_boundary`.
  Минимум 8 кейсов на срез. Legacy-parity — отдельная проверка 12 текущих
  кейсов (FR-16), не участвующая в минимуме срезов.
- **FR-4 (must) Конфигурации.** Гейт и отчёт покрывают `bm25`,
  `bm25+rerank`, `rrf`, `rrf+rerank`. Dense-only допускается как
  диагностический режим вне гейта.
- **FR-5 (must) Offline embeddings.** Векторы документов и запросов хранятся
  в фикстурах с ключом `(model pin, blake3(text))`; model pin включает
  provider, model, dimensions и normalize. Offline-прогон не вызывает сеть.
- **FR-6 (must) Offline rerank.** Rerank-скоры хранятся для каждой пары
  `(model pin, query, ordered candidate list)`; offline-прогон воспроизводит
  ровно тот candidate set, на котором они записаны.
- **FR-7 (must) Fail-closed.** Отсутствующий вектор, rerank-набор, кейс,
  конфигурация или несовпадение хэшей — явная ошибка с указанием ключа.
  Пропуск (skip) запрещён.
- **FR-8 (must) Метрики.** Для каждого кейса: `hit@k` (есть expected с
  `grade >= 1` в top-k), `recall@k` (доля expected, попавших в top-k),
  `MRR@k` (обратный ранг первого релевантного), `nDCG@k` (graded),
  `citation_overlap@k` (доля позиций top-k, чьи `lines` пересекают ожидаемый
  диапазон), `forbidden_hit` (bool), `duplicate_path_ratio@k` (доля позиций
  top-k, чей `path` уже встречался выше в выдаче).
- **FR-9 (must) Цитаты.** `citation_overlap@k` считается по пересечению
  возвращённых `lines` с ожидаемым диапазоном для того же `path`; если
  диапазон не задан, достаточно совпадения пути. Разметка lines — 1-based,
  включительно.
- **FR-10 (must) Запрещённые документы.** Любой `absent` документ в top-k —
  hard fail кейса независимо от остальных метрик. `absent` — только явно
  adjudicated ловушки с непустым `reason`; отсутствие документа в `expected`
  само по себе не делает его `absent`. При заданных `lines` запрет
  применяется к пересечению диапазона.
- **FR-11 (must) Срезы и дельты.** Отчёт агрегирует метрики по срезу
  (макро-среднее) и сравнивает с baseline: per-case win/loss/tie,
  per-slice delta, paired bootstrap CI с фиксированным seed.
- **FR-12 (must) Детерминизм и версии.** Baseline и фикстуры содержат
  `fixture_version`, `corpus_hash`, `legacy_corpus_hash`, `queries_hash` и
  `model_pins`; несовпадение — fail. Одинаковый вход даёт идентичную
  quality-часть отчёта (ранги и метрики) на всех поддерживаемых ОС; latency и
  timestamp зависят от хоста и сравниваются только с baseline своего
  runner-класса.
- **FR-13 (must) Baseline.** Baseline хранит per-config/per-case метрики и
  ранги, per-slice агрегаты, early cutoffs 3/10, outcome matrix,
  legacy-parity, discrimination, локальные p50/p95, версию harness и дату
  генерации. BM25-скоры не сравниваются побитово.
- **FR-14 (must) Гейт.** Hard fail, если: (а) `forbidden_hit`; (б) baseline
  `hit = true`, current `hit = false`; (в) per-case `nDCG` упал больше допуска
  (0.02); (г) `hit_rate` любого среза упал; (д) `nDCG` среза упал больше
  допуска; (е) missing fixture/hash mismatch; (ж) конфигурация отсутствует.
  Улучшения не требуют обновления baseline, но и не обновляют его
  автоматически.
- **FR-15 (must) Регенерация baseline.** Только явная ignore-команда; запись
  baseline допустима лишь при зелёном parity и осознанном решении, зафиксированном
  в `progress.md`. Повторный прогон после записи должен быть детерминированным.
- **FR-16 (must) Legacy parity.** Отдельный прогон 12 текущих кейсов на
  `legacy-corpus/` (точная копия старого корпуса) с теми же
  ожидаемыми/запрещёнными путями и `k = 3`; их `hit`/`absent_hit` должны
  совпасть с зафиксированной копией старого
  `search-quality/baseline.json` (`legacy-baseline.json`) до вывода старого
  harness из гейта. `nDCG` сверяется как диагностика, не как условие parity.
- **FR-17 (must) Live-прогон.** Отдельный `#[ignore]`-тест за feature
  `hybrid-remote` и `OPENROUTER_API_KEY` прогоняет те же четыре конфигурации
  на реальном провайдере. Live-результаты не сравниваются побитово с replay;
  отчёт содержит предупреждения о дельтах.
- **FR-18 (must) Стоимость и latency live.** Отчёт фиксирует p50/p95 полного
  поиска, embedding/rerank latency (где измеримо), usage (`tokens`,
  `search_units`) и оценку стоимости. Cost cap обязателен; превышение —
  остановка с ошибкой.
- **FR-19 (must) Record mode.** Явная ignore-команда записывает векторы,
  rerank-скоры и baseline для текущего model pin. Запись атомарна: при ошибке
  не остаётся частично обновлённых фикстур.
- **FR-20 (must) Отчёт.** Offline и live пишут JSON-отчёт с версией, хэшами,
  model pin, конфигурациями, per-case рангами/метриками, early cutoffs 3/10,
  outcome matrix, срезами, дельтами, legacy-parity, discrimination,
  локальными latency и live Pareto. Отчёт не содержит секретов.
- **FR-21 (must) Offline-режим.** Гейт работает в дефолтной сборке (без
  `hybrid-remote`), без `reqwest` в дереве зависимостей, без сети и без
  `OPENROUTER_API_KEY`; replay реализует существующие трейты `Embedder` и
  `Reranker`.
- **FR-22 (must) Локальная latency.** Offline-отчёт измеряет p50/p95 вызова
  `search_docs_with` без сети (индексный поиск, dense scan, replay lookup) и
  время построения индекса. Latency-гейт применяется на Linux CI: p95 любой
  конфигурации не выше `baseline_p95 * 1.5 + 5 ms` и не выше 500 ms в debug.
  На Windows/macOS latency измеряется и публикуется, но не гейтится.
- **FR-23 (must) CI.** Offline-гейт — отдельная PR-джоба (Linux); quality-часть
  прогоняется на Windows/macOS в матрице или отдельной джобе. Live — отдельный
  workflow по расписанию/вручную, не блокирует PR.
- **FR-24 (must) Валидация фикстур.** Проверяются: уникальные `id`, наличие
  всех `expected`/`absent` путей в соответствующем корпусе, корректные `lines`
  в границах файла, `grade ∈ {1,2}`, `k ∈ 3..=20`, непустой `reason` у
  `absent`, `reviewed_by`/`notes` по FR-2, lexical control по FR-27, минимум
  8 кейсов на срез, точная копия legacy-корпуса, хэши корпусов/запросов,
  размер фикстур.
- **FR-25 (must) Model pin.** Один embedding pin на прогон: provider, model,
  dimensions, normalize; и один rerank pin: provider, model. Смена любого
  компонента требует нового набора векторов/скоров; смешивание embedding- или
  rerank-пинов в одном baseline запрещено.
- **FR-26 (must) Adjudication и рубрикатор.** Каждый `absent` — явно
  отadjudicated ловушка: `reason` объясняет, почему документ нерелевантен
  (например, near-duplicate с изменённым идентификатором), а не «его нет в
  expected». Кейсы `semantic`, `hard_negative` и любые кейсы с `absent`
  проходят второе ревью (человек или свежий reviewer-агент), результат —
  `reviewed_by` из ≥2 имён; разногласия и решения фиксируются в `progress.md`.
  `reviewed_by` — это и есть фиксация adjudication, `reason` — evidence;
  недостаточный список — ошибка валидации. Grade 2 = прямой ответ, grade 1 =
  supporting/partial; `notes` объясняет grade 1.
- **FR-27 (must) Лексический контроль semantic.** Для каждого semantic-кейса
  считается Jaccard между токенами запроса и expected-спана (`lines`).
  Токенизация — та же, что видит индекс (`IdentifierTokenizer`,
  `TOKENIZER_VERSION`): нормализация, стемминг, CJK-биграммы; один
  канонический токен на позицию. `lexical_jaccard = |Q∩E| / |Q∪E|`;
  берётся максимум по expected; значение ≤0.5. `expected[].lines` обязателен,
  иначе кейс невалиден. Превышение — ошибка валидации: кейс переписывается
  или переносится в `lexical`/`mixed_lang`. Semantic — EN/RU только проза;
  идентификаторы идут в `identifier`, CJK/AR — в свои срезы. Смена
  `TOKENIZER_VERSION` инвалидирует baseline (проверяется
  `lexical_tokenizer_version`). Порог 0.5 — стартовая калибровка (см.
  DELTA-1).
- **FR-28 (must) Дискриминирующая способность.** Baseline валидирует сам
  бенчмарк: кейс saturated, если все 4 конфигурации дают `hit@k`; saturated
  доля основного корпуса ≤70%; у каждого обязательного среза есть ≥1
  non-saturated кейс; в `semantic` ≥3 кейса, где конфигурации расходятся
  (не все четыре `hit`-значения равны; направление не важно); во всём корпусе
  ≥1 кейс, где `bm25` и `bm25+rerank` расходятся (в любую сторону). Это
  сравнение production-конфигураций (включая их fetch-окно); чистая
  атрибуция dense/rerank — should, не must. Невыполнение — baseline не
  принимается: `benchmark_is_discriminating` fail; запись baseline для
  диагностики разрешена, коммит — только после зелёного. Добавляются более
  сложные кейсы, пороги не понижаются молча. 70%/3/1 — стартовые значения
  (см. DELTA-1).
- **FR-29 (should) Pooling review.** При каждом расширении корпуса после
  появления runner'а (T7+) выполняется pooling review: объединение top-k
  четырёх конфигураций; новые релевантные документы идут в `expected`,
  нерелевантные — в `absent` с `reason`, спорные — в ledger. Для T3–T6 —
  ручная cross-check; одного прогона после T11 недостаточно.
- **FR-30 (should) Multi-cutoff 3/10.** Отчёт считает `hit@3`, `recall@3`,
  `nDCG@3` из того же ranked list, что и `k=10`; значения — диагностика и
  дельты, не hard gate (корпус мал для 20/50/200).
- **FR-31 (should) Outcome matrix.** Отчёт классифицирует каждый кейс по
  hit-паттерну конфигураций (all-hit, all-miss, mixed) и агрегирует по срезам;
  это атрибуция «кто выиграл/проиграл», не отдельный гейт.
- **FR-32 (should) Pareto quality-vs-cost.** Live-отчёт считает quality
  (nDCG) и cost/latency по каждой конфигурации и отмечает Pareto-фронт;
  offline-гейт не зависит от стоимости.

## 6. Non-functional requirements

- **NFR-1 (performance).** Offline-гейт (индексация + 4 конфигурации на
  120–200 запросах) — не более 30 с в debug на CI-хосте, жёсткий предел 60 с.
- **NFR-2 (storage).** Фикстуры бенчмарка (корпус + векторы + скоры +
  baseline) — не более 12 MiB; векторы — raw little-endian f32.
- **NFR-3 (reliability).** Повторный прогон даёт идентичную quality-часть
  JSON (timestamp и latency исключены); missing fixture — fail, не skip;
  запись атомарна.
- **NFR-4 (maintainability).** Новый кейс добавляется правкой `queries.json` и
  корпуса без изменения кода harness. Record mode восстанавливает фикстуры
  одной командой.
- **NFR-5 (security).** Offline-режим не делает сетевых вызовов; фикстуры не
  содержат ключей, токенов и приватных данных; пути фикстур не выходят за
  `tests/fixtures/search-bench/`.
- **NFR-6 (portability).** Linux x86_64, Windows x64, macOS aarch64; raw f32
  little-endian; одинаковые метрики на всех ОС.
- **NFR-7 (cost).** Live-прогон ограничен cost cap (`$2` по умолчанию,
  переопределяется env); превышение останавливает прогон.
- **NFR-8 (observability).** Отчёт содержит причину каждого hard fail:
  конфигурация, срез, query id, ожидаемое/текущее значение.
- **NFR-9 (scalability).** При удвоении корпуса offline-прогон остаётся в
  пределах 60 с; фикстуры растут линейно, без квадратичных структур.
- **NFR-10 (traceability).** Каждый FR покрыт минимум одним тестом; каждый
  срез и метрика имеют владельца в задачах.

## 7. Data model

- **CorpusDoc** — `path` (относительный, `/`-разделитель), `origin`
  (`synthetic` | `frozen-real`), `license`, `frozen_at`, `blake3`, содержимое
  `.md`. Жизненный цикл: добавлен → заморожен → меняется только через явный
  record/review.
- **QueryCase** — `id` (стабильный slug), `query`, `class`, `k`,
  `expected[]`, `absent[]`, `source`, `notes`, `reviewed_by[]`.
- **ExpectedHit** — `path`, `lines?: [start, end]`, `grade` (1|2).
- **AbsentHit** — `path`, `lines?: [start, end]`, `reason` (непустой).
- **VectorFixture** — `model_pin`, `text_hash`, `dim`, raw `f32[]`; хранится
  отдельно для документов и запросов, но в одном пространстве ключей.
- **RerankFixture** — `model_pin`, `query_hash`, `candidate_hashes[]`,
  `scores[]`, `usage`.
- **Baseline** — `bench_version`, `fixture_version`, `corpus_hash`,
  `legacy_corpus_hash`, `queries_hash`, `model_pins`, `generated_at`,
  `configs{...}`, `cases[]`, `slices{}`, `latency{}`, `legacy_parity{}`,
  `discrimination{}`.
- **DiscriminationReport** — `saturated_total`, `saturated_ratio`,
  `non_saturated_by_slice{}`, `semantic_disagreements`,
  `rerank_disagreements`, `semantic_max_lexical_jaccard`,
  `lexical_tokenizer_version`.
- **EarlyMetrics** — `hit@3`, `recall@3`, `nDCG@3` из того же ranked list.
- **OutcomeMatrix** — счётчики hit-паттернов (all-hit/all-miss/mixed) по
  срезам.
- **ParetoReport** — per-config quality/cost/latency и frontier (live).
- **Report** — baseline-подобная структура + `current` результаты, дельты,
  предупреждения, стоимость/latency live, `legacy_parity`,
  `discrimination`, `early{}`, `outcome_matrix{}`, `pareto{}`.

Жизненный цикл: корпус/запросы → запись векторов и rerank-скоров → offline
baseline → PR-гейт; live-прогон читает те же запросы, но реальные стадии и
пишет отдельный отчёт.

## 8. Interfaces

**Layout**

```text
tests/fixtures/search-bench/
  corpus/                      # frozen .md docs (main)
  corpus/MANIFEST.json         # origin/license/blake3
  legacy-corpus/               # byte-identical copy of the old 10-doc corpus
  queries.json                 # 120–200 main cases
  legacy.json                  # 12 parity cases (k=3)
  legacy-baseline.json         # frozen copy of search-quality/baseline.json
  vectors/<model-pin>/         # manifest.json + raw f32 files
  rerank/<model-pin>.json      # recorded scores + usage
  baseline.json                # per-config gate baseline + legacy parity
tests/search_bench.rs          # offline gate + regenerate_baseline
tests/search_bench_live.rs     # live_measurement + record_fixtures (ignored)
tests/support/mod.rs           # module root
tests/support/bench.rs         # shared harness (loader, replay, metrics)
```

**Commands**

```sh
cargo test --locked --test search_bench
cargo test --locked --test search_bench -- --ignored regenerate_baseline
cargo test --locked --features hybrid-remote --test search_bench_live -- --ignored live_measurement
cargo test --locked --features hybrid-remote --test search_bench_live -- --ignored record_fixtures
```

**Report shape (minimum)**

```json
{
  "bench_version": 1,
  "fixture_version": 1,
  "corpus_hash": "blake3:...",
  "queries_hash": "blake3:...",
  "model_pins": ["openrouter/nvidia/nemotron-3-embed-1b:free@d2048@l2"],
  "configs": {
    "bm25": {
      "cases": [{"id": "ru-morph-01", "ranks": ["..."], "metrics": {}}],
      "slices": {"ru_morph": {"hit_rate": 1.0, "ndcg": 1.0}},
      "latency": {"p50_ms": 0.8, "p95_ms": 2.1}
    }
  },
  "deltas": {},
  "legacy_parity": {},
  "discrimination": {"saturated_ratio": 0.4, "semantic_disagreements": 5, "rerank_disagreements": 2},
  "warnings": []
}
```

**CLI/MCP:** публичные контракты не меняются. Бенчмарк — test-only артефакт.

## 9. Measurable success criteria

- **SC-1.** Offline-гейт проходит на unchanged baseline; два прогона дают
  идентичную quality-часть JSON (ранги/метрики; latency и timestamp
  исключены).
- **SC-2.** Каждый FR имеет автоматизированный тест или проверку валидации;
  отчёт traceability приложен к tasks.
- **SC-3.** Инъекция регрессии (например, `title` boost 2.0 → 1.0, отключение
  rerank, поломка RRF) переводит гейт в fail и указывает срез/кейс.
- **SC-4.** Все четыре конфигурации присутствуют в каждом прогоне; каждый
  обязательный срез основного корпуса содержит ≥8 кейсов.
- **SC-5.** Hard-negative ловушки дают `forbidden_hit = 0` на baseline; любая
  ловушка в top-k — fail.
- **SC-6.** Legacy-parity на `legacy-corpus/` воспроизводит 12 старых кейсов:
  `hit`/`absent_hit` совпадают с `search-quality/baseline.json`.
- **SC-7.** Offline-прогон укладывается в 30 с debug; фикстуры ≤12 MiB.
- **SC-8.** Live-отчёт содержит все четыре конфигурации, p50/p95, usage и
  стоимость; превышение cost cap останавливает прогон.
- **SC-9.** Record mode на неизменных входах не меняет baseline; после смены
  model pin требует явной регенерации и не оставляет старых фикстур.
- **SC-10.** Offline-гейт работает в дефолтной сборке без сети, без
  `reqwest` и без `OPENROUTER_API_KEY`.
- **SC-11.** Baseline проходит discrimination-проверку FR-28: saturated
  доля ≤70%, у каждого среза есть non-saturated кейс; в semantic ≥3 кейса с
  расхождением конфигураций (любое направление); в корпусе ≥1 расхождение
  `bm25` и `bm25+rerank` (любое направление).
- **SC-12.** Каждый `absent` имеет непустой `reason`; critical-кейсы имеют
  ≥2 reviewer'ов; grade 1 имеет `notes`; `lexical_jaccard` semantic-кейсов
  ≤0.5.
- **SC-13.** Отчёт содержит early cutoffs 3/10, outcome matrix и live Pareto;
  отсутствие — fail, значения не блокируют PR (should).

## 10. Assumptions & challenges

**A1. Видимый корпус можно переобучить.** Все кейсы лежат в репозитории, поэтому
подгонка под них возможна. *Решение:* provenance в `source`/`notes`, ротация
кейсов при каждой итерации, обязательный hard-negative слой, ревью новых кейсов
как отдельного изменения; скрытый набор не вводим (не воспроизводим в CI).

**A2. Replay candidate set хрупок.** Любое изменение ранжирования меняет
candidate set, и записанные rerank-скоры перестают покрывать пары.
*Решение:* fail-closed + `record_fixtures`; это осознанная цена
воспроизводимости. Для оценки изменения нужно явно перезаписать скоры (live)
или прогнать live-режим.

**A3. Path+lines разметка дороже path-level.** *Решение:* замороженный корпус
не дрейфует; lines проверяются валидатором; новые реальные копии добавляются
только с `MANIFEST.json`.

**A4. Offline latency не равна provider latency.** Replay убирает сеть.
*Решение:* offline измеряет наш код (BM25, dense scan, RRF, replay overhead) и
гейтит относительный рост; реальная latency — только live-отчёт (FR-18).

**A5. Debug-latency шумная.** *Решение:* относительный порог `1.5x + 5 ms`,
абсолютный потолок 500 ms, baseline калибруется на CI-хосте; при шуме
используется p95, а не единичный замер.

**A6. Стоимость live-прогона может выйти за бюджет.** *Решение:* cost cap и
остановка; отчёт фиксирует, на каком кейсе/конфигурации остановились.

**A7. Старый и новый гейты конфликтуют.** *Решение:* legacy-parity на
отдельном корпусе (FR-16); старый harness выводится отдельным коммитом после
зелёного parity.

**A8. `absent` — сильное утверждение.** Несуждённый документ может быть
релевантным (BEIR Hole@10 до 31.8%). *Решение:* `absent` только для явных
ловушек с `reason` и вторым ревью (FR-26); отсутствие в `expected` не
является доказательством нерелевантности.

**A9. Пороги — начальная калибровка.** `SEMANTIC_MAX_LEXICAL_JACCARD=0.5`,
`SATURATION_MAX_RATIO=0.70`, `SEMANTIC_DISAGREEMENT_MIN=3`,
`RERANK_DISAGREEMENT_MIN=1` — стартовые значения. *Решение:* сначала
добавлять более сложные кейсы; изменение порога — отдельный ruling с
rationale в ledger (DELTA-1).

**Red-team vectors**

- **Unstated:** поведение «нет ответа» не определено; в этой спеке его нет —
  `absent` проверяет запреты, а не threshold. Явно вынесено в non-goals.
- **Incentive:** бенчмарк виден и может быть подогнан; снижаем ротацией,
  hard negatives и ревью, но не устраняем полностью — это принятый риск.
- **Adversarial:** фикстуры из недоверенных документов могут содержать
  секреты; `MANIFEST.json` фиксирует origin/license, добавление только через
  ревью; пути валидируются внутри fixture root.
- **Scale-break:** 10× корпус упрётся в 60 с и размер; NFR-9 требует линейного
  роста, ANN — вне спеки. Первый cliff — dense replay O(N·Q), он измеряется.
- **Sunset:** провайдер/модель исчезнет; pinned replay остаётся рабочим,
  live-режим отключается, baseline можно перегенерировать с новым pin.

## 11. Out of scope

- ANN/приближённый dense-поиск.
- Локальные embedding-модели и HF-кэш.
- Per-request `ranking`/веса/`k` и query expansion.
- Relevance cutoff/no-answer порог.
- LLM-разметка и LLM-as-judge.
- UI/дашборды, production-телеметрия.
- Изменение MCP/CLI-контрактов и формата ответа `search_docs`.
- Тюнинг BM25 `k1/b` и замена tantivy.

## 12. Open questions

- **OQ-1. Каденс live-прогона.** Предлагаемый дефолт: вручную перед релизом +
  еженедельно по расписанию; PR не блокирует.
- **OQ-2. Cost cap.** Предлагаемый дефолт: `$2` на прогон,
  `DOCSBASE_BENCH_COST_CAP_USD` для переопределения.
- **OQ-3. Судьба старого harness.** Предлагаемый дефолт: оставить
  `tests/search_quality.rs` как быстрый smoke до parity, затем удалить
  отдельным PR вместе с его baseline.
- **OQ-4. Формат векторов.** Предлагаемый дефолт: raw little-endian f32 +
  JSON-манифест (без новых крейтов); JSON-массивы отклонены из-за размера.

## DELTA-1 (2026-10-19) — ужесточение методологии

**ADDED**

- FR-26: adjudication `absent` + рубрикатор grade 2/1 + второе ревью
  critical-кейсов; `reviewed_by` — запись adjudication, `reason` — evidence.
- FR-27: `lexical_jaccard` для semantic-среза: expected-span, токенизация
  индекса, порог 0.5.
- FR-28: saturation/disagreement проверка baseline (≤70% saturated,
  non-saturated по срезам, direction-agnostic расхождения конфигураций).
- FR-29…FR-32 (should): pooling review при каждом расширении корпуса,
  multi-cutoff 3/10, outcome matrix, live Pareto.
- SC-11, SC-12, SC-13.
- A8 (absent — сильное утверждение), A9 (калибровка стартовых значений).

**MODIFIED**

- FR-2: `absent.reason`, `reviewed_by`, `notes`, рубрикатор грейдов.
- FR-10: `absent` — только adjudicated ловушки; «нет в expected» не аргумент.
- FR-13/FR-20: baseline/отчёт содержат `discrimination`, early cutoffs,
  outcome matrix, live Pareto.
- FR-24: валидация reason/reviewed_by/notes/lexical Jaccard.
- Data model: `AbsentHit.reason`, `QueryCase.reviewed_by`,
  `DiscriminationReport`, `EarlyMetrics`, `OutcomeMatrix`, `ParetoReport`.

**Стартовые значения (калибруются после первого baseline; изменение —
отдельный ruling в `progress.md`, не тихий фикс):**

| Параметр | Старт | Как калибруется |
|---|---|---|
| `SEMANTIC_MAX_LEXICAL_JACCARD` | 0.5 | по expected-span и первым semantic-кейсам; rationale в ledger |
| `SATURATION_MAX_RATIO` | 0.70 | по доле all-hit на первом baseline; rationale в ledger |
| `SEMANTIC_DISAGREEMENT_MIN` | 3 | по фактическому числу расхождений semantic; rationale в ledger |
| `RERANK_DISAGREEMENT_MIN` | 1 | по фактическому числу расхождений bm25/rerank; rationale в ledger |

Это стартовые значения, не константы: калибровка меняет только порог, а
не направление требования (saturation ≤, disagreement ≥).

## DELTA-2 (2026-10-19) — баунд чанков под реальность

**MODIFIED**

- FR-1: корпус 60–80 документов, чанки 500–1000 → **200–400**. Замер T6 RED:
  62 дока дают 132 чанка при `MAX_CHUNK_CHARS=1500` + секционировании по
  заголовкам (≈2.1 чанка/док). 500+ достижимы только флудом коротких секций,
  что ломает смысл гейта (глубина, а не объём).
- Масштаб как сигнал отдаём внешним бенчам (`external-track.md`, primary);
  внутренний корпус оптимизируем на глубину: ловушки, границы чанков,
  multi-relevant кластеры.
- Нижняя граница 200 держит fetch-окна (`RERANK_WINDOW=50`, top-10 × 4
  конфига) в режиме реальной селекции, а не полного перебора.

## DELTA-2 amendment (2026-10-20) — границы под T12
- FR-1: документов 60–80 → **60–85** (T12 добавил 3 дока с неправильной
  морфологией для en_morph; чанки остались в 200–400).
- FR-2: кейсов 120–150 → **120–180** (T12 discrimination требует ~40+
  сложных кейсов; время прогона прежнее — доли секунды на replay,
  запас NFR-1 не тронут).
- Correction (2026-10-21): 120–180 → **120–200**. Фактический T12-корпус —
  193 кейса, что вышло за amendment-границу; единый набор зафиксирован
  как 60–85 документов / 120–200 кейсов (FR-1/FR-2, §8, tasks T6, design
  §5.8/§13).

## DELTA-3 (2026-10-20) — контракт движка: присутствие и порядок, не чистота

Решение заказчика по итогам T11/T12-измерений: 20/21 forbidden-кейсов
показывают верный ответ на 1-м месте и ловушку на 2–8-м. Движок отвечает
правильно; абсолютный запрет ловушек в топе требует от движка работы
читающего агента и недостижим любым лексическим движком на малом корпусе.

**MODIFIED**

- FR-10: `absent` в top-k — hard fail, только если ловушка занимает
  **ранг 1** (или верного вообще нет в топе — старое правило hit).
  Ловушка на 2–10 при верном первом — зелёный кейс; лучший ранг каждой
  ловушки пишется в отчёт как диагностика (`trap warnings`). Топ-1,
  не покрытый ни expected, ни absent, — warning (кандидат в pooling
  по FR-29), не провал. Реализация диагностики (2026-10-21):
  `Report.trap_diagnostics` (наполняется `attach_trap_diagnostics`, входит
  в `quality_json`), артефакт `target/search-bench/gate-report.json`
  в offline-гейте и поле `trap_diagnostics` в `live-report.json` —
  движение ловушек видно и на зелёных прогонах.
- Философия контракта (обязательно к прочтению): движок не гарантирует
  отсутствие ошибок в выдаче — для этого нужен понимающий задачу слой
  (gateway LLM). Контракт движка — присутствие (hit/recall) и порядок
  (верное выше ловушек; градиент nDCG без регресса). Чистота топа —
  зона ответственности читающего агента, не движка. Гейт защищает
  видимое пользователю качество (неправильный топ-ответ, пропавший
  верный, просевший nDCG), а не внутреннюю чистоту.
- FR-14(a) читается через новый FR-10. Остальные пункты FR-14 без изменений.
- Политика гейта: вердикт — required `linux` check (гейт-тест падает
  громко внутри него, причина в отчёте); мерж при красном гейте — только явным override ревьюера,
  зафиксированным в PR (как и сам мерж — действие человека). Benchmark
  отвечает «стало хуже или лучше», а не сертифицирует совершенство.
- Исправленная известная ловушка требует перезаписи baseline (`regenerate`,
  FR-15): вылеченный кейс проходит как fixed, но known-запись остаётся до
  регенерации — иначе повторная поломка совпадёт со stale-записью и пройдёт
  молча.
**ADDED**

- FR-33 (should) Дефолтные модели — бесплатные маршруты OpenRouter
  (`nvidia/nemotron-3-embed-1b:free` @2048, rerank
  `nvidia/llama-nemotron-rerank-vl-1b-v2:free`): пилот T11 показал паритет
  или лучше платных на пробах (Rrf 18 vs 15 хитов / 0.594 vs 0.512 nDCG
  на 20 запросах; реранк top-1 6/6). Платные маршруты — только по
  необходимости (явный `models use`). Условие: пользователь предупреждён,
  что free-маршруты могут использовать промпты для обучения и меняются
  каждый месяц; rate limits (20/мин) учитываются pacing'ом записи.
