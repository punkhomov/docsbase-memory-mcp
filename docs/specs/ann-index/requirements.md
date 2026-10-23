# SPECIFICATION: ann-index (итерация 3 hybrid-providers)

**Status:** Draft, ревизия 0.2 · **Дата:** 2026-10-22
**Constitution:** `docs/specs/constitution.md` (1.3.0)
**Связано:** `docs/specs/hybrid-providers/` (dense + RRF), ADR-12
(`(model_id, chunk_hash)` кэш), ADR-14 (OpenRouter), ADR-15 (ANN).
**Delta 0.1 (2026-10-22):** первичный план; кода ещё нет. Числа (threshold,
recall, latency, memory cap) — стартовые гипотезы, калибруются в T1/T6.

**Delta T1 (2026-10-22):** spike прошёл на Linux: `hnsw_rs 0.3.5` dump/reload
и mmap работают, `LoadedAnn: Send + Sync`. `self_cell` отклонён: его
`try_new` даёт `&Owner`, а `HnswIo::load_hnsw` требует `&mut self`; `Hnsw`
инвариантен по lifetime, поэтому `#[covariant]` не подходит. Рабочий wrapper —
`ouroboros 0.18.5` с `#[borrows(mut io)]` и `#[not_covariant]`. Windows/macOS
проверены только `cargo check`; runtime — в CI. 10k × 64: build 4.8 s,
mmap/heap load 43/42 ms; жёсткий build ≤5 s для 75k не подтверждён — цель
перенесена в T6.

**Delta T1.1 (2026-10-22, ревью):** v1 переводится на `hnsw_rs 0.3.5`
(0.3.5 — единственная версия) после временного 13-дневного cooldown (2026-10-22 → 23),
потому что 0.3.5 содержит graph-correctness фиксы и убирает `bincode`.
Migration layer не делаем: dump чужой версии/формата отвергается по `hnsw_rs`/`format_version`
и пересобирается. `parallel_insert` — дефолт; warm только фоновый, поиск во
время сборки идёт exact streaming и не блокируется. Добавлены проверки
`usize`/`u64`, `set_searching_mode`, basename `file_dump`, `env_logger` size,
`DistCosine`/recall-ef. ADR-15 финализируется после T6.

**Delta 0.2 (2026-10-22, post-T7 review):** вторая независимая проверка ветки
нашла разрывы между реализацией и уже принятыми FR/NFR. Они входят в scope v1
и закрываются T8–T11; до этого ветка не считается merge-ready:
- **FR-7 / NFR-4:** `DenseCache.max_bytes` — это лимит всего резидентного
  working set, включая exact rows. Один проект сверх лимита отдаётся как
  `TooLarge` (streaming), а не `Ready` со всеми rows; byte-LRU считается по
  всем entries.
- **FR-8 / NFR-3:** cold start сначала пытается загрузить persisted ANN
  (`AnnStore::load`/`AnnIndex::load`), а не читает все векторы. `dim` берётся
  из model/meta (допустимо прочитать один вектор), полный `VectorCache`-скан
  для валидного ANN запрещён.
- **FR-5 / FR-12:** failed background build переходит в явное failed-состояние
  и повторяется после backoff; `Building` не может быть вечным состоянием.
- **FR-11 / NFR-5:** dump/meta синхронизируются через write-handle (Windows
  `FlushFileBuffers` требует `GENERIC_WRITE`); Windows runtime входит в SC-9.
- **FR-7 / lifecycle:** cleanup не удаляет `tmp-*` активного build; retention
  поколений учитывает model; дисковый бюджет ограничен.
- **ef_search:** финальное правило — `max(64, fetch * 4)` с cap, а не
  фиксированные 64; высокие `fetch` покрываются recall-тестом.
- **Coverage capacity:** FR-6 остаётся строгим (ANN только при 100% coverage);
  решение по ёмкости `VectorCache`/pin — в `hybrid-providers` DELTA-9. ANN
  никогда не строится по частичному покрытию.
- **SC-1 scope:** 56.5 s / p95 5.4 ms из T6 — это core-`AnnIndex`, не
  end-to-end; T10 обязан измерить путь `DenseCache`/`VectorCache`/`search_docs`.

