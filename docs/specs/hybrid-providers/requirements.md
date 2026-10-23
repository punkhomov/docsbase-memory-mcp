# SPECIFICATION: hybrid-providers (итерация 1 — OpenRouter-only)

**Status:** Draft, ревизия 2.2 · **Дата:** 2026-10-14 (черновик — 2026-10-08;
delta 2026-10-22)
**Constitution:** `docs/specs/constitution.md` (1.2.0)
**Связано:** ADR-12 (кэш `(model_id, chunk_hash)`), ADR-13 (OpenRouter-старт, флаги, 40 MiB — вес ПО)

**DELTA-8 (2026-10-22):** ANN-индекс вынесен в отдельную спеку
`docs/specs/ann-index/` (draft 0.1): persisted HNSW для dense-канала RRF,
`hnsw_rs` + `ouroboros`, exact fallback. Для итерации 2 ANN остаётся
non-goal; реализация — iteration 3.

**DELTA-9 (2026-10-22, post-T7 review):** ANN требует 100% покрытия
(ann-index FR-6), а дефолтный `VectorCache` в 256 MiB не вмещает целевой
75k×2048 (~586 MiB f32); LRU-eviction делает coverage <100% постоянным, и
повторный sync не помогает (он снова вытесняет часть). Решение:
- `VectorCache` byte limit становится настраиваемым; дефолт поднимается до
  **1 GiB** (это disk-cache, не RAM).
- Backfill/ANN build держит per-project lease/pin: векторы проекта нельзя
  вытеснить, пока индекс строится; lease снимается после `READY`/fallback.
- Coverage остаётся гейтом: `cached < total` → ANN не строится, причина
  видна в `status`/логе.
- Это осознанное исключение из ann-index FR-10 «no new config keys»: ключ
  принадлежит hybrid-providers и будет добавлен в `docs/CONFIG.md` в T13.
Cross-ref: `docs/specs/ann-index/requirements.md` Delta 0.2; задача T13.

**Delta T1-спайка (2026-10-14, живые вызовы):** дефолтная embedding-модель —
`qwen/qwen3-embedding-8b` @ `dimensions=1024` (MRL): `qwen/qwen3-embedding-0.6b`
в каталоге OpenRouter ОТСУТСТВУЕТ (404). Факты: qwen3-8b нормирует вектора
(L2=1.000), `dimensions` resizable (256…4096, дефолт 4096), `input_type` даёт
минимальный эффект (cos 0.9976); rerank-ответ `results[].{index, relevance_score,
document.text}`, `top_n` клампится к числу документов, пустые `documents` → 400
(вызов пропускаем), `usage.{search_units,cost}`. Фикстуры — `tests/fixtures/openrouter/`.
Провайдерский дефолт кода при моделях с `@d1024@l2` — см. FR-5/P1/FR-13.

**Delta ревизии 2.1 (2026-10-14):** закрыты 6 Important + 16 Minor свежего
ревью (ungated `hybrid::config` для feature-off-проверки, хук backfill в
`daemon::server`, `hybrid::blocking` с thread-local маркером вместо
`Handle::try_current`, `window`/`top_n`-контракт, sequential-батчи и
golden-замер в NFR-1, seam `search_docs_with`; мелкие: заголовки, `enabled`-дефолт,
пустой candidates, источник текста, feature-gated тест-файл, ADR-14 в T2, R7-эвикция).

**Delta ревизии 2 (2026-10-14):** закрыты 7 блокеров ревью от 2026-10-13
(TOML-форма `hybrid`, поведение feature-off, backfill, контракт fusion, двойной
`/v1` в base_url, rustls roots, `dim`/L2 в ключе кэша) и внесены факты ресёрча
2026-10-14 (OpenRouter embeddings/rerank API: `input_type`, `relevance_score`,
`search_units`, каталог моделей и цены, коды ошибок). Разделы, где черновик
противоречил текущему коду, помечены `DELTA-2`.

## Problem

BM25-поиск (`index/tantivy_index.rs`) не ловит парафразы и семантически близкие
формулировки в русскоязычных доках. Hybrid-ступени (embedding + reranker)
запроектированы (ADR-12/13, `VectorCache`, `Config::hybrid`), но ни один
провайдер не подключён: `Embedder` реализован только fake'ами в тестах, rerank
отсутствует как сущность (проверено по коду 2026-10-14).

## Goals / Non-goals

