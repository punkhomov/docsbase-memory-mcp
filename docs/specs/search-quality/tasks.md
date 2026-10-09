# Search Quality Implementation Plan

**Goal:** поднять качество лексического поиска (RU/EN-морфология, точные цитаты,
корректные фразы, обязательный rebuild) и закрыть P3-мультиязычность — с
доказательством через golden-метрики.
**Architecture:** chunker (cap в chars, recursive split, overlap) → identifier-
токенайзер (normalize → script runs → варианты на одной позиции + стемы + CJK-
биграммы) → tantivy (BM25 + бусты + penalty); версия токенайзера в файле индекса
триггерит полный reindex. Детали: `docs/specs/search-quality/design.md`.
**Spec:** `docs/specs/search-quality/requirements.md`
**Global constraints:** Rust edition 2024, MSRV 1.88; `cargo fmt`; clippy
`--all-targets -- -D warnings` (+pedantic), `unsafe_code = warn`; без
`unwrap`/`expect`/`panic!` в `src/**`; без новых крейтов (`unicode-normalization` —
только через ADR); tantivy 0.26, один writer на индекс, индекс — производные данные
(rebuild из SQLite); офлайн по умолчанию; 40 MiB — бюджет самого ПО; схема
версионируется, смена анализатора = полный reindex.
**Review focus (failure modes → owning task):**
1. Стемминг ломает exact-матч идентификаторов (`MAX_FRAME_BYTES`) → SQ7.
2. Ложные/потерянные фразовые матчи после смены позиций → SQ6.
3. Молча устаревший индекс после смены анализатора → SQ5.
4. Cap/overlap ломает `lines`/цитаты → SQ4.
5. Perf-регресс от стемов/CJK-биграмм → SQ12.

Маппинг на research-план: SQ1–SQ2=S1, SQ3–SQ4=S2, SQ5–SQ6=S3, SQ7=S4, SQ8=S5,
SQ9–SQ11=S6; SQ12 — бюджеты.

---

## SQ1 — Golden fixture + harness метрик

**Goal:** фиксированный корпус и метрики recall@k/nDCG с воспроизводимым baseline
(FR-1, SC-1).
**Files:**
- Create: `tests/search_quality.rs` (harness + метрики)
- Create: `tests/fixtures/search-quality/*.md` (RU/EN/CJK/Arabic, ≥8 доков)
- Create: `tests/fixtures/search-quality/baseline.json` (отчёт метрик с порогами,
  не insta-снапшот: float-снапшот хрупок поперёк платформ)
- Test: `tests/search_quality.rs`
**Interfaces:**
- Produces: `fn recall_at_k(hits: &[Hit], expected: &[(path, rank)], k) -> f64`,
  `fn ndcg_at_10(...) -> f64` (тест-локальные), таблица запросов
  `&[Case { query, class, expected }]`.
**Steps:**
1. Создать фикстуры: RU-доки со словоформами (`замена/замены/заменой`,
   `каталоги/каталогов`), идентификаторы (`assessment_plan_id`,
   `MAX_FRAME_BYTES`), фразовый кейс (`plan id` рядом с `assessment_plan_id`),
   CJK/Arabic заготовки.
2. Написать harness: индексация фикстуры (`Bench`-паттерн из `search_golden.rs`),
   прогон кейсов, агрегаты.
3. Запустить и зафиксировать baseline-отчёт (детерминизм: два прогона в пределах
   толеранса; recall@k дискретен, nDCG с округлением).
4. Коммит.
**Acceptance:** FR-1; SC-1 — baseline воспроизводим.
**Verify:** `cargo test --locked --test search_quality` → `test result: ok`,
baseline-отчёт создан; повторный прогон в пределах толеранса.

## SQ2 — Target-классы и no-regression gate

**Goal:** приёмка изменений по классам запросов без регрессий (FR-2, FR-3, SC-4).
**Files:**
- Modify: `tests/search_quality.rs`
- Test: `tests/search_quality.rs`
**Interfaces:**
- Consumes: harness и baseline из SQ1.
- Produces: `fn assert_no_regression(current, baseline)` c правилами по классам
  (RU/EN/identifiers/phrases/CJK): hit@k presence + пороги метрик с толерансом
  (recall@k дискретен, nDCG — округление/толеранс); `Class` в таблице кейсов.