**Delta 0.3 (2026-10-23, pre-merge panel + verifier):** три независимых
ревьюера и отдельный верификатор нашли расхождения между заявленными
гарантиями и релизными артефактами. Закрываются T12–T16:
- **SC-4 / release profile:** `[profile.release] panic = "abort"` отключал
  `catch_unwind`-обёртку `hnsw_rs`, поэтому битый persisted-ANN в шипнутом
  бинаре абортил процесс вместо exact fallback. `abort` убран (+1.1 MiB на
  glibc-strip; бюджет NFR-9 — 40 MiB), добавлены compile-time guard
  (`panic = "abort"` = ошибка сборки) и release-профильный CI-тест
  `ann_load_failure_falls_back_to_exact`.
- **FR-7 / NFR-4:** `Building` учитывает граф **и** exact-rows, которые держит
  тред сборки. Сборка не допускается, если сумма превышает `max_bytes` —
  проект остаётся exact `Ready`, а не уходит в неучтённый по памяти build.
- **FR-5 / SC-11 evidence:** e2e-тест обязан доказать, что поиск во время
  сборки не обращается к ANN (debug-счётчик), что опубликованный ANN реально
  обслуживает read-path, и что recall@fetch ≥ 0.95 при `fetch ∈ {100, 1000}`
  против exact-скана. Задержка сборки делает состояние `Building`
  детерминированным.
- **Pins / NFR-4:** backfill-лиз ставится только когда ANN-сборка возможна
  (`ranking = rrf`); `unpin` и истечение лизов запускают eviction; при полном
  отсутствии вытесняемых строк пишется одно предупреждение (FR-9), а не по
  одному на `put`.
- **FR-9:** ошибка `ann.search` логируется отдельно от «insufficient
  candidates»; warm не строится для `ranking = lexical`.
- `DenseCache::invalidate` остаётся для удаления проекта; комментарий
  приведён в соответствие (вызывающего пути в коде пока нет).

**Delta 0.3.1 (2026-10-23, re-review panel):** ре-ревью дельты 0.3 нашло, что
новый admission-контроль учитывал пик `graph + exact` (1.31 GB на 75k×2048), а
дефолтный `max_bytes` оставался 1 GiB, поэтому ANN вообще не собирался на
целевом корпусе (порог — 61 680 чанков при d2048). Закрывается T17–T19:
- **FR-7 / NFR-4:** дефолтный резидентный кап поднят до 1.5 GiB: он покрывает
  транзиентный пик сборки (ANN 691 MB + exact-rows 586 MB = 1.22 GiB), при
  этом steady-state ANN по-прежнему 691 MB, то есть модель NFR-4 не меняется.
  Guard-тест `default_cap_admits_the_documented_target_corpus` фиксирует, что
  целевой корпус проходит admission.
- **T15 / leases:** пин ставится только если сборка действительно возможна —
  `enabled && ranking == rrf` **и** admission по капу (иначе лиз на час
  подвешивал бюджет кеша без сборки).
- **T12 / tests:** debug-хуки (`ann_search_count`, `set_test_build_delay`)
  оставлены debug-only, но тестовый таргет целиком под `#![cfg(debug_assertions)]`,
  поэтому `cargo test --release` снова компилируется; сброс задержки — RAII.
- **T16 / lifecycle:** гейт `ranking == rrf` относится только к `warm`;
  `cleanup`/`cleanup_disk` снова выполняются для любого проекта с embedding,
  чтобы смена rrf→lexical не оставляла старые дампы навсегда.
- Счётчик ANN считает **обслуженные** запросы (не консультации, закончившиеся
  fallback-ом), поэтому «опубликованный ANN обслуживает read-path» проверяется
  без ложноположительного срабатывания.

## 1. Problem

Dense-канал RRF сейчас сканирует все векторы проекта точным косинусом:
`hybrid::dense::top_k` делает O(N) по `DenseIndex.rows`, а при превышении
`DENSE_CACHE_MAX_VECTORS = 1_000_000` переключается на `top_k_iter` —
streaming exact по `VectorCache`. Для проектов 75k+ чанков это:
- линейная latency на каждый RRF-запрос;
- `DenseCache` хранит rows без byte-cap и LRU (при `d=2048` 1M векторов —
  около 8 GiB только на f32);
- `TooLarge`-путь пересчитывает `chunk_hashes` на каждый запрос;
- смена модели перестраивает in-memory rows, а не переиспользует
  persisted-индекс.

Это делает dense-канал непригодным для заявленного масштаба и оставляет
NFR-6 (изменения ранжирования только с замером) без практичного инструмента
на больших корпусах.

## 2. Goals / Non-goals

Goals:
- **G1.** Добавить ANN-кандидатов для dense-стадии RRF на больших проектах,
  сохранив текущий контракт `(chunk_id, score)` и порядок RRF.