Goals (итерация 1):
- G1. Embedding чанков через OpenRouter (`POST {base}/embeddings`), дефолт
  `qwen/qwen3-embedding-8b` @1024d (`DELTA-T1`), с записью в существующий `VectorCache`
  (контракт `(model_id, chunk_hash)` не меняется — меняется композиция строки
  `model_id`, см. FR-5).
- G2. Rerank BM25 top-k через OpenRouter (`POST {base}/rerank`), дефолт
  `cohere/rerank-v3.5`, окно `window = 50` (семантика — FR-11).
- G3. Один ключ, один base URL, две ручки; sync-контракт без переделки кэша.
- G4. Дефолтная сборка остаётся полностью офлайн (SC-8, NFR-5 без изменений).

Non-goals (явно позже, отдельными задачами): dense retrieval + RRF-fusion
(итерация 2; embeddings итерации 1 готовят кэш под неё), ANN-индекс
(iteration 3, spec `ann-index`, ADR-15; `usearch`/`sqlite-vector-rs` отклонены), прямые вендоры (Cohere/Voyage/…), Ollama/llama.cpp,
локальные модели (`fastembed`, Model2Vec), `encoding_format=base64`,
автоподбор `dimensions`, `return_logits`-rerank, streaming, async-трейт
(решение 2026-10-08, триггер пересмотра: веерные per-request запросы с дедлайном).

## Users / Actors

- U1. Оператор docsbase (владелец машины): включает hybrid в `.docsbase.toml`,
  кладёт `OPENROUTER_API_KEY` в env, платит за токены.
- U2. Daemon (`docsbase serve`): фоновая индексация батчами, per-request rerank поиска.
- U3. Разработчик следующей итерации (dense+RRF): добавляет ступень, не меняя
  wire-контракт и кэш.

## User stories

- **P1.** Index с эмбеддингами. *Given* hybrid включён, валидный `OPENROUTER_API_KEY`,
  проект с N чанками; *When* выполняется `index_project`/sync; *Then* все чанки
  получают вектора в `vectors.db` под ключом
  `openrouter/qwen/qwen3-embedding-8b@d1024@l2`, повторный прогон сети не
  касается (cache hit 100%).
- **P2.** Поиск с rerank. *Given* hybrid включён и reranker активен; *When*
  `search_docs(query)`; *Then* BM25 top-k (окно ≥ `top_n`) переупорядочены
  OpenRouter-rerank, ответ содержит те же поля цитат (формат MCP-ответа не
  меняется; `score` = `relevance_score`, см. FR-11).
- **P3.** Конфигурация оператором. *Given* `.docsbase.toml` с hybrid-секцией и
  env-ключом; *When* проект открывается; *Then* hybrid-ступени активны; *Given*
  ключа нет или сборка без feature; *When* hybrid включён; *Then* явная
  `Admission`-ошибка с инструкцией (никакого молчаливого fallback на BM25-only).
- **P4.** Офлайн-дефолт. *Given* `hybrid=false` (дефолт) или сборка без feature;
  *When* любой сценарий v1; *Then* ноль сетевых вызовов (`tests/offline.rs`
  зелёный), поиск работает как раньше.
- **P5.** Backfill. *Given* проект, проиндексированный до включения hybrid;
  *When* hybrid включается и проект открывается; *Then* daemon ставит sync-джобу,
  которая до-embed'ит отсутствующие чанки; `status` показывает покрытие
  (`embed_coverage`), поиск работает на протяжении backfill.

## Functional requirements

- **FR-1.** `OpenRouterEmbedder: Embedder` — батч `POST {base}/embeddings`
  `{model, input[], dimensions, input_type: "search_document"}` →
  `Vec<Vec<f32>>` по порядку входа. `base = https://openrouter.ai/api/v1`
  (без хвостового `/`); пути — `/embeddings`, `/rerank` (без второго `/v1`).
  Спайк: `dimensions` — MRL-truncation (200 на 256…4096), дефолт модели 4096;
  отправляется из конфига (модели без MRL — без параметра; фикстура pplx
  записана без него); qwen3-8b возвращает уже нормированные вектора (L2=1.000);
  эффект `input_type` минимален (cos 0.9976) — поле остаётся в контракте.
  Порядок сборки — по `index`, **когда провайдер его вернул** (поле advisory:
  документация OpenRouter его опциональна, отсутствие = порядок входа).
  Строку `model` в ответе не сверять с запрошенной: провайдер нормализует
  регистр/вендорный префикс (`qwen/qwen3-embedding-8b` → `Qwen/Qwen3-Embedding-8B`).
  `DELTA-2`, `DELTA-T1`.
