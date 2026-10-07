# Search quality research — русская морфология и гранулярность чанков

> Дата: 2026-10-06. Повод: догфудинг v0.1.0-alpha.3 на собственном репозитории
> (27 docs / 285 chunks).
>
> Наблюдения, которые исследуются:
> 1. Идентификаторы и EN-фразы ищутся точно, RU-формы — нет: «замены» и
>    «заменой» дают 0 хитов, «замена» — score 1; «каталогов» — 1, «каталоги» — 4.
> 2. Omnibus-чанк `progress.md:261–333` (73 строки, ~3345 слов, один заголовок
>    без подзаголовков) матчится почти на каждый запрос со score 2–6.

## 0. TL;DR

- **Стемминг для русского — стандартная опция индустрии с 2000-х.** Без него
  потери retrieval-качества измеряются десятками процентов; с ним — statistically
  significant выигрыш почти на всех коллекциях.
- **Гранулярность: индексировать пассажи, а не документы — классическая практика**
  (Callan 1994; MS MARCO; DPR). Длинный чанк даёт слабые «мусорные» совпадения
  из-за length normalization BM25 — это не баг BM25, а следствие единицы индексации.
- **Обе проблемы решаются независимо от фазы 2** и стоят дёшево относительно
  неё; hybrid-пайплайн (фаза 2) дополняет их, но не отменяет: ранкер остаётся
  лучшим на exact-матчах и на части русских датасетов.
- **Для docsbase:** (а) RU/EN стемминг мульти-токенами (Snowball-вариант tantivy
  умеет из коробки; готового Dolamic light для русского нет),
  (б) cap размера чанка + рекурсивный структурный сплит + `read_neighbors`,
  (в) фаза 2 — hybrid-пайплайн (ранкер + embedding + reranker) за флагами
  per-project/per-request.

## 1. Морфология и токенизация

### 1.1 Что принято в индустрии

