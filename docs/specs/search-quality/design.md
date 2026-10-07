# Search Quality — Design (IMPLEMENTATION)

> Feature: `search-quality` (S1–S6). Спека: `docs/specs/search-quality/requirements.md`.
> Продуктовые constraints: `docs/specs/constitution.md` (1.2.0),
> `docs/specs/docsbase-memory-mcp/design.md` (ADR-9/10/12), исследование:
> `docs/search-quality-research.md`.

## 1. Overview

Изменяем лексическую ветку поиска внутри существующего индекса: (а) golden-метрики,
(б) гранулярность чанков в нормализованных единицах с рекурсивным сплитом,
(в) корректные позиции токенов + версия токенайзера, (г) RU/EN стемминг мульти-токенами,
(д) тюнинг бустов/penalty, (е) P3-слой мультиязычности (CJK-биграммы, Arabic,
Unicode-фолдинг). Публичные контракты MCP/CLI не меняются; hybrid/сеть — вне спеки.

## 2. Global constraints

- Rust edition 2024, MSRV 1.88; `cargo fmt`; clippy `--all-targets -- -D warnings`
  (+pedantic), `unsafe_code = warn`.
- Без `unwrap`/`expect`/`panic!` в `src/**`; ошибки — существующая таксономия.
- Без новых крейтов в этом цикле; `unicode-normalization` — только через ADR
  (см. ADR-S3), дефолт — ручной минимум.
- tantivy 0.26 (публичные `Stemmer`, `Language`, `TextAnalyzer`); один writer на
  индекс; индекс — производные данные (rebuild из SQLite).
- Офлайн по умолчанию (конституция 1.2.0); 40 MiB — бюджет самого ПО (NFR-2 продукта).
- Схема индекса версионируется; смена анализатора = полный reindex (FR-8).

## 3. Architecture

```mermaid
flowchart LR
  MD[.md файл] --> CH[chunker<br/>cap в chars, recursive split, overlap]
  CH -->|Chunk{text, lines, heading}| TX[identifier-токенайзер<br/>normalize → script runs → variants@1 position + stems + CJK bigrams]
  TX --> TI[(tantivy: text/title/heading_path/identifiers/text_len)]
  Q[запрос] --> QP[QueryParser<br/>тот же токенайзер поля]
  QP --> BM25[BM25 + бусты полей + LONG_CHUNK_PENALTY]
  TI --> BM25
  BM25 --> HITS[search_docs / docsbase search]
  VER[TOKENIZER_VERSION] -.->|файл версии| OPEN[open_or_create / ReadIndex]
  OPEN -->|mismatch| REBUILD[create_fresh + REBUILD_MARKER → reindex]
```

## 4. Components

### 4.1 `src/index/textnorm.rs` (new) — нормализация и скрипты
Ответственность: Unicode-фолдинг и определение письменности для одного «сырого»
сегмента; без сети и крейтов. FR-13/FR-14, NFR-3 спеки.

```rust
pub enum Script { Latin, Cyrillic, Cjk, Arabic, Other }

/// Ручной минимум (ADR-S3): ё/Ё→е, fullwidth ASCII→ASCII, арабская
/// нормализация (harakat U+064B..U+0652, tatweel U+0640, أإآ→ا,
/// ة→ه, ى→ي). Возвращает исходную строку, если менять нечего.
pub fn normalize(raw: &str) -> Cow<'_, str>;

/// Скрипт по преобладающим буквенным символам (CJK = Han/Hiragana/Katakana/Hangul).
pub fn script_of(s: &str) -> Script;

/// Режет строку на односкриптовые раны: `Vec<(byte_offset, &str)>`.
/// Смешанный сегмент (`OpenSearchを検索`) обрабатывается по ранам, а не
/// целиком через преобладающий скрипт — иначе SQ11-кейс неустраним.
pub fn split_script_runs(s: &str) -> Vec<(usize, &str)>;
```

### 4.2 `src/index/tokenizer.rs` (modify) — позиции, стемы, CJK, версия
Ответственность: единственный источник токенов и на индексации, и на запросе.
FR-7/FR-9/FR-10/FR-12/FR-14/FR-16.