- **G2.** Сделать ANN **persistent с первого дня**: ключ, формат, atomic write,
  версия, загрузка и lifecycle не должны переделываться позже.
- **G3.** Всегда иметь exact fallback; ANN не должен превращать ошибку
  оптимизации в ошибку поиска.
- **G4.** Ограничить память и диск: byte-cap, LRU по проектам, очистка старых
  поколений, отказ от ANN при превышении лимитов.
- **G5.** Не строить ANN в одноразовом `docsbase search` direct mode: CLI
  загружает только готовый persisted-индекс, иначе использует exact.
- **G6.** Не менять публичные MCP/CLI-контракты и не включать ANN в golden
  baseline, пока порог не будет измерен отдельно.

Non-goals:
- Новые retrieval-ступени, query expansion, тюнинг RRF `k`/весов.
- Квантование/сжатие векторов (int8/PQ) — отдельная итерация.
- Локальные embedding-модели (это `hybrid-providers` / ADR-13).
- Фильтры, snippets, dedup, no-answer, cross-project search.
- Изменение формата `search_docs`, MCP-схемы или CLI-флагов.
- Замена `VectorCache` как source of truth; ANN — производный кэш.

## 3. Users / Actors

- **U1. Мейнтейнер поиска** — принимает решение о ANN по recall/latency, а не
  по «стало быстрее на глаз».
- **U2. Daemon** — владеет долгоживущим `DenseCache`, строит и загружает ANN.
- **U3. CLI direct mode** — одноразовый процесс; не должен платить за сборку
  HNSW на каждый запуск.
- **U4. Ревьюер** — проверяет persistence-контракт, fallback и отсутствие
  скрытой деградации.

## 4. User stories

- **P1. Scale search.** *Given* проект 75k+ чанков, hybrid RRF включён;
  *When* выполняется `search_docs`; *Then* dense-кандидаты берутся из ANN,
  а p95 не растёт линейно с N.
- **P2. Cold restart.** *Given* persisted ANN для текущего
  `(project, generation, model, dim)`; *When* daemon стартует заново;
  *Then* индекс загружается (mmap) без полной пересборки.
- **P3. Model/generation change.** *Given* сменилась модель или
  `index_generation`; *When* выполняется поиск; *Then* старый ANN не
  используется, новый строится/загружается по новому ключу.
- **P4. No ANN.** *Given* ANN отсутствует, повреждён или превышает лимит;
  *When* выполняется поиск; *Then* результат считается exact-сканом, ошибка
  логируется, поиск не падает.
- **P5. Direct CLI.** *Given* `docsbase search` без daemon и без persisted ANN;
  *When* проект большой; *Then* CLI делает exact, а не многсекундную сборку.
- **P6. Disk safety.** *Given* несколько поколений/моделей; *When* кэш растёт;
  *Then* старые ANN-каталоги удаляются best-effort, а незавершённая запись
  никогда не считается валидной.

## 5. Functional requirements

- **FR-1 (must) ANN candidate generation.** Для dense-стадии RRF при
  `N >= ANN_MIN_VECTORS` кандидаты `fetch` берутся из HNSW; при меньшем N —
  exact `top_k`. Контракт `Vec<(chunk_id, score)>` и tie-break по `chunk_id`
  сохраняются. ANN влияет только на порядок dense-списка; rerank по-прежнему
  работает по текстам.
- **FR-2 (must) Persistent index.** ANN хранится на диске под cache root,
  версионируется, пишется атомарно (temp + `READY` + rename) и загружается
  только после проверки metadata.
- **FR-3 (must) AnnKey + AnnMeta.** `AnnKey` включает `project_id`,
  `index_generation`, `model_id`, `dim`, `chunk_count`, digest отсортированных
  `(chunk_id, chunk_hash)` и `ann_format_version`. `AnnMeta` дополнительно
  фиксирует версию `hnsw_rs`; загрузчик проверяет и ключ, и версию крейта.
  Несовпадение любого поля — индекс не используется.
- **FR-4 (must) Invalidation.** ANN инвалидируется при смене generation,
  модели, chunk set, формата, после backfill и при повреждении файлов.
  Старый индекс не должен молча использоваться.
- **FR-5 (must) Fallback.** Ошибка сборки, загрузки, mmap, чтения или поиска
  ANN переводит запрос на exact. Ошибка ANN не является ошибкой `search_docs`.
  Пустой или явно недостаточный ANN-результат при непустом корпусе также
  считается сбоем ANN и уходит в exact.