**Steps:**
1. Написать gate-тест: сравнение текущих метрик с baseline (per-class;
   выпадение релевантного дока из top-k или падение метрики за толеранс → fail).
2. Убедиться, что на текущем коде проходит; зафиксировать формулировку
   «hit@k по классам без выпадений; снапшоты цитат обновляются только delta»
   комментарием.
3. Коммит.
**Acceptance:** FR-2/FR-3; SC-4.
**Verify:** `cargo test --locked --test search_quality` → ok; искусственное
загрубление baseline локально → fail (проверить вручную, не коммитить).

## SQ3 — Cap в chars и единицы измерения

**Goal:** лимит чанка и `text_len` в Unicode-символах, без перекоса RU×2 (FR-5).
**Files:**
- Modify: `src/index/job.rs` (`MAX_CHUNK_CHARS` → семантика chars, `:18`)
- Modify: `src/index/chunk.rs` (`split_ranges` считает chars, `:187-215`)
- Modify: `src/index/tantivy_index.rs` (`text_len` = chars, `:210-213`; порог
  penalty `:339-343` → `MAX_CHUNK_CHARS + CHUNK_OVERLAP`)
- Test: `tests/chunker.rs`, `tests/tantivy_index.rs`
**Interfaces:**
- Produces: `MAX_CHUNK_CHARS` (chars), `Chunk.text_len` в chars.
**Steps:**
1. Тест: RU-документ из 1000 символов (2000 байт) не превышает cap при 1500 chars.
2. Тест падает на текущем байтовом поведении.
3. Заменить `len()` на `chars().count()` в сплите/`text_len`; обновить константу;
   порог penalty → `MAX_CHUNK_CHARS + CHUNK_OVERLAP` (design §4.4).
4. Тесты зелёные; коммит.
**Acceptance:** FR-5; SC-5 (частично).
**Verify:** `cargo test --locked --test chunker --test tantivy_index` → ok.

## SQ4 — Рекурсивный сплит, overlap, lines

**Goal:** секции без подзаголовков режутся по абзацам→строкам→hard cut, с overlap
и корректными `line_start/line_end` (FR-4, FR-6, SC-5).
**Files:**
- Modify: `src/index/chunk.rs`
- Test: `tests/chunker.rs`
- Modify (delta снапшотов цитат): `tests/snapshots/search_golden__*.snap`
- Test: `tests/search_golden.rs`
**Interfaces:**
- Produces: `CHUNK_OVERLAP` (≈10%), гарантия
  `text.chars().count() <= MAX_CHUNK_CHARS + CHUNK_OVERLAP` для прозы
  (атомарные fence/table — исключение, SC-5 delta/SQ15), срез по границе
  токена (аварийный посимвольный — только для безпробельного блоба).
**Steps:**
1. Тест: секция 3000 слов без подзаголовков → все чанки ≤ cap+overlap; у каждого
   `line_start/line_end` указывают на исходные строки (пересечение с текстом).
2. Тест: overlap — следующий чанк содержит хвост предыдущего (≥1 общая строка/фраза).
3. Тест: длинная строка с идентификаторами режется по границе токена
   (идентификатор не рвётся); безпробельный блоб — аварийный посимвольный срез.
4. Реализовать рекурсивный сплит; прогонять.
5. `search_golden`: обновить **только** снапшоты цитат (delta);
   релевантные документы остаются в top-k (FR-3); зафиксировать в сообщении коммита.
6. Коммит.
**Acceptance:** FR-4/FR-6; SC-5; hit@k по классам без падений (FR-3).
**Verify:** `cargo test --locked --test chunker --test search_golden` → ok.

## SQ5 — TOKENIZER_VERSION и rebuild

**Goal:** смена анализатора/нормализации не оставляет молча устаревший индекс
(FR-8, SC-6). Идёт раньше смены позиций (SQ6): механизм версии должен существовать
до первого изменения пайплайна.
**Files:**
- Modify: `src/index/tokenizer.rs` (`pub const TOKENIZER_VERSION: u32 = 2;`)
- Modify: `src/index/tantivy_index.rs` (файл версии в каталоге индекса;
  `open_or_create` `:92-136`, `ReadIndex::open` `:257-280`)
