# Search Benchmark — Research Notes

Консолидация внешних источников и подходов, на которые опирается спека
`docs/specs/search-benchmark/` (requirements / design / tasks, ревизия 1,
2026-10-19). Собрано из ревью спеки и двух раундов исследования (2026-10-19).
Каждый раздел заканчивается выводом вида «что взято в спеку».

---

## 1. IR-бенчмарки: откуда брать корпуса и запросы

### 1.1 BEIR — гетерогенный zero-shot бенчмарк

BEIR (Thakur et al., 2021) собрал 18+ публичных датасетов в единую схему и
прогнал через них 10 архитектур (lexical / sparse / dense / late-interaction /
rerank). Ключевые уроки для нас:

- **BM25 — жестокий бейзлайн.** Dense-ретриверы (DPR, ANCE) систематически
  проигрывают BM25 на доменах со сдвигом (BioASQ, Touché). Вывод BEIR: «нет
  бесплатных обедов; in-domain качество не предсказывает zero-shot».
  Наше наблюдение (live: nDCG 1.000 → 0.926 с rerank поверх RRF) — тот же
  эффект, а не аномалия.
- **Hole-анализ (TREC-COVID).** Доля несуждённых документов в топ-10
  достигала 31.8% у TAS-B (DPR 30.6%, BM25 6.4%); ручная доразметка дыр
  подняла dense-метрики.
  Урок: **несуждённое ≠ нерелевантное**. Прямая угроза нашему FR-10 (hard fail
  за `absent` в top-k): absent-документ может быть релевантным, но
  незамеченным при разметке — тогда гейт карает правильное поведение.
  Следствие для спеки (на момент исследования; закрыто DELTA-1 → FR-26):
  absent-списки требуют adjudication, а не записи «от фонаря».
- **GenQ/docT5query как система, а не ground truth.** BEIR использует
  сгенерированные запросы как retrieval-метод (40 запросов на документ,
  Ансерини BM25), а не как эталон. Синтетика — инструмент, не источник
  истины.
- Формат BEIR/MTEB (corpus / queries / qrels) — де-факто стандарт схем;
  наша схема (corpus / queries.json / baseline) с ним совместима по духу.