- **FR-6 (must) Coverage.** ANN строится/публикуется только при полном
  покрытии текущего chunk set векторами. При неполном покрытии — exact.
- **FR-7 (must) Memory/disk caps.** `DenseCache` имеет `max_vectors`,
  `max_bytes` и `max_projects`; при превышении — LRU/отказ от ANN, затем
  exact/`TooLarge`. Лимиты не должны позволять unbounded рост.
- **FR-8 (must) Daemon vs CLI.** Daemon: `BuildOrLoad`; `docsbase search` direct:
  `LoadOnly`. После успешного sync/backfill daemon прогревает ANN **в фоне**,
  чтобы следующий поиск не строил его в request path.
- **FR-9 (should) Diagnostics.** `status` или daemon-лог показывает состояние
  ANN (disabled/loading/ready/stale/fallback), размер и причину fallback.
  Секреты и тексты документов не логируются.
- **FR-10 (must) Contract stability.** MCP-схема, CLI-флаги, формат
  `search_docs` и golden baseline не меняются. Новых пользовательских
  конфиг-ключей на первом этапе нет.
- **FR-11 (must) Portability.** Default build остаётся offline; новые
  зависимости — только pure-Rust/ADR-15; Linux x86_64, Windows x64,
  macOS aarch64.
- **FR-12 (must) Non-blocking build.** Warm/build не блокирует поиск: пока
  HNSW строится, `search_docs` использует exact streaming (без
  `DenseIndex.rows`), публикация происходит только после `READY`.
  Ошибка фоновой сборки не переводит проект в error.
- **FR-13 (must) Version/format.** v1 пинится на `hnsw_rs 0.3.5`;
  fallback-версии нет. Dump другой версии/формата отвергается по
  metadata и пересобирается; migration layer не нужен.

## 6. Non-functional requirements

- **NFR-1 (latency).** На синтетике 75k × 2048 p95 ANN-поиска ≤ 10 ms или
  ≥5× быстрее exact на том же хосте. Финальный порог — по T1/T6.
- **NFR-2 (recall).** Recall@10 ANN vs exact ≥ 0.95 (цель 0.99) на
  калибровочном наборе; top-1 совпадает на хорошо разделённых точках.
- **NFR-3 (cold start).** Загрузка persisted ANN не делает полную
  пересборку; mmap-путь не удваивает RSS относительно exact rows.
- **NFR-4 (memory).** `DenseCache` учитывает `max_bytes`; steady-state ANN при
  75k × 2048 — ≈691 MB (vectors + graph overhead, ≤ 1 GiB), а транзиентный пик
  фоновой сборки (ANN + exact-rows) — ≈1.22 GiB; дефолтный резидентный кап
  1.5 GiB покрывает пик (Delta 0.3.1). ANN не должна обходить cap.
- **NFR-5 (durability).** Crash/обрыв в любой точке записи не делает
  частичный индекс валидным; `READY` и metadata обязательны.
- **NFR-6 (security).** Cache-файлы считаются локальными, но парсятся
  defensive: проверка размеров, версии, ключа; никакого сетевого доступа.
- **NFR-7 (quality).** `cargo fmt`, clippy `--all-targets -D warnings`,
  MSRV 1.88; без `unwrap`/`expect`/`panic!` в `src/**`.
- **NFR-8 (CI).** Linux/macOS/Windows зелёные; static Linux; default build
  без `reqwest`; новые зависимости проверены ADR и `cargo tree`.
- **NFR-9 (binary size).** `env_logger` — normal-зависимость `hnsw_rs`;
  release/static size до/после измеряется в T6. Бюджет 40 MiB; выход за него —
  отдельное решение, не молчаливое исключение.

## 7. Success criteria

- **SC-1.** 75k × 2048: recall@10 ≥ 0.95; p95 ANN ≤ 10 ms (или ≥5× exact).
  Build измеряется отдельно; 5 s не является обещанием: spike дал 4.8 s уже
  на 10k × 64, поэтому бюджет 75k фиксируется в T6 после замера. T6-числа —
  core-`AnnIndex`; end-to-end через `DenseCache`/`VectorCache`/`search_docs`
  обязан подтвердить T10 (Delta 0.2).
- **SC-2.** При `N < ANN_MIN_VECTORS` результаты и baseline не меняются;
  exact-путь остаётся байт-в-байт прежним.
- **SC-3.** Повторный старт daemon загружает persisted ANN без полного чтения
  векторов и не пересобирает его; проверяется по metadata/времени и счётчику
  `VectorCache::get` (Delta 0.2).