- Test: `tests/index_job.rs`, `tests/tantivy_index.rs`
**Interfaces:**
- Consumes: `TOKENIZER_VERSION`.
- Produces: `const TOKENIZER_VERSION_FILE: &str = "docsbase.tokenizer_version"`.
**Steps:**
1. Тест: собрать индекс, подменить файл версии на `1` (и отдельно — удалить файл,
   как у старого индекса), открыть → `was_recreated()` true и полный reindex из
   SQLite.
2. Тест: `ReadIndex::open` при mismatch → `Error::Project` с инструкцией.
3. Реализовать: `TOKENIZER_VERSION = 2` (1 = legacy), запись файла в `create_fresh`
   до создания индекса, сверка в `open_or_create`/`ReadIndex`.
4. Тесты зелёные; коммит.
**Acceptance:** FR-8; SC-6.
**Verify:** `cargo test --locked --test index_job --test tantivy_index` → ok.

## SQ6 — Позиции: одна позиция на сегмент + лимит токена

**Goal:** все варианты одного сегмента — одна позиция; фразы без ложных матчей и
без потерь; токены > 40 символов не индексируются (FR-7, FR-16, SC-3).
**Files:**
- Modify: `src/index/tokenizer.rs` (`emit`/`tokenize`, `:73-155`)
- Test: `tests/tokenizer.rs`
- Test: `tests/tantivy_index.rs` (фразовый интеграционный кейс)
**Interfaces:**
- Consumes: механизм версии SQ5 (изменение позиций — смена пайплайна: bump
  `TOKENIZER_VERSION` до 3 в этой же задаче).
- Produces: инвариант «позиции строго возрастают по сегментам, варианты внутри —
  равны»; `MAX_TOKEN_CHARS = 40`.
**Steps:**
1. Тест: `defineStore` → все варианты (`definestore`, `define`, `store`) с одной
   позицией; `assessment_plan_id` → одна позиция.
2. Тест (интеграция): фраза `"plan id"` не матчит документ с `assessment_plan_id`;
   настоящая фраза `plan id` находится.
3. Тест: токен длиной 64 символа не эмитится.
4. Переписать `emit`: позиция инкрементируется на сегмент, не на вариант; лимит
   `MAX_TOKEN_CHARS`; bump версии до 3.
5. Тесты зелёные; коммит.
**Acceptance:** FR-7/FR-16; SC-3.
**Verify:** `cargo test --locked --test tokenizer --test tantivy_index` → ok.

## SQ7 — RU/EN стемминг мульти-токенами

**Goal:** словоформы находят друг друга, идентификаторы не стеммятся (FR-9, FR-10,
SC-2).
**Files:**
- Modify: `src/index/tokenizer.rs` (стем-анализаторы `ru`/`en` в структуре,
  эмиссия стемов в позиции сегмента)
- Test: `tests/tokenizer.rs`
- Test: `tests/search_quality.rs` (target RU-кейсы flip → pass)
**Interfaces:**
- Consumes: `TOKENIZER_VERSION` (механизм SQ5; bump до 4 при изменении набора
  вариантов).
- Produces: стем-вариант для standalone-слов (алфавит, >3, не ALL_CAPS, без
  `_`/цифр, не часть camel).
**Steps:**
1. Тест: `замена`/`замены`/`заменой` дают одинаковый стем-вариант.
2. Тест: `MAX_FRAME_BYTES`, `assessment_plan_id`, `defineStore`-части НЕ стеммятся.
3. Тест: SC-1…SC-4 golden остаются true (no-regression).
4. Реализовать; bump версии; тесты зелёные; коммит.
**Acceptance:** FR-9/FR-10; SC-2; SC-4.
**Verify:** `cargo test --locked --test tokenizer --test search_quality --test search_golden`
→ ok.

## SQ8 — Тюнинг бустов/penalty

**Goal:** значения бустов полей и `LONG_CHUNK_PENALTY` выбраны по golden и
защищены guard-тестом (FR-11).
**Files:**
- Modify: `src/index/tantivy_index.rs` (`build_parser` `:403-418`, penalty `:24`)
- Test: `tests/search_quality.rs` (guard)
**Interfaces:**
- Consumes: baseline/классы SQ1–SQ2.
- Produces: зафиксированные значения + guard-тест «не хуже baseline по классам».
**Steps:**
1. Прогнать 2–3 варианта конфигурации на golden, записать цифры (в PR-описание).
2. Выбрать вариант; внести константы.
3. Guard-тест: выбранный конфиг не хуже baseline по всем классам.
4. Коммит.
**Acceptance:** FR-11; assumption #7 (выигрыш по классам, не точечный).
**Verify:** `cargo test --locked --test search_quality` → ok.