- **FR-2.** `OpenRouterReranker: Reranker` — `POST {base}/rerank`
  `{model, query, documents, top_n}` → `Vec<(original_index, relevance_score)>`,
  desc по score. Ответ разбирается из `results[].{index, relevance_score}`;
  `usage.search_units` логируется. Строку `model` в ответе не сверять с
  запрошенной (`cohere/rerank-v3.5` → `rerank-v3.5`; у 4-pro — `rerank-v4.0-pro`).
  `DELTA-2`, `DELTA-T1`.
- **FR-3.** `provider = "openrouter"` — единственное допустимое значение в
  итерации 1; иное → `Admission`-ошибка («итерация 1: только openrouter»).
- **FR-4.** API-ключ — только из env (`OPENROUTER_API_KEY`); в toml-интерфейсе
  поля под ключ нет; ключ не попадает в логи, `Debug`, `status` (маскирование).
  Заголовки — по FR-9.
- **FR-5.** Строка `model_id` (ключ кэша) = `openrouter/<model>@d<dim>@<post>`,
  где `post ∈ {l2, raw}` — нормализация; `dimensions` входит в ключ. Rerank не
  кэшируется (ключ не нужен). `DELTA-2` (блокер: dim и L2 не были в ключе).
- **FR-6.** Чанкинг `embed_many`: лимит числа текстов на запрос (дефолт 128 до
  P0-spike) + UTF-8 byte-budget; переполненный одиночный текст — отдельным
  запросом; склейка строго по порядку.
- **FR-7.** Retry: 429/500/502/503/524/529 и таймауты — экспоненциальный
  backoff с капом и джиттером (учёт `Retry-After` при 429); 400/401/402/403/404/413
  — сразу ошибка без ретраев. `DELTA-2`. Бюджет попыток и коды 524/529 —
  предварительно, по докам OpenRouter (спайк их не провоцировал); калибровка —
  при первом инциденте. Спайк подтвердил: 404 «No endpoints found for {model}»
  (недоступная модель), 400 «Input is empty» (пустой батч), 400 zod-envelope на
  `top_n < 1`; конверт ошибки — `{"error":{"message","code"}}`. `DELTA-T1`.
- **FR-8.** Client-side L2-нормализация — конфиг-флаг `normalize` (презумпция
  `true`; флаг входит в ключ кэша, FR-5). Спайк: qwen3-8b уже нормирует
  (L2=1.000) → `true` идемпотентен; pplx-0.6b возвращает raw (4.83) — флаг
  сохраняется как защита при смене модели. `DELTA-T1`.
- **FR-9.** Заголовки OpenRouter: `HTTP-Referer`, `X-Title` (значения —
  константы, финализируются в T2; OQ-2 закрывается там же).
  `X-OpenRouter-Categories` не задаём.
- **FR-10.** Трейты остаются sync (`Embedder::embed`, новый `Reranker::rerank`);
  async только внутри реализации, ожидание — в `spawn_blocking` (ADR-7); вызов
  embedder/reranker из async-контекста без `spawn_blocking` — явная ошибка (I3).
- **FR-11.** Fusion-контракт итерации 1 (rerank-only): вход — финальный BM25-список
  (после penalty/сортировок); окно `window = max(limit, rerank_window)`, где
  `rerank_window` — конфиг (дефолт 50, границы 10..=100), но не больше числа
  найденных хитов; документы — `text` чанка (тот же текст, что индексируется, не
  heading-path). Один rerank-запрос с `top_n = window` (провайдер возвращает
  ≤ window ранжированных; фактический лимит `limit` обеспечивается срезом у нас);
  порядок — строго по `relevance_score` (listwise), срез до `limit`. Ноль
  кандидатов → rerank не вызывается, пустой ответ как у BM25 (спайк: пустые
  `documents` — 400 у провайдера, т.е. пропуск обязателен; `top_n` клампится к
  числу документов, `top_n=999` → 200; у нас всегда `top_n = window ≥ 10`).
  `score` в MCP-ответе
  для переупорядоченных хитов = `relevance_score`; формат и остальные поля
  неизменны. Ошибка ступени — явная ошибка запроса (NFR-2). Качество rerank
  измеряется на golden (recall@k/nDCG, rerank on/off; live-замер в T5 — конституция:
  ранжирование меняется только с замером). RRF/dense — итерация 2 (k=60 дефолт
  по Elasticsearch, веса тюнятся на golden). `DELTA-2` (блокер: контракт fusion
  не был определён).