```rust
/// Версия пайплайна токенайзера. Bump при любом изменении нормализации,
/// набора вариантов или стемминга (FR-8). 1 = legacy (индексы без
/// version-файла считаются старыми); 2 = первый версионированный пайплайн.
pub const TOKENIZER_VERSION: u32 = 2;

pub struct IdentifierTokenizer {
    ru: tantivy::tokenizer::TextAnalyzer, // SimpleTokenizer + LowerCaser + Stemmer(Russian)
    en: tantivy::tokenizer::TextAnalyzer, // ... + Stemmer(English)
    ar: tantivy::tokenizer::TextAnalyzer, // ... + Stemmer(Arabic), нужен с SQ10
}
// Rationale: ядро стеммера в tantivy 0.26 непублично (frostem не
// реэкспортируется) — вложенные TextAnalyzer единственного публичного пути;
// LowerCaser внутри дублирует наш lowercase, цена приемлема.

Правила эмиссии (ключевые инварианты):

1. **Одна позиция на сырой сегмент** (FR-7): все варианты (`raw`, `trimmed`,
   `alnum_runs`, `camel_split`, стемы) получают одинаковый `position` при
   `position_length = 1` (Lucene-эквивалент posIncr=0/posLen=1); следующему
   сегменту — `position + 1`. Проверено по исходникам tantivy 0.26:
   phrase-scorer сдвигает позиции терма по индексу и ищет пересечение
   (`phrase_scorer.rs:11-24` `PostingsWithOffset`, `:61`, `:111`
   `intersection_exists`) — матч определяется соседством позиций,
   `position_length` во фразовом матчинге не участвует (0 vs 1 — косметика);
   `postings_writer` использует его лишь для end_position. Группировка на
   одной позиции даёт «синоним-семантику».
2. **Стем только «слов»** (FR-10): стеммится токен, если оригинал — чисто
   алфавитный, длина > 3 символов, не ALL_CAPS, **не содержит `_`/цифр и не является
   частью camel-разбиения** (`defineStore` → `define`/`store` не стеммятся; сам
   идентификатор защищён). Скрипт → стеммер (Cyrillic → RU, Latin → EN,
   Arabic → AR с SQ10); стем
   эмитится в той же позиции; дубликаты отбрасываются существующим `HashSet`.
3. **CJK-биграммы** (FR-12): CJK-ран внутри сегмента → перекрывающиеся биграммы
   с последовательными позициями; одиночный CJK-символ → униграмма. Латиница в том
   же сегменте обрабатывается обычным путём (script runs).
4. **Длина токена** (FR-16): токены > `MAX_TOKEN_CHARS` (40) не эмитятся.

### 4.3 `src/index/chunk.rs` (modify) — cap и рекурсивный сплит
FR-4/FR-5/FR-6, NFR-1 спеки (числа совпадают с NFR-1 продукта).

```rust
/// Лимит в Unicode-символах (не байтах); начальное значение, калибровка
/// ≈300–500 токенов уточняется S2-замером.
pub const MAX_CHUNK_CHARS: usize = 1_500;
/// Хвост предыдущего чанка, добавляемый следующему при сплите абзацев.
pub const CHUNK_OVERLAP: usize = 150; // ≈10%
```

Алгоритм: заголовочный сплит (существующий) → если секция > cap: рекурсивно
абзацы (`\n\n`) → строки → срез по границе токена (пробел/пунктуация,
идентификатор не рвётся); аварийный посимвольный срез — только если границ нет
(безпробельный блоб длиннее cap). Между частями — overlap (хвост ≤
`CHUNK_OVERLAP`); `line_start/line_end` считаются по исходным строкам
(offset→line через существующий `line_starts`); `text_len` — в символах.

### 4.4 `src/index/tantivy_index.rs` (modify) — версия, единицы, бусты
FR-5/FR-8/FR-11, SC-6.

- Файл `docsbase.tokenizer_version` в каталоге индекса (рядом с `REBUILD_MARKER`):
  пишется в `create_fresh` до создания индекса (как маркер) — краш до/во время
  reindex оставляет маркер, следующий open идёт на rebuild. `open_or_create`
  сверяет значение; mismatch/отсутствие → `create_fresh` + `recreated = true`
  (полный reindex из SQLite); `ReadIndex::open` при mismatch возвращает
  `Error::Project` с инструкцией `docsbase index` (как для маркера).
- `text_len` = `chunk.text.chars().count()` (FR-5); `LONG_CHUNK_PENALTY` сравнивает
  символы с `MAX_CHUNK_CHARS + CHUNK_OVERLAP` — легальный overlap-хвост не должен
  штрафоваться.
- Бусты полей и penalty — значения из S5 (FR-11), зафиксированы константами рядом
  с `build_parser` и покрыты golden-guard-тестом.

### 4.5 Тестовые компоненты (new/modify)
- `tests/search_quality.rs` (new): golden-метрики и target-кейсы (FR-1/2/3),
  fixture `tests/fixtures/search-quality/` (RU/CJK/Arabic доки), baseline-отчёт
  (пороги + толеранс, не float-снапшот).
- `tests/tokenizer.rs` (modify): позиции (одна на сегмент), стемы, CJK-биграммы,
  нормализация, длинные токены.
- `tests/chunker.rs` (modify): cap в символах, рекурсивный сплит, overlap, lines.
- `tests/index_job.rs` / `tests/tantivy_index.rs` (modify): rebuild при смене
  `TOKENIZER_VERSION`, `text_len` в символах.
- `tests/search_golden.rs` (не менять снапшоты рангов; при S2 — delta-обновление
  снапшотов цитат, зафиксированное в PR).

## 5. Module interfaces

- `textnorm::normalize(&str) -> Cow<str>` — идемпотентна; вызывается до токенизации
  (и на индексации, и на запросе — один и тот же пайплайн).
- `textnorm::script_of(&str) -> Script`.
- `textnorm::split_script_runs(&str) -> Vec<(usize, &str)>` — границы ранов для
  токенайзера; смешанные сегменты режутся до эмиссии вариантов (SQ11).
- `IdentifierTokenizer::token_stream` — без `Result`, не паникует; инвариант
  «позиции строго возрастают по сегментам, варианты внутри — равны».
- `chunk_markdown(body, max_chars) -> Vec<Chunk>` — сигнатура сохраняется
  (константа меняет смысл единиц); гарантия: `text.chars().count() <= max_chars +
  CHUNK_OVERLAP`.
- `TOKENIZER_VERSION: u32` — публичная константа; читается `tantivy_index`.

## 6. Data flow

1. Файл → `chunk_markdown` (cap/сплит/overlap) → `Chunk {text, heading_path,
   line_start, line_end}`; `text_len` = число символов.
2. Чанк → `IdentifierTokenizer`: `normalize` → whitespace-сегменты → script runs →
   варианты на одной позиции (+стемы, +CJK-биграммы) → tantivy-поля
   `text/title/heading_path/identifiers`.
3. Запрос → `QueryParser` с тем же токенайзером поля → BM25 + бусты + penalty →
   `Hit {chunk_id, doc_id, score}` → citations (`path, heading_path, lines`).
4. Версия: `TOKENIZER_VERSION` → файл версии в каталоге индекса → при mismatch
   `create_fresh` + `REBUILD_MARKER` → reindex из SQLite (индекс производный).

## 7. Dependencies + rationale

| Зависимость | Зачем | Почему она |
|---|---|---|
| `tantivy` 0.26 (есть) | `Stemmer`/`Language` (RU/EN/Arabic), `TextAnalyzer` для стем-фильтров внутри токенайзера | публичный API, без новых крейтов; проверено: `Stemmer::new`, `TextAnalyzer::builder().filter()` |
| (нет) `unicode-normalization` | полный NFKC | отложено: ручной минимум покрывает ё/width/case/Arabic (ADR-S3) |
| (нет) `lindera`/`jieba` | словарная сегментация CJK | словари и размер бинаря; биграммы достаточны (ADR-S4) |

## 8. Required / Forbidden stack

- **Required:** tantivy 0.26 (`Stemmer`, `Language`, `TextAnalyzer`), существующие
  `pulldown-cmark`/`rusqlite`/`tokio`.
- **Forbidden:** новые крейты в этом цикле (кроме ADR-одобренного
  `unicode-normalization`), сетевые клиенты, embedding/LLM, словари CJK/Thai,
  `unsafe`, semantic/LLM-chunking.

## 9. Error handling

- Токенайзер/нормализация — infallible, без паник (конституция); некорректный UTF-8
  невозможен (`&str`).
- Версия не совпала → тот же путь, что у schema-mismatch: recreate + rebuild
  (не ошибка для writer-путей), `Error::Project` для read-only путей с инструкцией.
- Cap-нарушение невозможно по построению (сплиттер гарантирует), кроме одиночного
  абзаца/строки > cap — оговорено в SC-5.

## 10. Testing strategy

- **Unit:** токенайзер (позиции/стемы/биграммы/нормализация/длинные токены),
  chunker (cap/сплит/overlap/lines).
- **Golden:** `search_quality.rs` — recall@k/nDCG + target-кейсы; baseline-отчёт
  с порогами;
  per-class no-regression (FR-3, assumption #7).
- **Integration:** `index_job`/`tantivy_index` — rebuild при смене версии, `text_len`.
- **Perf:** существующий `perf_budget` + замер CJK-корпуса (NFR-1 спеки).
- **Reliability:** существующие lifecycle/crash-safety тесты остаются зелёными;
  writer-путь не меняется (NFR-4 спеки).
- **Offline:** существующий `tests/offline.rs` (дефолт-конфиг); отсутствие
  сетевых крейтов в дефолтных features — CI/review-check `cargo tree`,
  не автотест (FR-15/SC-7).
- **Не тестируем:** качество на внешних корпусах, семантику (вне спеки).

## 11. Risks & trade-offs

| Риск | Митигация |
|---|---|
| Overfit на малый golden-корпус | per-class правила приёмки (assumption #7); directional-метрики |
| Стемминг ломает exact-идентификаторы | мульти-токены raw+стем; FR-10; SC-1 продукта как регресс-тест |
| CJK-биграммы ×2 токенов → latency/размер | cap в символах; отдельный замер CJK (NFR-1 спеки) |
| Снапшоты цитат при S2 | delta-обновление только цитат; hit@k по классам без выпадений (FR-3) |
| Забыли bump `TOKENIZER_VERSION` | константа рядом с токенайзером + тест rebuild (SC-6) |
| Ручная нормализация неполна (NFKC) | ADR-S3: крейт при подтверждённой необходимости |
| Известные находки code-ревью вне скоупа (буст пунктуации в `identifiers`, OR-дефолт парсера) | сознательно не меняются в этом цикле; зафиксированы в research §3 |

## 12. Alternatives considered (ADR-style)

- **ADR-S1. Единицы cap: chars vs tokens vs bytes.** Выбрано: **chars**
  (`MAX_CHUNK_CHARS` меняет смысл на Unicode-символы, калибровка ≈300–500 токенов
  замером). Tokens требуют токенизационного прохода до сплита (сложность, порядок
  нормализация→сплит); bytes дают перекос RU×2 (текущий баг). Последствия: cap
  детерминирован, калибровка документируется.
- **ADR-S2. Размещение стемминга.** Выбрано: **кастомный токенайзер с
  мульти-токенами** (raw+стем на одной позиции). Отклонено: (а) отдельные
  per-language поля `text_ru/text_en` — раздувает схему и запрос; (б) цепочка
  `TextAnalyzer + Stemmer` поверх поля — стеммит идентификаторы и умеет один язык.
  Последствия: пайплайн токенайзера сложнее, но один источник истины для индекса и
  запроса.
- **ADR-S3. Unicode-нормализация.** Выбрано: **ручной минимум** (ё, width, арабская,
  case через `to_lowercase`). Отклонено пока: `unicode-normalization` (полный NFKC) —
  новый крейт; вернуться через ADR, если замеры покажут недостаточность.
- **ADR-S4. CJK.** Выбрано: **биграммы в своём токенайзере** (позиции корректны, без
  словарей). Отклонено: `NgramTokenizer` (position=0 → фразы не работают),
  `lindera`/`jieba` (словари/бюджет).

## 13. Directory structure (затрагиваемое)

```
src/index/
├── textnorm.rs        # new: Unicode-фолдинг + script_of
├── tokenizer.rs       # modify: группировка позиций, стемы, CJK-биграммы, TOKENIZER_VERSION
├── chunk.rs           # modify: recursive split, chars-cap, overlap, lines
├── job.rs             # modify: MAX_CHUNK_CHARS (chars), константы
└── tantivy_index.rs   # modify: version-файл, text_len в chars, бусты/penalty
tests/
├── search_quality.rs  # new: golden-метрики, target-кейсы, baseline
├── fixtures/search-quality/  # new: RU/CJK/Arabic корпус
├── fixtures/search-quality/baseline.json  # new: отчёт метрик, не insta-снапшот
├── tokenizer.rs       # modify: позиции/стемы/биграммы/нормализация
├── chunker.rs         # modify: cap/сплит/overlap
├── index_job.rs       # modify: rebuild при смене версии
└── tantivy_index.rs   # modify: text_len в chars
```

## 14. Traceability (FR → компонент)

| FR | Компонент | Тест |
|---|---|---|
| FR-1/2/3 | `tests/search_quality.rs` | golden + target + no-regression |
| FR-4/5/6 | `chunk.rs`, `job.rs`, `tantivy_index.rs` | `chunker.rs`, `index_job.rs` |
| FR-7 | `tokenizer.rs` | `tokenizer.rs` (позиции/фразы) |
| FR-8 | `tokenizer.rs` (const), `tantivy_index.rs` | `index_job.rs` (rebuild) |
| FR-9/10 | `tokenizer.rs` | `tokenizer.rs` + SC-1 продукта |
| FR-11 | `tantivy_index.rs` | golden-guard |
| FR-12/13/14 | `tokenizer.rs`, `textnorm.rs` | `tokenizer.rs` |
| FR-15 | отсутствие сетевых крейтов | SC-7 |
| FR-16 | `tokenizer.rs` | `tokenizer.rs` |