## SQ9 — textnorm: ё/width/case + script_of

**Goal:** базовая Unicode-нормализация и определение скрипта без крейтов (FR-14).
**Files:**
- Create: `src/index/textnorm.rs`
- Modify: `src/index/mod.rs` (модуль)
- Modify: `src/index/tokenizer.rs` (вызов `normalize` до токенизации)
- Test: `tests/tokenizer.rs`
**Interfaces:**
- Produces: `normalize(&str) -> Cow<str>` (ё/Ё→е, fullwidth ASCII→ASCII,
  идемпотентна), `Script { Latin, Cyrillic, Cjk, Arabic, Other }`,
  `script_of(&str) -> Script`.
**Steps:**
1. Тесты: `ёлка`==`елка`; `ＡＢＣ`==`abc`; идемпотентность; `script_of` на 4 скриптах.
2. Реализовать; подключить в токенайзер; bump `TOKENIZER_VERSION` до 5 (механизм
   SQ5); коммит.
**Acceptance:** FR-14; SC-8 (частично).
**Verify:** `cargo test --locked --test tokenizer` → ok.

## SQ10 — Arabic: нормализация + стем

**Goal:** арабские словоформы с/без harakat находят друг друга (FR-13, SC-8).
**Files:**
- Modify: `src/index/textnorm.rs` (harakat U+064B..U+0652, tatweel U+0640,
  أإآ→ا, ة→ه, ى→ي)
- Modify: `src/index/tokenizer.rs` (Arabic-скрипт → `Stemmer(Language::Arabic)`)
- Test: `tests/tokenizer.rs`, `tests/search_quality.rs`
**Interfaces:**
- Consumes: `script_of`, стем-механизм SQ7.
- Produces: нормализованный арабский токен + стем-вариант.
**Steps:**
1. Тест: документ с harakat находится запросом без harakat.
2. Тест: стем арабского слова эмитится в позиции сегмента.
3. Реализовать; bump версии до 6 (механизм SQ5); коммит.
**Acceptance:** FR-13; SC-8.
**Verify:** `cargo test --locked --test tokenizer --test search_quality` → ok.

## SQ11 — CJK-биграммы

**Goal:** CJK-текст без пробелов находится биграммами с корректными позициями
(FR-12, SC-8).
**Files:**
- Modify: `src/index/tokenizer.rs` (CJK-раны → биграммы, последовательные позиции;
  одиночный символ → униграмма; латиница в сегменте — обычным путём)
- Test: `tests/tokenizer.rs`, `tests/search_quality.rs`
**Interfaces:**
- Consumes: `script_of` + `split_script_runs` (SQ9), позиционная механика (SQ6);
  смешанный сегмент режется на односкриптовые раны до эмиссии вариантов.
- Produces: биграммы CJK с `position+1` на биграмму.
**Steps:**
1. Тест: `東京国際空港` → `東京,京国,国際,際空,空港` с последовательными позициями.
2. Тест: фраза из двух биграмм находится; смешанный сегмент (`OpenSearchを検索`).
3. Интеграция: CJK-кейс из фикстуры SQ1 → pass.
4. Реализовать; bump версии до 7 (механизм SQ5); коммит.
**Acceptance:** FR-12; SC-8.
**Verify:** `cargo test --locked --test tokenizer --test search_quality` → ok.

## SQ12 — Perf/CJK бюджеты