- **FR-12.** Per-request выключатель: `search_docs` получает опциональный параметр
  `rerank: bool` (дефолт — конфиг проекта); `rerank=false` возвращает строгий
  BM25-порядок. Per-project выключатели ступеней — конфиг (FR-13). `DELTA-2`
  (конституция: ступени включаются per-project и per-request).
- **FR-13.** Конфиг: `hybrid` принимает **две формы** (`DELTA-2`, блокер — сейчас
  `hybrid: bool` и `deny_unknown_fields`):
  ```toml
  hybrid = true                      # шорткат: дефолты ступеней
  # или полная форма
  [hybrid]
  enabled = true
  [hybrid.embedding]
  provider = "openrouter"
  model = "qwen/qwen3-embedding-8b"
  dimensions = 1024
  normalize = true
  [hybrid.reranker]
  provider = "openrouter"
  model = "cohere/rerank-v3.5"
  window = 50
  ```
  Разбор — `#[serde(untagged)]` enum (bool | таблица); отсутствующая секция
  ступени = ступень выключена; отсутствующие поля (включая `enabled`) — дефолты,
  `enabled` по умолчанию `true` (наличие таблицы = намерение включить). Слияние
  глобального и проектного конфига — замена значения целиком (существующее
  поведение precedence). CLI-флага `--hybrid` нет и не добавляется (YAGNI);
  управление — только конфиг-файлами.
- **FR-14.** Feature-off: сборка без `hybrid-remote` при включённом в конфиге
  hybrid → `Admission`-ошибка при открытии проекта (daemon) / старте команды
  (CLI) с инструкцией («пересоберите с `--features hybrid-remote` или выключите
  hybrid»). Проверка живёт в **ungated** резолвере (`src/hybrid/config.rs`,
  компилируется в обеих сборках), поэтому тестируется в дефолтной сборке.
  Молчаливый BM25-only запрещён. `DELTA-2` (блокер: поведение не было определено).
- **FR-15.** Backfill: при открытии проекта с hybrid, если в кэше нет векторов
  под текущий ключ для части чанков, ставится sync-джоба (FR-17 продукта),
  до-embed'ящая отсутствующие (content-addressed cache: уже посчитанные чанки
  бесплатны). Покрытие видно в `status` как `embed_coverage`
  (`cached/total`). Поиск на протяжении backfill работает; rerank не зависит от
  покрытия. Эвикция LRU-кэша (дефолт 256 MiB) может держать покрытие < 100%
  (особенно на больших проектах); повторный sync до-embed'ит выпавшее — это
  документированная стоимость, покрытие видно в `status`. Для ANN этого
  недостаточно: ann-index FR-6 требует 100%, поэтому capacity/pinning
  регулируются DELTA-9 (настраиваемый кап + lease на время build).
  `DELTA-2` (блокер: политики backfill не было).

## Non-functional requirements (ISO 25010)

- **NFR-1 Performance:** backfill 50k чанков — фоновая sync-джоба с
  последовательными батчами (параллелизм — оптимизация следующих итераций по
  данным замеров); daemon в это время отвечает и ищет без деградации.
  Per-request rerank окна 50 — p95 ≤ BM25 +2 с (замер в PR, не гейт). Качество
  rerank — recall@k/nDCG на golden с rerank on/off (live-замер в T5; конституция:
  ранжирование меняется только с замером).
- **NFR-2 Reliability:** батч валидируется целиком до записи в кэш (ADR-12:
  «полу-поколение» запрещено); сетевой сбой ступени — явная ошибка операции,
  не тихий BM25-fallback.
- **NFR-3 Security:** дефолт офлайн; сеть только при `hybrid=true`; allowlist —
  ровно `https://openrouter.ai/api/v1` (origin + path-prefix; нарушение — ошибка,
  не warning); ключ только env.
- **NFR-4 Maintainability:** один wire-код на оба вызова; новый провайдер позже —
  только конструктор пресета, без touching wire-кода.