Refs: [BEIR paper](https://arxiv.org/abs/2104.08663),
[BEIR v4 html](https://arxiv.org/html/2104.08663).

### 1.2 CoIR — бенчмарк code retrieval

CoIR (Li et al., ACL 2025): 10 курированных кодовых датасетов, 8 задач,
7 доменов, ручная чистка + 2 новых датасета, схема BEIR/MTEB, pip-пакет.
Уроки:

- Ручная курация существующих данных работает для идентификаторов/фраз
  (наши срезы `identifier`, `phrase`).
- Авторы честно фиксируют ограничение «только английский». Нам стоит так же
  честно зафиксировать: RU/AR/CJK-срезы покрывают морфологию, а не семантику
  этих языков.
- Цель CoIR — различать системы там, где все «уже хорошо» (SOTA проваливается
  на кодовом поиске). Наш мотив тот же: уйти от насыщенного 10/10.

Refs: [CoIR paper](https://arxiv.org/abs/2407.02883),
[ACL Anthology](https://aclanthology.org/2025.acl-long.1072/).

### 1.3 LoTTE — органические запросы (ближайший аналог наших «реальных»)

LoTTE (ColBERTv2, Khattab & Zaharia): 12 доменных сетов, два типа запросов —
`search` (GooAQ, веб-поиск) и `forum` (вопросы StackExchange с принятым
ответом). Forum-ветка — это **реальные информационные потребности с готовым
ответом**, без аннотаторов. Наш прямой аналог: GitHub issues и StackOverflow
вопросы по проектам + маппинг «вопрос → раздел документации». Распределение
запросов настоящее; ручной остаётся только маппинг, а не выдумывание запроса.

Refs: [ColBERTv2 / LoTTE](https://arxiv.org/abs/2112.01488),
[LoTTE data (ColBERT repo)](https://github.com/stanford-futuredata/ColBERT/blob/main/LoTTE.md).

### 1.4 NeuCLIR / NeuCLIRBench — многоязычный поиск и пулы

TREC NeuCLIR (2022–2024): CLIR/MLIR для китайского, персидского, русского;
топики пишут асессоры, пулы глубиной до 50. NeuCLIRBench (2025) объединяет
эти раунды: 250,128 суждений на ~350 запросов (~700 на запрос), заявленная
«сильная дискриминирующая способность». Релевантно нашим RU/CJK-срезам
(персидский — бонус; арабского в NeuCLIR нет). Кросс-скрипт — известное
узкое место; BM25 без перевода почти не находит.

Урок: дискриминирующую силу дают **объём суждений + пулинг**, а не число
кейсов само по себе. Наш дешёвый аналог пулинга — объединение топ-k всех
4 конфигураций как множество для разметки/отбора (см. §5).

Refs: [NeuCLIR 2022 overview](https://arxiv.org/abs/2304.12367),
[NeuCLIRBench paper](https://arxiv.org/abs/2511.14758),
[NeuCLIRBench data](https://huggingface.co/datasets/neuclir/bench).

### 1.5 MS MARCO — масштаб ценой тонких суждений

Реальные запросы Bing, пассажи из веба — но **одно суждение на запрос**.
Итог: всё несуждённое считается нерелевантным (та же hole-проблема, §1.1).
Для нас: режим «один expected на кейс» допустим только как регрессионный;
заявлять «измерение качества» на нём нельзя.

---

## 2. Синтетические запросы: протоколы с фильтрами

### 2.1 InPars / InPars-v2 / Promptagator / InPars+

- **InPars:** LLM генерирует запрос по документу (few-shot из MS MARCO);
  без фильтрации — падение качества (минус 4 MRR@10 при обучении на всех
  100k пар).
- **InPars-v2:** открытый GPT-J + фильтрация reranker-скором (monoT5-3B),
  топ-10k из 100k; negatives — случайный документ из BM25 топ-1000.
- **Promptagator:** dataset-specific промпты, 8 запросов на документ,
  sampling с температурой; round-trip фильтр — запрос принимается, только
  если исходный документ возвращается в топ-K.
- **InPars+ (2025):** ревизия воспроизводимости; часть пайплайнов невоспроизводима
  (проприетарные модели, потерянные артефакты). Урок: фиксировать генератор,
  промпт, seed и отбрасывать few-shot примеры из метрик.

**Статус в спеке:** частично (DELTA-1). Текущий план (T5) пишет
semantic-кейсы вручную, поэтому InPars-пайплайн не нужен; вместо него —
FR-27 (`lexical_jaccard ≤ 0.5`, expected-span, токенизация индекса) и второе ревью. Если появится
генерация запросов, протокол обязателен (двухступенчатый фильтр, фиксация
генератора/промпта/seed, дисквалификация few-shot примеров). Без фильтра
синтетика вредит, а не помогает.

Refs: [InPars-v2](https://arxiv.org/abs/2301.01820),
[InPars Toolkit](https://arxiv.org/abs/2307.04601),
[InPars+](https://arxiv.org/abs/2508.13930).

### 2.2 IR2 — предупреждение о лексических утечках

Синтетические запросы делят с документом поверхностные лексические признаки;
это раздувает лексические системы и душит измерение семантики. Контрмеры:
регуляризация документа/запроса/инструкции. Для нас — операционализация:
**у каждого semantic-кейса измерять лексическое пересечение с expected и
требовать ниже порога** (иначе «без общих терминов» из T5 — декларация).
Оценка стоимости ручной разметки из той же работы: ~$5k/датасет (≈5 мин на
запрос при $15/ч) — ориентир цены ручного пути.

Refs: [IR2 (LREC 2024)](https://aclanthology.org/2024.lrec-main.810.pdf).

---

## 3. Релевантность: разметка, шкалы, судьи

### 3.1 TREC-методология (канон)

- Рабочее определение: документ релевантен, если его использовали бы в отчёте
  по теме (покрывает «кусок релевантен, даже если мал»).
- **Pooling:** судятся топ-n всех участвующих систем; остальное считается
  нерелевантным (осознанное приближение).
- **Шкалы грейдов — стандарт** (0–3 в DL-треках; раньше бинарные).
- **Релевантность субъективна:** различается между судьями и у одного судьи
  во времени (Voorhees). Асессоры — нанятые аналитики с track-specific
  training; темы и суждения делают одни и те же люди.
- Наш вывод: грейды 1–2 в спеке — норма, но нужен TREC-style рубрикатор
  (что отличает 2 от 1, с примерами), иначе два размечающих разойдутся.

Refs: [TREC relevance judgements](https://trec.nist.gov/data/reljudge_eng.html),
[TREC how-to (термины: pooling, qrels, assessor)](http://trec.nist.gov/howto.html),
[TREC 2021 overview (graded relevance)](https://trec.nist.gov/pubs/trec30/papers/Overview-2021.pdf).

### 3.2 LLM как размечающий: UMBRELA и контр-позиция

- **UMBRELA (NIST, TREC 2024 RAG Track; ICTIR 2025):** LLM-разметка с точными
  рубриками, три LLM-режима — фильтр пулов, post-edit (ассессор видит метки
  LLM), полностью авто. Шкала 0–3 (non-relevant / related / highly /
  perfectly).
- **Известный биас (Alaofi et al. 2024):** ложные positives LLM коррелируют с
  наличием терминов запроса в пассаже — ровно то, что убьёт наши hard
  negatives. Наш вывод: LLM — только pre-filter и подсветка спорных пар;
  frozen benchmark полностью-авто разметке не доверяем.
- **Контр-позиция** («Don’t Use LLMs to Make Relevance Judgments», 2025):
  оценка и поиск — одна задача; LLM-судья страдает теми же провалами, что и
  LLM-поиск. Учитываем как аргумент за детерминированный гейт без судьи.

Refs: [UMBRELA study (NIST)](https://tsapps.nist.gov/publication/get_pub.cfm?pub_id=959054),
[Don’t Use LLMs… (PMC)](https://pmc.ncbi.nlm.nih.gov/articles/PMC11984504/).

### 3.3 RAGAS — reference-free оценка

Метрики faithfulness / answer relevance / context relevance. В WikiEval два
аннотатора совпали в ~95% (faithfulness, context relevance) и ~90% (answer
relevance); это **inter-annotator agreement**, а не согласие метрики с
людьми. Accuracy самой RAGAS против людей: 0.95 / 0.78 / 0.70 (Table 4).
Для нас: RAGAS — это уровень **ответа**, наш гейт — уровень
**ранжирования**; faithfulness/context relevance — кандидат на будущую
pipeline-задачу («агент отвечает по чанкам»), а не замена
retrieval-метрикам.

Refs: [RAGAS paper](https://arxiv.org/abs/2309.15217),
[RAGAS docs](https://docs.ragas.io/).

---

## 4. Как меряют память/RAG смежные инструменты

### 4.1 Mem0 — открытый harness `memory-benchmarks`

Пайплайн Ingest → Search → Evaluate на LoCoMo / LongMemEval / BEAM, Apache-2.0.
Забираем три практики:

- `--predict-only`: остановка после retrieval (изоляция качества поиска) —
    наш offline replay архитектурно, у них флагом. Подтверждает разделение
    «поиск отдельно, ответ отдельно» как индустриальный стандарт.
- `--top-k-cutoffs 10,20,50,200`: оценка на нескольких глубинах. Наш `k=10`
    (+ legacy 3) стоит расширить — дёшево, повышает различимость.
- Публикация variance судьи (±1 пункт) и web UI per-question разбора.
    Наш детерминированный гейт variance не имеет — преимущество, проговаривать.

Refs: [memory-benchmarks](https://github.com/mem0ai/memory-benchmarks),
[Memory Evaluation (docs)](https://docs.mem0.ai/core-concepts/memory-evaluation).

### 4.2 Cognee — мульти-метрики и их несогласие + матрица evaluability

- Evals на HotPotQA-подмножестве (24 вопроса): EM, F1, DeepEval Correctness,
  Human-like — метрики часто спорят; LLM-судья плавает между прогонами.
  Вывод: пачка метрик вместо одной (у нас: hit/recall/MRR/nDCG/citation/
  forbidden) — правильно; single-metric врёт.
- Сравнительный разбор («How to Evaluate AI Memory»): Mem0 — открытый
  harness; **Zep — отделяет completeness контекста от accuracy ответа**
  (провал retrieval vs провал генерации видны раздельно); Letta — тесты
  использования памяти агентом; LangMem — сьюта нет, тесты определяет
  приложение. Наш per-config гейт уже атрибутирует вину
  (упал `rrf+rerank` при зелёных `bm25/rrf` = вина rerank-стадии) — держать
  и проговаривать.

Refs: [How to Evaluate AI Memory (Cognee)](https://www.cognee.ai/how-to-evaluate-ai-memory-tools),
[cognee evals README](https://github.com/syntax-syndicate/cognee-ai-memory/blob/main/evals/README.md).

### 4.3 Mem0 vs Zep — спор о методологии как урок

Публичный спор о подсчёте adversarial-вопросов LoCoMo: 84% → оспаривание →
признание ошибки → ревизия 75.14%. Урок: **negative-control и adversarial
кейсы требуют airtight-определений ДО замера** (как считаются, что значит
провал в каждой комбинации конфигураций). Наши FR-10/FR-14 вычитать именно
под этим углом.

Refs: [Zep vs Mem0 (методологические заметки)](https://mem0.ai/blog/zep-vs-mem0-which-ai-memory-layer-should-you-choose).

### 4.4 «How to Read Memory Benchmarks» (Mem0) — методология чтения

- Retrieval-метрики (recall@k) отделяют провал поиска от провала ответа.
- Глубина — компромисс: больше контекста не всегда лучше (их multi-session
  упал 93.2% → 88.0% при росте промпта). Наш аналог: размер fetch-окна не
  бесплатен — аргумент за измерение, а не «окно побольше».
- Saturation: маленькие бенчмарки влезают в контекст целиком — тогда кейс за
  memory layer держится на cost/latency, а не accuracy. Ровно наша ситуация
  10/10.
- Pareto frontier accuracy-vs-cost и split bills (стоимость агента отдельно,
  стоимость памяти отдельно). Наш live-отчёт (FR-18) собирает usage/стоимость;
  следующий шаг — фронтир per-config как ответ «стоит ли hybrid своих денег».

Refs: [How to Read Memory Benchmarks](https://mem0.ai/library/agent-memory/how-to-read-memory-benchmarks).

---

## 5. RepoBench / CrossCodeEval — ближайшие родственники по задаче

- **RepoBench (ICLR 2024):** R (retrieval) / C (completion) / P (pipeline).
  Retrieval меряется отдельно от end-task; pipeline-задачи у нас нет.
  Главный трюк — **behavioral ground truth**: релевантность определяется тем,
  что downstream-задача реально использовала (какой сниппет понадобился),
  а не судьёй. Наш аналог: логи сессий — какие чанки агент реально читал /
  цитировал. Снимает проблему субъективности разметки для большей части
  корпуса.
- **CrossCodeEval (NeurIPS 2023):** данные в разрезе
  `{baseline, retrieval, retrieval w/ ref} × {bm25, UniXCoder, Ada}` —
  предподсчитанный retrieval под каждый ретривер. То же, что наш replay
  (фикстуры под конфигурацию). Дизайн валидирован независимо.

Refs: [RepoBench paper](https://arxiv.org/abs/2306.03091),
[RepoBench repo](https://github.com/Leolty/repobench),
[CrossCodeEval repo](https://github.com/amazon-science/cceval).

---

## 6. Что из этого взято в спеку search-benchmark (карта решений)

| Внешний вывод | Статус в спеке |
|---|---|
| Разделение retrieval / answer (Mem0 `--predict-only`, Zep completeness vs accuracy, RepoBench R/C/P) | Взято: offline-гейт только про поиск через `search_docs_with`; pipeline-задача — горизонт, не скоуп |
| Behavioral ground truth (RepoBench-P, LoTTE-forum, логи сессий) | Частично: добавить блок источников expected (сессии → issues/SO → bug-driven → синтетика); сейчас только «авторство» (T3–T6) |
| Pooling для отбора/разметки (TREC) | Взято (should, DELTA-1): FR-29 — pooling review при каждом расширении корпуса; универсальный admission-фильтр отклонён в пользу saturation-квоты |
| Hole-анализ (BEIR TREC-COVID) | Взято (DELTA-1): `absent.reason` + второе ревью (FR-10/FR-26); отсутствие в expected — не доказательство нерелевантности |
| Фильтры синтетики (InPars-v2 top-K, Promptagator round-trip) | Взято (DELTA-1): FR-27 lexical Jaccard ≤0.5 (expected-span, токенизация индекса); InPars-пайплайн не нужен для ручных semantic-кейсов |
| Рубрикатор + adjudication (TREC; RAGAS inter-annotator 95/90, metric accuracy 0.95/0.78/0.70) | Взято (DELTA-1): рубрикатор grade 2/1, `reviewed_by` ≥2, agreement/разногласия в ledger (FR-26) |
| LLM-судья только как pre-filter (UMBRELA биас к лексике) | Принято: frozen benchmark не доверяем полностью-авто разметке |
| Multi-cutoff (Mem0 top-k cutoffs) | Взято (should, DELTA-1): FR-30 — early cutoffs 3/10; 20/50/200 отклонены при 67 документах |
| Pareto accuracy-vs-cost (Mem0 reading guide) | Взято (should, DELTA-1): FR-32 — per-config quality/cost/latency и frontier в live |
| Airtight negative-control (урок LoCoMo-спора) | Частично (DELTA-1): reason/reviewer + direction-agnostic disagreements (FR-26/FR-28) + outcome matrix (FR-31) |
| BM25 как сильный бейзлайн (BEIR, NeuCLIRBench) | Принято как позиция: сначала измерить, потом включать hybrid; live 1.000→0.926 — ожидаемо, не аномалия |
| No-replacement тезис (CoIR: SOTA проваливается; saturate-наборы) | Мотивация G2: уйти от насыщенного 10/10; взято в FR-28 (≤70% saturated, disagreements) |

Открытые пункты после DELTA-1: поведенческие источники (сессии/issues) и
pipeline-горизонт (RAGAS-style). Pooling, multi-cutoff 3/10, outcome matrix и
live Pareto закрыты в FR-29…FR-32 (should); adjudication, рубрикатор и
lexical control — в FR-26…FR-28.