- Lucene/Elasticsearch/Solr штатно предлагают algorithmic stemmers
  (Porter/Snowball) для каждого языка, включая Russian (`russian`,
  `light_russian`) — как opt-in языковые анализаторы (дефолтный анализатор
  Elastic — `standard` без стемминга). Elastic явно рекомендует algorithmic
  stemmers вместо dictionary (hunspell): меньше памяти, нет зависимости
  от полноты словаря.
  Важно: один и тот же анализатор применяется на индексации и на запросе.
  [Elastic: Stemming](https://www.elastic.co/docs/manage-data/data-store/text-analysis/stemming),
  [Stemmer token filter (языки, incl. russian/light_russian)](https://www.elastic.co/docs/reference/text-analysis/analysis-stemmer-tokenfilter)
- `stem_exclusion` (keyword_marker) — стандартный механизм «не стеммить вот эти
  слова»: им принято защищать идентификаторы, аббревиатуры и термины.
- Лемматизация (pymorphy2, MyStem, Rubic2) даёт более точные нормальные формы,
  но тяжелее и нестабильнее на OOV; в поиске её берут, когда стемминга мало
  (например, для QA/морфологического анализа). Rubic2 (Slavic NLP 2025) —
  SOTA neural-лемматизация, 82.9–99.7% accuracy, но это transformer-инференс,
  не для локального лёгкого рантайма.
  [Rubic2 paper](https://aclanthology.org/2025.bsnlp-1.18.pdf)

### 1.2 Данные: насколько стемминг реально помогает

- **Dolamic & Savoy (JASIST 2009), каноническое исследование русского IR:**
  сравнили no stemming / light / aggressive / Snowball / 4-gram на 6 retrieval-
  моделях. Все варианты со стеммингом существенно лучше: relative MAP +83%
  (4-gram) … +90.3% (light); light и aggressive статистически не отличаются.
  Snowball — приемлемо, но light-стеммер (57 правил на словоизменение) чуть
  лучше на ряде тем.
  [PDF: Indexing and searching strategies for the Russian language](http://members.unine.ch/jacques.savoy/Papers/JASISTru.pdf)
- Чешский (тот же автор, Information Processing & Management 2009): +45% MAP
  от стемминга, различия всегда значимы; выигрыш для славянских языков выше,
  чем для английского. [PDF](http://members.unine.ch/jacques.savoy/Papers/IPMCzech.pdf)
- Современный взгляд (RusBEIR, arXiv:2504.12879; ранняя версия — Dialogue 2025):
  - топ по качеству — группа **mE5-large и оба BGE-варианта**, суммарно
    +15.9 п.п. над BM25 в среднем по бенчмарку (дословно: «mE5-large and both
    BGE variants … surpassing BM25 by 15.9 pp»; 17 датасетов; метрику сверять
    с таблицами paper);
  - **но BM25 выигрывает 4 датасета из 17: rus-NFCorpus, rus-SciFact,
    wikifacts-articles и wikifacts-para, включая wikifacts-articles
    (+13 п.п. над BGE-M3, +27 п.п. над mE5-large)**, т.е. exact-поиск
    не выкидывается;
  - лучшая комбинация — **BGE-M3 + BGE reranker**, затем mE5-large + reranker.
  [arXiv:2504.12879](https://arxiv.org/abs/2504.12879),
  [ранний PDF (Dialogue 2025)](https://dialogue-conf.org/wp-content/uploads/2025/04/KovalevGetal.046.pdf)
  Важно: BM25-бейзлайн в RusBEIR шёл **с отключённым языковым анализатором
  (без стемминга)** (в paper §4.1: «with language analyzer disabled to avoid stemming»)
  — выигрыш BM25 на длинных документах объясняется в первую очередь truncation
  нейросетей (лимит входа mE5 — 512 токенов против полных статей), а не
  морфологией. Это аргумент за hybrid, а не против стемминга.
- MTEB multilingual (срез на дату написания; при цитировании фиксировать версию
  бенчмарка): BGE-M3 (avg retrieval ~62) сильно опережает MiniLM-класс
  (~48) и LaBSE (~52); Russian — 60.8 у BGE-M3.
- Практический вывод: стемминг и эмбеддинги решают разные части проблемы:
  стемминг — морфологию (дёшево, exact-ветка), эмбеддинги — синонимы/перевод
  (дорого, dense-ветка). Индустрия держит обе.

### 1.3 Tantivy: что есть из коробки

- `tantivy::tokenizer::Language` содержит **Russian** (18 языков, включая
  English) — [docs.rs](https://docs.rs/tantivy/latest/tantivy/tokenizer/enum.Language.html).
- `Stemmer` — обычный `TokenFilter` в цепочке
  `TextAnalyzer::builder(...).filter(LowerCaser).filter(Stemmer::new(Language::Russian))`
  ([tantivy README](https://github.com/quickwit-oss/tantivy), [docs.rs tokenizer](https://docs.rs/tantivy/latest/tantivy/tokenizer)).
- Анализаторы регистрируются per-index (`index.tokenizers().register(name, analyzer)`),
  поле выбирает анализатор по имени в схеме — можно завести отдельные поля или
  отдельные анализаторы для RU и EN.
- Ограничение: цепочка фильтров не «ветвится» по языку токена — два Stemmer
  подряд технически собираются, но бессмысленны (второй стеммит уже
  стеммированную форму); для смешанного текста нужен либо per-language поля,
  либо кастомный фильтр, эмитящий мульти-токены (raw + ru-stem + en-stem)
  в одной позиции (`position_length=1` при общей позиции — Lucene-эквивалент
  posIncr=0/posLen=1; phrase-scorer tantivy 0.26 смотрит только соседство
  позиций, `position_length` во фразовом матчинге не участвует, `postings_writer`
  использует его лишь для end_position).
- Нюанс для нас: из коробки tantivy даёт **Snowball Russian**, а по литературе
  Dolamic light-стеммер чуть лучше Snowball — но готового light для русского
  нет: в `tantivy-stemmers` Dolamic (light/aggressive) есть **только для
  чешского**, для русского там тот же Snowball. Light-вариант придётся
  портировать/писать самим. Плюс наш `IdentifierTokenizer`
  (`src/index/tokenizer.rs`) — это кастомный `Tokenizer`, а не
  `SimpleTokenizer`, поэтому стеммер нельзя просто прицепить в `TextAnalyzer` —
  его надо встраивать в/поверх identifier-пайплайна с сохранением consistency
  индекс/запрос, полным reindex и bump схемы.

### 1.4 Смешанный корпус (наш случай)

- Детект языка per-chunk (whatlang/franc) ломается на смешанных чанках, а у нас
  в одном абзаце русская проза и английские термины (`blacklist системных
  каталогов`) — это типовой случай для двуязычных техдоков.
- Индустриальные варианты: per-language поля + language detection (Elasticsearch
  language analyzers), либо multi-token indexing. Для кодовых/технических
  корпусов принят и третий приём — **identifier splitting**: CamelCase/snake_case
  токенизация с сохранением исходного токена (де-факто стандарт code search:
  CamelCase splitter, Samurai, TIDIER).
  [ICPC 2011: Identifier Splitting](https://www.cs.wm.edu/~denys/pubs/ICPC2011-IdentifierSplitting.pdf)
- Рекомендация для нас: индексировать **и сырой, и стеммированный** токен
  (мульти-токен в одной позиции — так делает Lucene keyword_marker/synonym),
  стеммить только алфавитные токены длиной > 3, не трогая идентификаторы.
  Это даёт RU/EN-формы и не ломает exact-матч идентификаторов (сырой токен
  вида `MAX_FRAME_BYTES` остаётся в индексе рядом со стемами).

### 1.5 За пределами EN/RU: CJK, агглютинативные, арабо-персидские

- **CJK (zh/ja/ko) — нет пробелов:** либо словарная/морфологическая сегментация
  (jieba, Kuromoji/MeCab, lindera), либо **character n-grams** (биграммы: Lucene
  `CJKBigramFilter`, ES `cjk` analyzer). NTCIR: n-граммы сопоставимы со
  словарными, для китайского часто лучше (сегментация ошибается на OOV), для
  ja/ko — на уровне; биграммы — надёжный fallback без словарей. Точнее обоих —
  ICU word segmentation (UAX#29 + словари для CJK, Thai, Lao, Khmer, Burmese).
  [IBM: CJK linguistic processing](https://www.ibm.com/docs/en/i/7.4.0?topic=processing-chinese-japanese-korean),
  [Lucene CJK analyzer](https://lucenenet.apache.org/docs/4.8.0-beta00008/api/Lucene.Net.Analysis.Common/Lucene.Net.Analysis.Cjk.html),
  [OpenSearch ICU tokenizer](https://docs.opensearch.org/latest/analyzers/tokenizers/icu-tokenizer)
- **Корейский:** пробелы разделяют eojeol, но не слова внутри него; морфоанализ
  (lindera + ko-dic) даёт стеммы. **Тайский/лаосский/кхмерский/бирманский:**
  пробелов нет вовсе — только словарная сегментация (ICU, PyThaiNLP newmm,
  AttaCut), биграммы не работают. **Вьетнамский:** пробелы разделяют слоги, а не
  слова (слово = 1–4 слога) — тоже нужна word segmentation (VnCoreNLP).
  [ICU boundary analysis](https://github.com/unicode-org/icu/blob/main/docs/userguide/boundaryanalysis/index.md),
  [VnCoreNLP](https://github.com/dungnt085/vn-core-nlp)
- **Агглютинативные и сложные:** турецкий/финский/венгерский — Snowball-стеммеры
  есть (tantivy: Turkish), n-gram-анализ исторически особенно силён для fi/hu
  (CLEF 2002, +65–80% MAP); немецкие композиты требуют decompounding —
  лексические стеммеры с разбиением композитов заметно лучше алгоритмических
  (CLEF 2003, пример `Kruzifixstreit`). **Арабский:** корневая морфология +
  критичная нормализация (hamza→alef, teh marbuta→heh, удаление harakat/tatweel;
  light stemming по Ballesteros) — стандарт Lucene `ArabicAnalyzer`; Snowball
  Arabic встроен в tantivy. **Персидский/урду:** ZWNJ (half-space) — отдельный
  класс ошибок, нужны нормализаторы.
  [CLEF 2003: lexical vs algorithmic](http://clef.isti.cnr.it/2003/WN_web/19.pdf),
  [Lucene ArabicNormalizer](https://lucene.apache.org/core/9_11_1/analysis/common/org/apache/lucene/analysis/ar/ArabicNormalizer.html),
  [PerSpaCor: ZWNJ](https://aclanthology.org/2025.ranlp-1.40.pdf)
- **Tantivy-экосистема:** встроенный `Stemmer` — 18 языков (Arabic, Turkish,
  Greek…), CJK нет; CJK-крейты `tantivy-jieba`/`cang-jie` (zh),
  `lindera-tantivy` (ja IPADIC/UniDic/NEologd, ko ko-dic, zh CC-CEDICT), но у
  lindera словари **отключены по умолчанию** «to keep the binary size small»
  (конфликт с нашим 40-MiB static budget). Встроенный `NgramTokenizer` ставит
  position=0 (фразовые запросы для CJK не работают) → биграммы лучше эмитить
  своим токенайзером с корректными позициями.
  [lindera-tantivy](https://docs.rs/lindera-tantivy/latest/lindera_tantivy/),
  [tantivy-jieba](https://docs.rs/tantivy-jieba/latest/tantivy_jieba/),
  [tantivy NgramTokenizer](https://docs.rs/tantivy/latest/tantivy/tokenizer/struct.NgramTokenizer.html)
- **Эмбеддинги закрывают «хвост»:** BGE-M3 — 100+ языков, MIRACL avg 62.1;
  per-language retrieval (срез на дату написания, цифры ориентировочные —
  сверять с MIRACL/MMTEB лидербордами): zh 61.8, ja 58.2, ko 57.5, ar 56.8,
  vi 56.8, hi 55.2, th 52.1 (ниже европейских, но сильно выше LaBSE/MiniLM).
  MMTEB (ICLR 2025) —
  500+ задач на 250+ языках; есть JMTEB (28 датасетов), Korean/Chinese/Indic
  MTEB. Корейский retrieval: BM25 0.47 vs BGE-M3 0.71 nDCG, корейские
  дообученные (KURE-v1, bge-m3-ko) и reranker — ещё выше.
  [MMTEB](https://arxiv.org/abs/2502.13595),
  [MIRACL (TACL 2023)](https://aclanthology.org/2023.tacl-1.63),
  [Korean MTEB retrieval](https://github.com/BM-K/Korean-MTEB-Retrieval-Evaluators)

### 1.6 Unicode-нормализация и фолдинг (общий выигрыш для всех языков)

- Порядок: **NFC** (каноническая нормализация) → **NFKC + full case folding
  (`nfkc_cf`)** для поиска: width folding (полноширинные/полуширинные — критично
  для японского), совместимые формы, ß→ss, греческая сигма, арабские
  презентационные формы. Отдельно: арабо-специфичная нормализация, турецкий
  İ/I (обычный `lower()` неверен), ё→е (наш кейс), CJK width filter.
  [Lucene ICU (normalization / case folding)](https://lucene.apache.org/core/9_12_0/analysis/icu/index.html),
  [UAX #15](https://unicode.org/reports/tr15/tr15-58.html)
- Нормализация и токенизация применяются одинаково на индексации и на запросе;
  направление то же, что версионирование анализаторов в Lucene: версия
  обработки учитывается рядом с индексом и сверяется на запросе (наш
  `tokenizer_version` — в том же духе; точную механику сверять с Lucene ICU
  при реализации).
- Tantivy: есть только `LowerCaser`; нормализация — кастомный фильтр, либо
  крейт `unicode-normalization` (**открытый вопрос design §7** для v1.x).

## 2. Гранулярность чанков и «длинные чанки матчатся на всё»

### 2.1 BM25 и длина документа

- Стандарт: Lucene/ES BM25 `k1=1.2`, `b=0.75` (b — степень length
  normalization). [Lucene BM25Similarity](https://lucene.apache.org/core/9_9_1/core/org/apache/lucene/search/similarities/BM25Similarity.html)
- Длинные документы **перештрафовываются**: Lv & Zhai, SIGIR 2011 «When documents
  are very long, BM25 fails!» → BM25L; продолжение — CIKM 2011 lower-bounding TF
  normalization / BM25+. Это про случай «нужный термин есть, но документ
  огромный». Наш кейс — зеркальный: tf=1 в чанке на 3345 слов, length
  normalization давит score до 2–6, и такой чанк попадает в top-N почти любого
  запроса с общим токеном, но цитата бесполезна (73 строки).
  [BM25L, SIGIR'11 poster](https://doi.org/10.1145/2009916.2010070),
  [lower-bounding TF, CIKM'11 PDF](https://timan.cs.illinois.edu/czhai/pub/cikm11-bm25.pdf),
  [обзор вариантов BM25, ECIR 2020 (Kamphuis et al.)](https://pmc.ncbi.nlm.nih.gov/articles/PMC7148026)
- Оптимальный `b` зависит от коллекции и длины запросов: исследования дают
  оптимум от 0.125 до 0.75 (короткие запросы — меньший b). Для passage-level
  коллекций тюнят и `k1`: DPR использовал `b=0.4, k1=0.9`.
  [Cummins & O'Riordan](https://www.dcs.gla.ac.uk/~ronanc/papers/cumminsAICS09.pdf),
  [DPR paper](https://arxiv.org/pdf/2004.04906)
- Практика для RAG: держать чанки умеренными и тюнить `k1/b`; длинные чанки
  «никогда не ранжируются» — [разбор, блог 2026, non-peer-reviewed](https://dev.to/ji_ai/bm25-length-normalization-why-long-rag-chunks-never-rank-5d03).

### 2.2 Passage-level retrieval — классика, а не новинка

- Callan, SIGIR 1994: passage-level evidence улучшает ранжирование длинных
  структурированных документов; индексация пассажей как отдельных единиц —
  accepted practice.
  [Passage-Level Evidence in Document Retrieval](https://www.cs.cmu.edu/~callan/Papers/callan794.pdf)
- MS MARCO — passage-level датасет; DPR, ColBERT, SPLADE — все индексируют
  пассажи ~100–350 токенов. DAPR benchmark (2024): для длинных документов
  документ-уровневые сигналы надо добавлять сверху passage-retrieval, а не
  вместо него. [DAPR](https://arxiv.org/pdf/2305.13915v2)
- Это ровно наша модель: маленькие чанки + `read_neighbors` для контекста.

### 2.3 Стратегии чанкинга: что показали исследования 2024–2026

- **Semantic chunking не панацея.** NAACL 2025 findings «Is Semantic Chunking
  Worth the Computational Cost?»: на 5 датасетах fixed-size выиграл на 3,
  разница минимальна; качество эмбеддера важнее стратегии.
  [PDF](https://aclanthology.org/2025.findings-naacl.114.pdf)
- **Recursive (структурный) — дефолт для структурированных доков.** Сплит по
  `\n\n` → `\n` → предложениям; для Markdown это заголовки/абзацы/списки.
  Semantic chunking дорог (45 мин vs 12 сек на корпус) и на структурных
  документах проигрывает recursive.
  [Сравнение с цифрами](https://abhilashganji.com/research/rag-chunking-strategies.html),
  [обзор 2026](https://denser.ai/blog/rag-chunking-strategies)
- **Размер:** 64–128 токенов оптимален для фактовых вопросов, 512–1024 — для
  широкого контекста; разные эмбеддеры имеют разную чувствительность.
  [Rethinking Chunk Size (2025)](https://arxiv.org/abs/2505.21700)
- Systematic study 36 стратегий: лучшая — Paragraph Group Chunking
  (nDCG@5 ≈ 0.459), fixed-size baseline — nDCG@5 < 0.244.
  [arXiv 2603.06976](https://arxiv.org/html/2603.06976)
- Adaptive chunking (2026): split-then-merge recursive с целевым размером 600–
  1100 токенов — лучший баланс качества/компактности.
  [arXiv 2603.25333](https://arxiv.org/pdf/2603.25333)
- Форматный чанкинг для леджеров/списков: каждая запись-пункт = отдельный
  чанк — это частный случай recursive split по строкам списка.

### 2.4 Small-to-big / sentence window — устоявшийся паттерн

- «Search small, return large» реализован в основных фреймворках:
  LangChain ParentDocumentRetriever, LlamaIndex SentenceWindowNodeParser,
  Haystack SentenceWindowRetriever. Доказано: small-chunk retrieval точнее,
  а контекст добирается окном соседей/родителем.
  [Haystack SentenceWindowRetriever](https://github.com/deepset-ai/haystack/blob/main/haystack/components/retrievers/sentence_window_retriever.py),
  [LlamaIndex пример](https://github.com/machinelearningzuu/Advanced-RAG-Experiments/blob/main/02-small-to-big-retriever.ipynb)
- FUNNELRAG (NAACL 2025): coarse-to-fine progressive retrieval даёт +16.9% @1
  над flat-retrieval. [PDF](https://aclanthology.org/2025.findings-naacl.165.pdf)
- Наш `read_neighbors` — ровно этот паттерн; он оправдан, когда чанки маленькие.

### 2.5 Contextual retrieval и late chunking (embeddings-эра)

- **Anthropic Contextual Retrieval (2024):** перед эмбеддингом/BM25 к чанку
  добавляется 50–100 токенов LLM-контекста; top-20 failure rate −35%
  (contextual embeddings), −49% (contextual BM25 hybrid), −67% с reranker.
  [Anthropic](https://www.anthropic.com/news/contextual-retrieval)
- **Late chunking (Jina, EMNLP 2024):** документ кодируется целиком, чанк-
  эмбеддинги получаются mean pooling'ом по спанам — контекст «протекает» между
  чанками без LLM и без обучения; работает только с long-context моделями
  на mean pooling. [Paper](https://arxiv.org/abs/2409.04701),
  [блог](https://jina.ai/news/late-chunking-in-long-context-embedding-models)
- Сравнение (2025): Anthropic-contextual точнее late chunking, но ~120× дороже
  (1890 мс/док vs 15.8); на практике late chunking — «бесплатное» улучшение
  при наличии подходящей модели. [ConTEB paper](https://arxiv.org/html/2505.24782v1)

### 2.6 Что спорно / не принято как обязательное

- Semantic chunking: модно, но нет систематического выигрыша, оправдывающего
  стоимость (NAACL 2025); для структурных доков — нет.
- Contextual retrieval с LLM: даёт лучшие цифры, но требует внешнего LLM
  (напрягает local-first как рекомендацию) и дорог; это не «индустриальный
  дефолт», а продвинутая опция.
- BM25L/BM25+: доказаны для «очень длинных документов», но на стандартных
  коллекциях разница с BM25 незначима; менять similarity без замеров не принято.
- Reranker: устойчивый выигрыш в hybrid-пайплайнах (BGE-M3 рекомендует
  hybrid+rerank; Russian benchmark подтверждает), но это +модель +латентность.

## 3. Что ещё нашло code-ревью (наш код, tantivy 0.26.2)

- **OR-дефолт парсера:** `build_parser` (`src/index/tantivy_index.rs:403-418`)
  не вызывает `set_conjunction_by_default` — запрос разбирается как OR.
  Вклад в «omnibus матчится на всё» наряду с длиной чанка.
- **Нет `RemoveLongFilter`/стоп-слов:** длинный безпробельный токен режется
  только `MAX_TOKEN_LEN=65530` tantivy (дроп с warning,
  `postings_writer.rs:141-146`) — это гигиена терм-словаря, не корректность,
  но раздувание терм-словаря реально.
- **Буст пунктуации в `identifiers`:** `extract_identifiers`
  (`src/index/tantivy_index.rs:456-463`) пропускает слова с не-буквами —
  `«привет»,` уходит в поле с бустом ×2.5, а голое `привет` — нет.
- **`е/ё` не схлопываются** (только `to_lowercase`, без нормализации/стеммера).
- **Golden только EN:** `tests/search_golden.rs` — 4 кейса SC-1…SC-4, все
  английские; RU-морфология и фразы не покрыты.

## 4. Рекомендации для docsbase

### 4.1 v1.x — без новых крейтов (Snowball Russian встроенный; готового Dolamic light для русского нет)

1. **Cap размера чанка** (`max_chunk_tokens` ≈ 300–500; текущий
   `MAX_CHUNK_CHARS=4000` в `src/index/job.rs:18` — это **байты**, не chars:
   `split_ranges` считает `line.len()`, `text_len` — `chunk.text.len()`
   (`src/index/tantivy_index.rs:210-213`); для RU 4000 байт ≈ 2000 букв,
   т.е. cap сейчас вдвое строже для русского — нужна нормализация
   bytes→chars/tokens и замер для RU) с рекурсивным сплитом: заголовок →
   абзацы → строки списка; overlap ~10% (≈50 токенов) — сейчас overlap нет
   вообще, это миграция (`line_start/line_end`, цитаты, `read_neighbors`).
   Цитаты станут точными, omnibus-чанки исчезнут. При замере учесть уже
   существующий `LONG_CHUNK_PENALTY=0.5` (`src/index/tantivy_index.rs:24`) —
   cap и penalty взаимодействуют, сравнивать надо связкой.
2. **RU/EN стемминг мульти-токенами (Snowball из коробки):** `Stemmer(Language::Russian)` +
   `Stemmer(Language::English)` через кастомный фильтр, эмитящий raw + стемы
   в одной позиции (`position_length=1` при общей позиции, иначе фразовые
   запросы); стеммить только алфавитные токены > 3 символов. Идентификаторы
   не трогать. `position_length=1` при общей позиции нужен **всем**
   синонимичным вариантам — это фикс
   существующего бага фраз (сейчас `emit()` в `src/index/tokenizer.rs`
   раздаёт каждому варианту новую позицию: `defineStore → 0,1,2`), а не
   только будущим стемам; обновить тест `tests/tokenizer.rs:135-139`
   (`positions_are_contiguous`). Требует полного reindex + bump схемы,
   consistency индекс/запрос и явный `tokenizer_version` в rebuild-маркере:
   сейчас `open_or_create` сверяет только имена полей (`fields_from`) +
   `REBUILD_MARKER`, смена токенайзера молча оставит старый индекс.
   Прирост RU — по литературе десятки процентов MAP; для Snowball это нижняя
   граница (Dolamic light чуть лучше, но готовой реализации для русского нет —
   только собственный порт).
3. **Защита идентификаторов:** keyword_marker-подход (не стеммить токены с
   `_`, CamelCase, ALL_CAPS, цифрами) + сохранять исходный токен.
4. **Тюнинг BM25:** `k1/b` в tantivy 0.26 захардкожены (`K1=1.2, B=0.75`
   в исходниках tantivy 0.26.2, `src/query/bm25.rs:8-9`, публичного API нет) —
   тюнинг возможен только форком; наши рычаги в v1.x — бусты полей,
   `LONG_CHUNK_PENALTY` и чанкинг. Замерять их на golden-наборе.
5. **Golden-метрики:** расширить существующий `search_golden` (`tests/search_golden.rs`,
   сейчас SC-1…SC-4 + insta без scores, все кейсы EN) до recall@k/nDCG
   на наборе запросов из догфудинга: RU-формы (`замена/замены/заменой`,
   `каталоги/каталогов`), идентификаторы, фразы — включая фразовый кейс
   `"plan id"`, который сейчас ложно матчит `assessment_plan_id`
   (позиции `assessment(1), plan(2), id(3)`), иначе позиционный баг §4.1 п.2
   нечем ловить. Без этого любые изменения недоказуемы.

### 4.2 Фаза 2 (hybrid-пайплайн, за флагами)

Архитектура: **ранкер (BM25) + embedding + reranker** — «ранкер» здесь общее
слово для lexical first-stage (BM25); в литературе hybrid именно так и
описывают: sparse ranker + dense retriever + reranker. Каждая ступень
включается/выключается **per-project** (конфиг) и **per-request** (параметры
запроса): точный дешёвый поиск, семантика и точность ранжирования
комбинируются по ситуации, а не «всё или ничего».

- Embedding-старт — через **OpenRouter** (API-модели, без локального веса);
  local-first — рекомендация, а не строгое правило.
- 40 MiB — бюджет **самого ПО** (статический артефакт, NFR-2); скачивание
  тяжёлых моделей отдельно (например с HF в локальный кэш моделей) —
  допустимо. Локальный BGE-M3 (568M) — опция, не требование: через Rust
  `fastembed` (≥5.17) `Bgem3Embedding` даёт dense + sparse + ColBERT одним
  проходом, `try_new_from_path` — локальные ONNX-файлы (в т.ч. split
  `model.onnx_data`), т.е. модель можно предустановить и не ходить в сеть;
  ONNX-вес ≈2.27 GB. Fallback: компактный `mE5-base/small` или
  русско-специфичный `USER-BGE-M3` (359M, отмечен в RusBEIR); `mE5-large`
  fallback'ом по размеру не является (≈560M, тот же класс).
- RRF с `rank_constant=60` (дефолт Elasticsearch); веса fusion (например
  1:0.25) — стартовая точка для тюнинга на golden-наборе (атрибуция 4:1
  к Anthropic в литературе вторичная — Singh & Merola ссылаются на неё).
  BGE-reranker поверх top-N — тоже за флагом.
- Late chunking — дешёвое улучшение, только если эмбеддер long-context **и**
  mean pooling; совместимость с выбранной моделью (BGE-M3 и др.) проверить
  до рекомендации — иначе пункт убрать.
- Вектор-кэш T53 уже готов под `(model_id, chunk_hash)` — менять модель можно
  без переэмбеддинга; ключ не зависит от того, откуда векторы (OpenRouter
  или локальная модель).

### 4.3 Порядок

1. Golden-набор + метрики (иначе нечего сравнивать).
2. Chunk cap + recursive split (влияет и на BM25, и на будущие эмбеддинги).
3. Стемминг мульти-токенами (+ `tokenizer_version`, фикс позиций).
4. Тюнинг бустов/penalty по метрикам (k1/b захардкожены).
5. Фаза 2: hybrid-пайплайн (ранкер + embedding + reranker) за флагами.

### 4.4 Мультиязычный roadmap (CJK и другие)

- **v1.x без новых крейтов:** script-aware роутинг в нашем `IdentifierTokenizer`
  (Unicode script property): Cyrillic → RU-стем, Latin → EN-стем,
  **CJK → биграммы с корректными позициями** (fallback Lucene/ES, без словарей),
  Arabic → встроенный Snowball + нормализация; тайский/лаосский/кхмерский — вне
  v1 (нужны словари; закрываются эмбеддингами в фазе 2).
- **Unicode-нормализация:** NFC + width folding + фолдинг регистра/ё/İ/Arabic —
  решить: минимальный ручной фолдинг в токенайзере или крейт
  `unicode-normalization` (design §7, открыто). Версия нормализации — в тот же
  `tokenizer_version` маркер.
- **Golden:** добавить кейсы CJK (биграмма vs целое слово), width folding,
  Arabic harakat, тайский — иначе мультиязычные изменения недоказуемы.
- **Phase 2:** BGE-M3 (100+ языков) закрывает th/vi/km/lo и дополняет CJK;
  полная сегментация (lindera/jieba) — только опциональной фичей из-за размера
  словарей и 40-MiB бюджета.
- **Архитектура:** single index + мульти-токены; per-language поля/индексы — по
  паттерну Elastic (multi-fields, `multi_match best_fields`) — только если
  понадобится раздельная выдача по языкам.
- **Приоритет:** P2/P3 после RU-стемминга и чанкинга из §4.3 — мультиязычность
  за пределами EN/RU не блокирует v1.x.

## 5. Наивный план задач (S1–S7)

Порядок из §4.3; каждая задача — отдельный шаг/коммит в этой ветке. Без формальных
брифов — при старте задачи переносим её в `tasks.md` по обычной форме.

- **S1. Golden-набор и метрики.** recall@k/nDCG поверх `tests/search_golden.rs`;
  кейсы: RU-формы (`замена/замены/заменой`, `каталоги/каталогов`), идентификаторы,
  фразовые (`"plan id"` vs `assessment_plan_id`), заготовки CJK/Arabic. База для
  всех замеров — без неё ничего не сравниваем.
- **S2. Chunk cap + единицы.** bytes→chars/tokens (с замером для RU), рекурсивный
  сплит: заголовок → абзацы → строки списка, overlap ~10%; миграция
  `line_start/line_end`/цитат/`read_neighbors`; учёт связки с `LONG_CHUNK_PENALTY`.
- **S3. Токенайзер: позиции и версия.** `position_length=1` при общей позиции
  для всех синонимичных вариантов (фикс фразовых ложных матчей),
  `tokenizer_version` в rebuild-маркер
  (полный reindex), обновить `tests/tokenizer.rs`.
- **S4. RU/EN стемминг мульти-токенами.** Snowball (`Language::Russian/English`)
  поверх identifier-пайплайна; стемы не ломают exact-матч идентификаторов; замер
  на golden (S1).
- **S5. Тюнинг бустов/penalty.** По golden-метрикам; k1/b не трогаем (hardcoded в
  tantivy 0.26).
- **S6. Мультиязычный слой (P2/P3).** Script-aware роутинг: CJK → биграммы с
  корректными позициями, Arabic → нормализация + встроенный Snowball, Unicode-фолдинг
  (ё/width/İ); golden CJK/Arabic/Thai; решение по `unicode-normalization`.
- **S7. Hybrid (фаза 2, ADR-13).** Провайдер-абстракция (OpenRouter/local), RRF
  k=60, флаги per-project/per-request; **до сети — security-обзор**, до крейтов —
  бенчмарк.

Зависимости: S1 → (S2, S3, S4) → S5; S6 — после S4 (общий пайплайн токенайзера);
S7 — независим по коду, но замеры только через S1.

## Источники (сводно)

- Elastic: stemming, stemmer/snowball filters, language analyzers, RRF retriever:
  <https://www.elastic.co/docs/manage-data/data-store/text-analysis/stemming>,
  <https://www.elastic.co/docs/reference/text-analysis/analysis-stemmer-tokenfilter>,
  <https://www.elastic.co/docs/reference/elasticsearch/rest-apis/retrievers/rrf-retriever>
- Dolamic & Savoy: Russian JASIST 2009, Czech IPM 2009.
- RusBEIR (arXiv:2504.12879; Dialogue 2025 — ранняя версия), Rubic2 (Slavic NLP 2025).
- M3-Embedding/BGE-M3 (arXiv 2402.03216), MTEB multilingual, NVIDIA BGE-M3 card.
- Lv & Zhai SIGIR 2011 (BM25L), lower-bounding TF / BM25+ (CIKM 2011),
  Kamphuis et al. ECIR 2020 BM25 reproducibility, Cummins & O'Riordan (b tuning),
  DPR (arXiv 2004.04906).
- Callan SIGIR 1994 (passages), DAPR (arXiv 2305.13915).
- Chunking: NAACL 2025 «Is Semantic Chunking Worth the Cost?»,
  arXiv 2505.21700 (chunk size), arXiv 2603.06976 (36 strategies),
  arXiv 2603.25333 (adaptive).
- Small-to-big: Haystack/LlamaIndex/LangChain docs, FUNNELRAG NAACL 2025.
- Contextual retrieval: Anthropic 2024; Late chunking: arXiv 2409.04701 (Jina).
- Weaviate hybrid/fusion docs; OpenSearch RRF 2.19.
- Identifier splitting: ICPC 2011; code-aware tokenization practice.
- Tantivy: Language enum, Stemmer/TextAnalyzerBuilder, README features.
- Мультиязычность: NTCIR CJK experiments, Lucene CJK/ICU/Arabic analyzers,
  Elastic «Language pitfalls» + language-identification blog, ICU UAX#29.
- CJK-крейты: tantivy-jieba, cang-jie, lindera-tantivy (IPADIC/UniDic/ko-dic).
- Мультиязычные бенчмарки: MMTEB (arXiv 2502.13595), MIRACL (TACL 2023),
  JMTEB, Korean MTEB retrieval; BGE-M3 per-language (MIRACL avg).