- **NFR-5 Portability:** `hybrid-remote` — cargo-feature (первый `[features]` в
  `Cargo.toml`); дефолтный статический артефакт без сетевых крейтов, 40 MiB
  бюджет (NFR-2 продукта) не затронут.

## Data model

Без новых таблиц. `vectors.db` (`src/vector_cache.rs`): ключ
`(model_id, chunk_hash)` — существующий; композиция `model_id` расширяется по
FR-5. `dim` строки сверяется с ответом (несоответствие → `Internal`, батч не
пишется). Rerank-скоры не персистятся. Покрытие (`embed_coverage`) считается
запросом к `vectors.db` по хешам чанков проекта.

## Interfaces

```rust
// переиспользуется из src/vector_cache.rs (сигнатуры не меняются)
pub trait Embedder: Send + Sync {
    fn model_id(&self) -> &str;
    fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>>;
}
// новый трейт, src/hybrid/mod.rs (без model_id: rerank не кэшируется, YAGNI)
pub trait Reranker: Send + Sync {
    fn rerank(&self, query: &str, docs: &[String], top_n: usize) -> Result<Vec<(usize, f32)>>;
}
```

`Embedder::model_id()` возвращает ключ по FR-5 (используется `VectorCache`).

## Measurable success criteria

- **SC-1.** P1 на фикстуре ≥100 чанков: второй прогон — 0 HTTP-вызовов (счётчик
  в fake-транспорте); ключ включает dim и normalize.
- **SC-2.** P2: rerank меняет порядок синтетического BM25-top-k предсказуемо;
  `rerank=false` возвращает исходный порядок (тесты на фикстурах).
- **SC-3.** P3: отсутствие ключа / feature-off при `hybrid=true` → `Admission`
  с инструкцией (тесты).
- **SC-4.** P4: `cargo test --locked --test offline` зелёный; дефолтная сборка
  без `reqwest` (`cargo tree` в CI-check).
- **SC-5.** Wire-тесты на записанных фикстурах OpenRouter (схемы запрос/ответ,
  все ветки ошибок, порядок, L2) — без сети в CI; feature-сборка тестируется
  отдельной CI-джобой.
- **SC-6.** P5: backfill-джоба до-embed'ит отсутствующие чанки; `embed_coverage`
  в `status` сходится (тест).

## Assumptions & challenges

- **A1.** Схемы OpenRouter стабильны на итерацию (спайк; дрейф — явная ошибка
  парсинга, не молчание).
- **A2 (закрыто спайком).** `qwen/qwen3-embedding-0.6b` в роутере НЕТ (404) →
  дефолт — `qwen/qwen3-embedding-8b` @1024 (32k ctx, $0.01/M, нормированные вектора,
  MRL 256…4096). Fallback из каталога (33 модели): `perplexity/pplx-embed-v1-0.6b`
  (1024d, $0.004/M, raw-вектора), `baai/bge-m3` (1024d, $0.01/M), `voyageai/voyage-4-lite`
  (1024d, $0.02/M). Rerank: `cohere/rerank-v3.5` (дефолт, $0.001/search) проверен
  живьём; доступны `cohere/rerank-4-pro` (другие скоры, 32k ctx) и `cohere/rerank-4-fast`.
- **A3.** OpenRouter не версионирует веса провайдеров → тихий апдейт веса не
  инвалидирует кэш. Принято как известное ограничение итерации 1 (дешёвый риск;
  дорогой контроль — пин провайдера/ревизии — отложен).
- **A4.** Sync вместо async: батч+параллелизм дают весь выигрыш; триггер
  пересмотра — веерные per-request запросы с дедлайном.
- **A5.** Почему не напрямую Cohere: один ключ/биллинг/схема и нормализация
  OpenRouter перевешивают; прямой адаптер — позже за тем же трейтом.

## Out of scope

См. Non-goals. Дополнительно: смена формата MCP-ответа (кроме `score`-семантики
по FR-11), мультимодальные входы OpenRouter-rerank, локальные провайдеры.

## DELTA-3 (2026-10-15): итерация 2 — dense-канал и RRF-fusion

Заменяет плейсхолдер в FR-11 («RRF/dense — итерация 2… контракт fusion не был
определён»): контракт фиксируется ниже до кода.