**Goal:** подтвердить NFR-1 спеки после стемов/биграмм (latency, размер),
включая CJK-корпус (review focus #5).
**Files:**
- Modify: `tests/perf_budget.rs` (CJK-сценарий; существующие бюджеты)
- Test: `tests/perf_budget.rs`
**Interfaces:**
- Consumes: финальный токенайзер/chunker.
- Produces: замеры (числа в PR) и, при необходимости, скорректированные константы
  бюджетов с обоснованием.
**Steps:**
1. Добавить CJK-корпус в perf-сценарий; зафиксировать p95/размер индекса.
2. Прогнать существующие бюджеты; убедиться в отсутствии регресса.
3. Записать числа в PR-описание; коммит.
**Acceptance:** NFR-1 спеки; SC-7: `offline`-гейт зелёный, сетевых крейтов
в дефолтных features нет (CI/review-check `cargo tree`, не автотест).
**Verify:** `cargo test --locked --release --test perf_budget` → ok;
`cargo test --locked --test offline` → ok (Linux-only, на Windows skip).

---

## Post-review convergence: SQ13–SQ16

Внешний аудит (кросс-ревью SQ1–SQ12 + code-ревью) подтвердил 4 контрактно-учётных
пробела и 1 рассинхрон спеки; багов не найдено. SQ13–SQ15 закрывают delta-протокол,
SQ16 — code-hygiene (m15/m10). Порядок: тесты/код → доки (по рекомендации аудита:
поведение fence проверяется до правок спеки — проба сделана: oversized fence/table
не падают, 5018/4005 chars при cap+overlap=1650).

## SQ13 — Fence/table pin + penalty e2e + инвариант `bound_prose`

**Goal:** зафиксировать oversized fence/table тестами (атомарность, валидные lines,
без падений), доказать достижимость `LONG_CHUNK_PENALTY` на реальном пайплайне,
убрать тихую потерю данных в `bound_prose`, запинить tie-брейк `script_of`
(FR-4/FR-11, SC-5).
**Files:**
- Modify: `tests/chunker.rs` (pin-тесты oversized fence/table)
- Modify: `tests/tantivy_index.rs` (penalty e2e через `chunk_markdown`)
- Modify: `src/index/chunk.rs` (`bound_prose` `:339-343`: `debug_assert!` + fallback)
- Modify: `tests/tokenizer.rs` (tie pin `script_of`)
**Interfaces:**
- Consumes: `chunk_markdown`, `CHUNK_OVERLAP`, `LONG_CHUNK_PENALTY`.
- Produces: зафиксированный инвариант «oversized fence/table — атомарный чанк
  > cap+overlap (осознанное исключение SC-5; delta-маркер — SQ15)»;
  `bound_prose` без silent drop (fallback `push_piece` целой строки).
**Steps:**
1. Pin: fence >5000 chars → 1 Code-чанк, текст цел, `line_start/line_end` валидны;
   аналогично table (1 Table-чанк).
2. Penalty e2e: markdown с giant fence → `chunk_markdown` → `add_chunks` → search:
   oversized реально штрафуется (паттерн `score > legal × 1.1`).
3. `bound_prose`: `debug_assert!` + fallback вместо `return` (на валидных входах
   поведение не меняется).
4. Tie `script_of` 2:2 Latin/CJK → Latin (enum-порядок осознан); whole-токен
   не эмитится (stem-only путь), биграммы эмитятся — тест-комментарий.
5. Тесты зелёные; коммит.
**Acceptance:** FR-4/FR-11; SC-5 (поведение зафиксировано тестами).
**Verify:** `cargo test --locked --test chunker --test tantivy_index --test tokenizer`
→ ok.

## SQ14 — SQ1-контракт: `recall_at_k`/`ndcg_at_10` + детерминизм + gate-бранч

**Goal:** буквально выполнить контракт SQ1 (`tasks.md` Interfaces) и шаг 3 брифа
(два прогона), доказать nDCG-only бранч гейта (FR-1/FR-2/FR-3, SC-1).
**Files:**
- Modify: `tests/search_quality.rs` (`evaluate`, `gate_rejects_regression`)
**Interfaces:**
- Consumes: `hit`/`hit_rate`, `ndcg_at_k`, `regression_failures`.
- Produces: `fn recall_at_k(paths, expected, k) -> f64`;
  `fn ndcg_at_10(paths, expected) -> f64` (truncate ≤10 поверх `ndcg_at_k`);
  `evaluate_is_deterministic`; gate-кейс «hit сохранён, nDCG упал».
**Steps:**
1. `recall_at_k`: заменить inline `found` в `evaluate` (идентично: `paths` уже top-k).
2. `ndcg_at_10`: использовать в `evaluate` (все кейсы k=3 < 10 → метрики не
   меняются; baseline не регенерируется).
3. Тест: два `evaluate(&Bench::new())` подряд → равенство.
4. Gate: baseline `hit=true, ndcg=1.0` → current `hit=true, ndcg=0.0` → fail
   с ndcg-сообщением.
5. Тесты зелёные; коммит.
**Acceptance:** FR-1/FR-2; SC-1; контракт SQ1 выполнен буквально.
**Verify:** `cargo test --locked --test search_quality` → ok.

## SQ15 — Delta-маркеры и ledger

**Goal:** устранить рассинхрон спеки (SC-5 ⇔ design §9) и учесть все delta:
FR-3/assumption #3, SC-5, NFR-6, ledger SQ4/SQ8.
**Files:**
- Modify: `requirements.md` (FR-3 `:157-161`, assumption #3 `:264`,
  SC-5 `:248-251`, NFR-6 `:213-214`)
- Modify: `design.md` (§9 `:208-209`)
- Modify: `progress.md`
**Interfaces:**
- Consumes: SQ13-тесты (ссылки в маркерах), коммиты `a777e85`/`2619d5f`.
- Produces: delta-маркеры в SC-5 **и** §9 (синхронные формулировки); ledger-строки
  `Post-review:`; sweep-таблица SQ8.
**Steps:**
1. FR-3 + assumption #3: «хвостовые позиции citation-снапшотов могут дрейфовать
   (не только координаты); критерий — hit@k per-case».
2. SC-5 + design §9: одинаковый маркер «атомарные fence/table — исключение ради
   целостности блока (SQ13 pin-тесты)»; §9 не ссылается на «одиночный абзац».
3. NFR-6: «sweep валиден только на дискриминирующем корпусе; на сатурированном
   golden фиксируется „варианты неразличимы“ (SQ8)».
4. Ledger: SQ4-delta (`error_message` rank-3, `a777e85`); SQ8-таблица (C1–C4 из
   `2619d5f` → все классы 1.0/1.0); `Post-review:` fixed/deferred.
5. Коммит.
**Acceptance:** спека внутренне согласована; каждая delta учтена.
**Verify:** `git diff docs/specs/search-quality/` — SC-5 и §9 совпадают по смыслу.

## SQ16 — Code hygiene: `tokenize`, `camel_split`, `merge_heading_only`, `#[must_use]`

**Goal:** закрыть code-ревью без изменения поведения: `tokenize` ≤50 строк (m15),
один `camel_split` на ран, O(n) `merge_heading_only`, `#[must_use]`; замеры до/после
(m10).
**Files:**
- Modify: `src/index/tokenizer.rs` (`emit_whole`/`emit_runs`, единый `camel_split`,
  `#[must_use]` на чистых хелперах)
- Modify: `src/index/chunk.rs` (`merge_heading_only` без `remove` в цикле)
- Test: существующие сьюты + chunker-тест цепочки заголовков
**Interfaces:**
- Consumes: текущие тесты как контракт поведения.
- Produces: `tokenize` ≤50 строк; `merge_heading_only` O(n); замеры full index
  1000/10000 до/после в ledger.
**Steps:**
1. Baseline-замер (`perf_budget` release): full index 1000/10000, cjk.
2. Вынос `emit_whole`/`emit_runs` (чистый move); единый `camel_split` (флаг из
   `stem_language`/предвычисленный сплит); `#[must_use]` на `stem_language`,
   `identifier_span`, `alnum_runs`, `camel_split`, `overlap_start`,
   `tokenizer_version_matches`; проверить `TextAnalyzer: Default` (derive или ledger).
3. `merge_heading_only`: push/pop-перепись (back-step сохраняется) + тест цепочки
   заголовков.
4. Полный сьют зелёный; замер после; разница — в ledger; коммит.
**Acceptance:** m15/m10; golden/baseline не регенерируются; перф не хуже.
**Verify:** `cargo test --locked` → ok; `cargo test --locked --release --test
perf_budget` → ok.

---

## Scoring evidence: SQ17–SQ18

Аудит BM25-покрытия (после SQ16) нашёл: (а) реальный баг окна penalty —
проба: 17 oversized (fence-like) + 1 compact, `limit=1` возвращает oversized
(0.01390) вместо compact (0.01573), потому что penalty применяется после
отсечения raw top-N; (б) бусты полей (FR-11) не покрыты прямыми тестами —
смена буста не роняет ни один тест (корень недоказуемости SQ8; NFR-6 требует
дискриминирующий корпус).

## SQ17 — Penalty за пределами fetch-окна

**Goal:** top-k считается по финальному скору (raw × penalty), а не после
отсечения raw top-N: результат не должен зависеть от `limit` (FR-11).
**Files:**
- Modify: `src/index/tantivy_index.rs` (`search_reader` `:342-347`)
- Test: `tests/tantivy_index.rs`
**Interfaces:**
- Consumes: `LONG_CHUNK_PENALTY`, `MAX_CHUNK_CHARS + CHUNK_OVERLAP`, `TopDocs`.
- Produces: гарантия «top-k по финальному скору»; окно растёт, пока
  `final_k < min_raw_fetched` и выборка не исчерпана (звуковость: unseen raw ≤
  `min_raw_fetched` ⇒ unseen final ≤ raw ≤ `min_raw_fetched`).
**Steps:**
1. RED: 17 oversized + 1 compact, `limit=1` → compact first (проба подтвердила
   падение на текущем коде).
2. Итеративное расширение окна: после penalty, если окно полное и
   `final_k < min_raw_fetched` → widen (×4, cap — число доков), повтор.
3. GREEN; существующие penalty/golden/perf без изменений; коммит.
**Acceptance:** FR-11; корректный top-k при любом числе oversized-чанков.
**Verify:** `cargo test --locked --test tantivy_index` → ok; полный сьют → ok.

## SQ18 — Boost-порядок и чувствительность

**Goal:** прямые пин-тесты бустов полей — порядок
`identifiers > title(+heading) > heading > text` и окна отношений,
откалиброванные так, чтобы деградация любого буста (≥~30%) роняла тест
(FR-11; дискриминирующий мини-корпус для NFR-6).
**Files:**
- Modify: `tests/tantivy_index.rs` (синтетические доки: термин в одном поле)
**Interfaces:**
- Consumes: текущие бусты (text 1.0, title 2.0, heading 1.5, identifiers 2.5).
- Produces: дискриминирующий мини-корпус; окна отношений зафиксированы в ledger.
**Steps:**
1. Синтетика: text-only; heading-only (`["Zed","target"]` — title без матча);
   title (`["target"]` — title+heading матч); identifier (`target.` — терм
   попадает в identifiers через production `extract_identifiers`).
2. Probe: снять фактические отношения скоров (BM25-длины полей различаются);
   задать окна с запасом.
3. Ассерты порядка + окон; чувствительность: временно ослабить буст → тест
   падает; вернуть.
4. Коммит.
**Acceptance:** FR-11; NFR-6 (дискриминирующий корпус).
**Verify:** `cargo test --locked --test tantivy_index` → ok.

---

## Self-review плана

- **Spec coverage:** FR-1→SQ1, FR-2/3→SQ2, FR-4→SQ4, FR-5→SQ3, FR-6→SQ4,
  FR-7→SQ6, FR-8→SQ5, FR-9/10→SQ7, FR-11→SQ8, FR-12→SQ11, FR-13→SQ10,
  FR-14→SQ9, FR-15→констрейнт (SC-7 в SQ12), FR-16→SQ6. Пробелов нет.
  Post-review convergence: SQ13→FR-4/FR-11/SC-5 (pin/penalty/инвариант),
  SQ14→FR-1/2/3/SC-1 (контракт SQ1), SQ15→доки (FR-3/SC-5/NFR-6 delta),
  SQ16→m15/m10 hygiene.
  Scoring evidence: SQ17→FR-11 (penalty window), SQ18→FR-11/NFR-6 (boost pins).
- **Type consistency:** `MAX_CHUNK_CHARS`/`CHUNK_OVERLAP`/`TOKENIZER_VERSION`/
  `normalize`/`script_of` определены в ранних задачах и используются в поздних.
- **Review focus:** все 5 failure modes привязаны к тестам задач SQ7/SQ5/SQ6/SQ4/SQ12.
- **Proportion:** 12 задач на 16 FR + NFR — план, не транскрипт.
- **Порядок:** строго зависимый: SQ1→SQ2→(SQ3,SQ4)→SQ5→SQ6→SQ7→SQ8;
  SQ9→(SQ10,SQ11); SQ12 — последняя. (SQ5 до SQ6: механизм версии раньше
  первой смены пайплайна.)
