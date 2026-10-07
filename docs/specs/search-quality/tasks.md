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
  `text.chars().count() <= MAX_CHUNK_CHARS + CHUNK_OVERLAP`, срез по границе
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

## Self-review плана

- **Spec coverage:** FR-1→SQ1, FR-2/3→SQ2, FR-4→SQ4, FR-5→SQ3, FR-6→SQ4,
  FR-7→SQ6, FR-8→SQ5, FR-9/10→SQ7, FR-11→SQ8, FR-12→SQ11, FR-13→SQ10,
  FR-14→SQ9, FR-15→констрейнт (SC-7 в SQ12), FR-16→SQ6. Пробелов нет.
- **Type consistency:** `MAX_CHUNK_CHARS`/`CHUNK_OVERLAP`/`TOKENIZER_VERSION`/
  `normalize`/`script_of` определены в ранних задачах и используются в поздних.
- **Review focus:** все 5 failure modes привязаны к тестам задач SQ7/SQ5/SQ6/SQ4/SQ12.
- **Proportion:** 12 задач на 16 FR + NFR — план, не транскрипт.
- **Порядок:** строго зависимый: SQ1→SQ2→(SQ3,SQ4)→SQ5→SQ6→SQ7→SQ8;
  SQ9→(SQ10,SQ11); SQ12 — последняя. (SQ5 до SQ6: механизм версии раньше
  первой смены пайплайна.)