- **FR-15. Dense-канал (косинус по кэшу векторов).** При `hybrid.ranking = "rrf"`
  поиск строит второй список: query-эмбеддинг тем же `Embedder`/`cache_model_id`,
  косинусная близость (для L2-нормированных = скалярное произведение) против
  векторов **текущей ревизии** проекта; кандидаты — чанки, у которых есть вектор;
  чанки без вектора пропускаются (частичное покрытие допустимо: backfill сходится
  в фоне, покрытие видно в `status.embed_coverage`); top-`fetch`,
  `fetch = max(limit, window)`. Пустой dense-список не ошибка — выдача равна
  BM25-части RRF (деградация при нулевом покрытии).
- **FR-16. RRF-fusion.** Списки BM25 и dense объединяются Reciprocal Rank Fusion:
  `score = Σ 1/(60 + rank)` (rank с 1), `k = 60` — константа `RRF_K`, веса равные.
  Детерминированный тай-брейк при равенстве: (1) больше вхождений в списки,
  (2) лучший ранг, (3) BM25-скор убыв., (4) `chunk_id` возр. После fusion
  rerank-ступень (если активна) работает как в FR-11: тексты fused-окна,
  `top_n = window`, порядок и `score` = `relevance_score`; финальный срез — `limit`.
- **FR-17. Конфиг `hybrid.ranking`.** Значения `"lexical" | "rrf"`, дефолт
  `"lexical"` (смена порядка выдачи — только опти-ин, NFR-6). `"rrf"` требует
  включённой embedding-ступени; иначе ошибка валидации (`Admission`). Неизвестное
  значение — ошибка конфига. Per-request параметр — итерация 3 (как и веса/`k`).
- **FR-18. Семантика `score`.** С активным rerank — как в 2.1 (`relevance_score`).
  Без rerank: `ranking = "lexical"` — BM25-скор (не меняется); `ranking = "rrf"` —
  RRF-скор (rank-based, не сопоставим с BM25 — документируется в CONFIG.md).
  Уточнение FR-12 из 2.1: `rerank=false` отключает только rerank-ступень, порядок
  определяет `ranking`; формулировка «строгий BM25» относится к `ranking = "lexical"`.

**Non-goals итерации 2:** ANN-индекс (iteration 3, spec `ann-index`), локальные модели, тюнинг `k`/весов,
per-request `ranking`, квантование/сжатие векторов, параллельный скан.

## Open questions

- **OQ-1 (ЗАКРЫТ спайком 2026-10-14, `DELTA-T1`):** `qwen3-embedding-0.6b`
  отсутствует → дефолт `qwen3-embedding-8b`@1024; `dimensions` resizable
  (256/512/1024/2048/4096 — 200, dim точный; дефолт 4096); qwen3-8b нормирует
  (1.000), pplx-0.6b raw (4.83), bge-m3 нормирует (1.000); мультиязычный probe
  (RU-запрос): qwen3-8b 0.837 против 0.37–0.41 нерелевантных (bge-m3 0.769/0.295–0.532;
  pplx 0.691/0.105–0.225); коды: 404 модель, 400 пустой input, 400 `top_n<1`
  (zod-envelope), пустые `documents` → 400; rerank `top_n` клампится; фикстуры —
  `tests/fixtures/openrouter/`. Лимиты docs/call по-прежнему берём из батч-конфига
  (≤128 текстов / byte-budget), 429/5xx не провоцировались — политика по докам.
- **OQ-2 (ЗАКРЫТ в T2):** `HTTP-Referer = https://github.com/punkhomov/docsbase-memory-mcp`,
  `X-Title = docsbase-memory-mcp`; `X-OpenRouter-Categories` не задаём (константы
  в `src/hybrid/openrouter.rs`).

## References (ресёрч 2026-10-14)

- OpenRouter: Embeddings API (`/embeddings`, `/embeddings/models`, `input_type`,
  `dimensions`, `encoding_format`), Rerank API (`/rerank`, `results[].relevance_score`,
  `usage.search_units`), каталог и цены (blog «Best Embedding Models in 2026»).
- `docs/search-quality-research.md` §4.2 (BGE-M3+reranker, RRF k=60, contextual retrieval).
- Qwen3-Embedding model card (MMTEB 64.33 у 0.6B против 59.56 у BGE-M3; 1024d, MRL).
- EmbeddingGemma (300M, on-device) и Model2Vec (static embeddings) — кандидаты
  локальной итерации; `sqlite-vector-rs`/`hnsw_rs` — кандидаты ANN-ступени.