- **SC-4.** Stale/corrupt/missing ANN, включая dump чужой версии/формата,
  даёт exact fallback и явную диагностику; поиск не падает и не пытается
  интерпретировать чужой формат.
- **SC-5.** Прерывание записи не оставляет валидный `READY`-индекс.
- **SC-6.** Превышение `max_bytes`/`max_vectors`/`max_projects` не приводит к
  OOM; LRU/fallback покрыты тестами.
- **SC-7.** `search_bench` baseline не меняется; ANN-порог не задевает 331
  чанк текущего корпуса.
- **SC-8.** Default build без сети; `cargo tree` без `reqwest`; новые крейты
  только из ADR-15.
- **SC-9.** CI Linux/macOS/Windows зелёный; MSRV 1.88; static Linux.
- **SC-10.** MCP/CLI-контракты и `docs/MCP.md` не расходятся с кодом.
- **SC-11.** Поиск во время фоновой сборки возвращает exact-результаты и не
  ждёт завершения build; проверяется тестом с задержанной сборкой.
- **SC-12.** 0.3.5 dump читается после рестарта; dump чужой версии отвергается;
  двойная сборка 0.3.5 с fixed seed проверена (single-thread; parallel —
  отдельный ruling).

## 8. Out of scope

- ANN для `TooLarge` > 1M векторов (streaming exact остаётся safety net).
- Квантование, PQ, int8, SIMD-тюнинг.
- ANN для BM25 или rerank (только dense-канал).
- Per-request `ranking`/веса, фильтры, dedup, snippets, no-answer.
- Автоматическая миграция старых ANN-форматов (только rebuild).
- Публичный Rust API для ANN (внутренний модуль).

## 9. Open questions

- **OQ-1 (закрыт T1, 2026-10-22).** `hnsw_rs` + `ouroboros 0.18.5`.
  `self_cell` не подходит из-за `&mut HnswIo` и инвариантного `Hnsw`;
  `hnswlib-rs` остаётся запасным вариантом, если `ouroboros` упрётся в
  платформу/CI.
- **OQ-2 (закрыт T6, 2026-10-22).** v1 — `hnsw_rs 0.3.5`; fallback-версии
  нет. Dump чужой версии/формата не мигрируем; пересобираем. ADR-15 recorded
  in product design. Для 0.3.5 временно используется **глобальный** cooldown
  13 дней (2026-10-22 → 23), потому что Cargo 1.101 nightly не умеет
  per-package декларативные исключения. T1.5 — обязательный merge gate:
  вернуть `14 days`; 0.3.5 остаётся в `Cargo.lock`.
- **OQ-3 (закрыт Delta 0.2, 2026-10-22).** `ANN_MIN_VECTORS = 4096`,
  `DenseCacheLimits.max_bytes = 1 GiB` (глобальный резидентный cap),
  `max_projects = 8`; `ef_search = max(64, fetch * 4)` с cap. `min_vectors`
  остаётся тестовым/внутренним, не публичным контрактом.
- **OQ-4 (открыт до T9).** Windows runtime, включая writable sync dump/meta и
  удаление generation-каталогов после drop `HnswIo`; `cargo check` не
  считается доказательством.
- **OQ-5 (решено 2026-10-22).** Прогрев — после sync/backfill, в фоне;
  до `READY` поиск идёт exact streaming, без блокировки.
- **OQ-6 (закрыт T5, 2026-10-22).** v1 — log-only диагностика; публичный
  `status`-контракт не меняется, расширение остаётся should на потом.
- **OQ-7 (закрыт T6, 2026-10-22).** Seed API у 0.3.5 нет; побайтовая
  детерминированность `parallel_insert` не обещается. Обязателен детерминизм
  запросов к persisted-индексу, а не идентичность сборки.

## 10. References

- `docs/specs/hybrid-providers/requirements.md` — FR-15…FR-18 (dense/RRF);
  DELTA-9 — `VectorCache` capacity/pinning для ANN coverage.
- `docs/specs/hybrid-providers/progress.md` — accepted minor: byte-cap,
  DenseCache eviction, `TooLarge` hash memoization.
- `src/hybrid/dense.rs` — текущий exact scan, `DENSE_CACHE_MAX_VECTORS`.
- `src/vector_cache.rs` — content-addressed vectors, `CacheLimits`.
- `src/daemon/tools.rs` — `search_docs_with`, `dense_candidates`.
- `src/store/repo.rs` — `chunk_hashes`, `index_generation`.
- `docs/specs/constitution.md` — ADR/dependency rules, no global state.
