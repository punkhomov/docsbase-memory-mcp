# Technical Design: ann-index (итерация 3 hybrid-providers)

**Status:** Draft, ревизия 0.2 · **Дата:** 2026-10-22
**Spec:** `docs/specs/ann-index/requirements.md` (ревизия 0.2)
**Связано:** `docs/specs/hybrid-providers/design.md` (dense + RRF), ADR-12,
ADR-14; **ADR-15** — ANN-зависимость и формат.

**Delta 0.1 (2026-10-22):** первичный дизайн; код не реализован. Все числа и
выбор крейта подтверждаются spike T1 до интеграции.

**Delta T1 (2026-10-22):** spike прошёл. `self_cell` не подходит: `try_new`
даёт `&Owner`, а `HnswIo::load_hnsw` требует `&mut self`; `Hnsw` инвариантен
по lifetime. Рабочий вариант — `ouroboros 0.18.5`:

```rust
#[ouroboros::self_referencing]
struct LoadedAnn {
    io: HnswIo,
    #[borrows(mut io)]
    #[not_covariant]
    index: Hnsw<'this, f32, DistCosine>,
}
```

10k × 64: build 4.8 s, dump 15 ms, mmap/heap load 43/42 ms, top-10 совпал;
`LoadedAnn: Send + Sync`. Windows/macOS — `cargo check`, runtime в CI.

**Delta T1.1 (2026-10-22, ревью):** v1 — `hnsw_rs 0.3.5`,
временный 13-дневный cooldown; dump чужой версии отвергается, не мигрируется.
T1.5 (2026-10-23): cooldown возвращён к политике 14 дней, 0.3.5 остаётся запиненным.
`parallel_insert` — дефолт, warm — фоновый, exact streaming во время сборки.
Добавлены `usize`/`u64`, `set_searching_mode`, basename, env_logger size,
`DistCosine`/recall-ef. ADR-15 recorded in product design; final version/format pin in T6.

**Delta 0.2 (2026-10-22, post-T7 review):** implementation rules corrected:
1. §5.1: sync dump/meta через write-handle; Windows `FlushFileBuffers` требует
   `GENERIC_WRITE`.
2. §6: `ef_search = max(64, fetch * 4)` с cap; фиксированные 64 — не финал.
3. §8: порядок lookup — generation+digest → key/`dim` → `AnnStore::load` →
   `AnnIndex::load`; rows читаются только на miss/build. `max_bytes` — общий
   резидентный cap, включая exact rows; проект сверх cap → `TooLarge`.
   Failed build → `Failed(retry_at)` и retry после backoff, не вечный
   `Building`.
4. §9: backfill — key-based invalidation + warm; explicit `invalidate` — для
   удаления проекта. Cleanup не трогает активные `tmp-*`, хранит поколения
   per-model, соблюдает глобальный disk budget.
5. §10: cleanup не запускается слепо сразу после warm; mtime/active-set.
6. §12: добавлены тесты cold start без `VectorCache`-скана, failed-build
   retry, byte-cap/`TooLarge`, Windows sync, cleanup race, high-`fetch` recall
   и end-to-end 75k×2048 через `DenseCache`/`VectorCache`/`search_docs`.

## 1. Overview

Добавляем immutable HNSW-индекс поверх уже существующего dense-канала.
ANN не меняет retrieval-контракт: `search_docs_with` по-прежнему получает
`Vec<(chunk_id, score)>` для RRF, а rerank работает по текстам. Меняется
только источник dense-кандидатов: exact scan или HNSW.

Persistence — обязательная часть v1, потому что от неё зависят:
- ключ и формат файлов;
- atomic write и crash recovery;
- загрузка/mmap и время холодного старта;
- политика daemon/CLI и очистка поколений.

Индекс — производный кэш: его можно удалить и пересобрать из `vectors.db`.
`VectorCache` остаётся source of truth для векторов.

## 2. Global constraints

- Rust edition 2024, MSRV 1.88; `cargo fmt`; clippy `--all-targets -D warnings`.
- Новые крейты — только через ADR-15; default build остаётся offline.
- `unwrap`/`expect`/`panic!` запрещены в `src/**`.
- Публичные MCP/CLI-контракты и `search_docs` не меняются.
- `search_docs_with` — единственный seam; direct CLI и daemon используют его.
- ANN — внутренняя оптимизация: ошибка ANN не должна валить поиск.
- Платформы: Linux x86_64, Windows x64, macOS aarch64; little-endian.

## 3. Crate decision (ADR-15)

**Выбрано:** `hnsw_rs` 0.3.5 + `ouroboros` 0.18.5 для owning-wrapper.
Единственная версия v1 — 0.3.5.

Почему:
- pure Rust, без C++/FFI;
- HNSW + `DistCosine`;
- dump/reload и mmap данных;
- активный проект, 1.2M downloads;
- `ouroboros` безопасно хранит `HnswIo` (owner) и `Hnsw` (dependent) без
  `unsafe`; поддерживает `#[borrows(mut io)]` и `#[not_covariant]`.

Ограничения:
- `HnswIo` borrow-ит `Hnsw`; без wrapper хранить загруженный индекс нельзя.
- 0.3.5 содержит graph-correctness фиксы и убирает `bincode`; dump
  формат несовместим между версиями. Для persisted-индекса это нормально: старый dump
  отвергается и пересобирается.
- 0.3.5 fixed seed проверяется двойной single-thread сборкой; для
  `parallel_insert` побайтовая идентичность не обещается.

Альтернативы:
- `hnswlib-rs` 0.10.0 (wilsonzlin) — owned API, `save_to`/`load_from`, seed,
  decoupled `chunk_id` и int8-квантование. Не берём сейчас из-за зрелости:
  6K vs 1.2M загрузок, 11 breaking-релизов за ~14 месяцев, кастомные
  `corenn-kernels`, нет истории mmap/persistence в нашем сценарии. Вернуться
  на итерации квантования, а не сейчас.
- `self_cell` 1.3 — отклонён после spike: `try_new` даёт `&Owner`, а
  `HnswIo::load_hnsw` требует `&mut self`; `Hnsw` инвариантен по lifetime.
- `sqlite-vector-rs` 0.3.1 — `usearch` (C++/FFI), Arrow, SQLite extension;
  несовместим с требованиями static/offline/MSRV.
- `instant-distance` 0.6.1 — archived 2026-07-23, no persistence; отклонён.
- `hnsw` (rust-cv) — последний релиз 2021; отклонён.
- `hannoy`/LMDB — C-зависимость и лишний слой; отклонён.

`hnsw_rs 0.3.5` (2026-10-09) на 2026-10-22 моложе 14 дней. Для v1
используется временный **глобальный** 13-дневный cooldown (2026-10-22 → 23),
затем 14 дней возвращаются шагом T1.5; 0.3.5 остаётся зафиксированным в
`Cargo.lock`. Это workaround: Cargo 1.101 nightly поддерживает только
global/per-registry `min-publish-age`, per-package декларативного allowlist
нет; env-override `CARGO_RESOLVER_INCOMPATIBLE_PUBLISH_AGE=allow` действует
на весь resolve.

## 4. Architecture

```text
search_docs_with
  -> dense_candidates
     -> DenseCache::get(db, project, model_id)
        -> DenseRows::Ann(loaded)   -> loaded.search(query, fetch)
        -> DenseRows::Ready(rows)   -> dense::top_k
        -> DenseRows::TooLarge      -> dense::top_k_iter
```

```text
DenseCache
  +-- AnnStore (paths, metadata, atomic write/load)
  +-- entries: project_id -> Entry { state, last_used, bytes }
  +-- key: (project_id, generation, model_id, dim, chunk_digest)
```

### 4.1 Компоненты

| Компонент | Ответственность |
|---|---|
| `hybrid::ann` | `AnnIndex`/`LoadedAnn`; build, search, mmap load; `ouroboros` wrapper |
| `hybrid::ann_store` | пути, `AnnKey`, `AnnMeta`, atomic write, validation, cleanup |
| `hybrid::dense` | `DenseRows::Ann`, limits, LRU, `TooLarge` hash cache |
| `daemon::tools` | `dense_candidates` match; exact fallback |
| `daemon::server` | build/load policy, warm after sync/backfill, invalidate |
| `cli::search` | `LoadOnly` для direct mode |

### 4.2 `AnnKey`

```rust
struct AnnKey {
    project_id: i64,
    generation: i64,
    model_id: String,
    dim: usize,
    chunk_count: usize,
    chunk_set_digest: String, // blake3(sorted (chunk_id, chunk_hash))
    format_version: u32,
}
```

`model_id` уже содержит provider/model/dim/normalize (FR-5 hybrid-providers).
`chunk_set_digest` — защита от stale generation/backfill; `DenseCache`
кэширует его по `(project_id, index_generation)`. `chunk_hashes`/digest
пересчитываются только при смене generation или явном `invalidate`, не на
каждом запросе. `format_version` входит в ключ; версия `hnsw_rs` живёт в
`AnnMeta` и проверяется при load (FR-3).

## 5. On-disk format

```text
<cache>/projects/<project_id>/ann/
  <model_hash>-g<generation>-d<dim>-c<count>-<digest8>/
    index.hnsw.graph
    index.hnsw.data
    meta.json
    READY
```

`model_hash` — первые 16 hex `blake3(model_id)`. `digest8` — первые 8 hex
`chunk_set_digest`.

`meta.json`:

```json
{
  "format": 1,
  "hnsw_rs": "0.3.5",
  "project_id": 1,
  "generation": 42,
  "model_id": "openrouter/...@d2048@l2",
  "dim": 2048,
  "count": 75000,
  "chunk_set_digest": "blake3...",
  "m": 16,
  "ef_construction": 200,
  "created_at": 1792600000
}
```

`READY` — последний маркер. Без него каталог не считается валидным.

### 5.1 Atomic write

1. Создать `<ann>/tmp-<pid>-<nanos>/`.
2. `Hnsw::file_dump(tmp, "index")`.
3. Записать `meta.json`.
4. Открыть graph/data/meta write-handle и вызвать `File::sync_all` (Windows:
   `FlushFileBuffers` требует `GENERIC_WRITE`; read-only handle не подходит).
5. Записать `READY`.
6. Rename tmp-dir в финальный `<key>/`.

Финал generation-specific, поэтому overwrite не нужен. После crash
`tmp-*` и каталоги без `READY` игнорируются и чистятся.

### 5.2 Load

1. Прочитать `meta.json`; проверить `format`, `hnsw_rs`, `AnnKey`.
2. Проверить размеры файлов (`MAX_ANN_FILE_BYTES`) и `READY`.
3. `HnswIo::new(dir, "index")`; `ReloadOptions::set_mmap(true)`.
4. `load_hnsw::<f32, DistCosine>()`.
5. Обернуть в `ouroboros`.

```rust
#[ouroboros::self_referencing]
struct LoadedAnn {
    io: HnswIo,
    #[borrows(mut io)]
    #[not_covariant]
    index: Hnsw<'this, f32, DistCosine>,
}
```

`HnswIo` обязан жить дольше `Hnsw`; `ouroboros` даёт это без `unsafe`.
Если mmap не удался — retry с `set_mmap(false)`; если и это не удалось —
exact fallback.

## 6. Build

Параметры:

```text
M                = 16
ef_construction  = 200
ef_search        = max(64, fetch * 4) // final; capped by count/recall-vs-ef
ANN_MIN_VECTORS  = 4096        // final v1
MAX_ANN_FILE_BYTES = 2 GiB     // final v1
```

Шаги:
1. `repo::chunk_hashes` → `(chunk_id, hash)`.
2. `VectorCache::get(model_id, hash)` для каждого; если хотя бы одного нет —
   coverage неполное, ANN не строим.
3. Проверить `N >= ANN_MIN_VECTORS` и `estimated_bytes <= max_bytes`.
4. `Hnsw::new(16, n, 16, 200, DistCosine)`.
5. `parallel_insert` — дефолт; serial — только fallback/сравнение. Вставка
   идёт в фоне, в отдельный `Hnsw`, не опубликованный для поиска.
6. После вставки — `set_searching_mode(true)`.
7. `file_dump` → проверить basename `"index"` (иначе fail/новый temp-dir),
   записать metadata + `READY`, atomic rename.

Build выполняется только в daemon (`BuildOrLoad`) или при явном warm после
sync/backfill, **в фоне**. Пока HNSW строится, поиск идёт exact streaming из
`VectorCache` (без `DenseIndex.rows`), чтобы не блокироваться и не держать
rows + новый HNSW одновременно. `docsbase search` direct — `LoadOnly`.

## 7. Search

```rust
fn search(&self, query: &[f32], fetch: usize) -> Result<Vec<(u64, f32)>> {
    // validate dim/non-empty; hnsw.search(query, fetch, ef_search);
    // map Neighbour -> (chunk_id, 1.0 - distance);
    // sort by (distance, chunk_id), truncate(fetch)
}
```

- ANN order влияет только на dense-список RRF; rerank получает тексты.
- Tie-break по `chunk_id` сохраняется.
- `score` из ANN — cosine similarity (для диагностики); RRF использует rank.
  T3 пиннит формулу `score = 1 - distance` тестом на `cos=1/0/-1`; это
  исключает `acos`/угловую меру. Если `DistCosine` вернёт другую шкалу —
  адаптер обязан это исправить, а не маскировать.
- `fetch` остаётся `max(limit, window)`, как в hybrid-providers.
- `ef_search = max(64, fetch * 4)` с cap; recall-vs-ef кривая
  (16/32/64/128/256) проверяет формулу и выбирает cap, а не заменяет её.
- Если ANN вернул пустой/недостаточный список при непустом корпусе, это
  трактуется как сбой ANN и уходит в exact fallback.

## 8. `DenseCache` integration

```rust
pub enum DenseRows {
    Ready(Arc<DenseIndex>),
    Ann(Arc<AnnIndex>),
    TooLarge(Arc<Vec<(u64, String)>>), // hashes cached
    Building(Arc<BuildingAnn>),        // exact streaming while warm
    Failed { retry_at: Instant },      // exact; retry after backoff
}

pub enum AnnPolicy {
    BuildOrLoad, // daemon
    LoadOnly,    // direct CLI
    Disabled,    // tests/explicit fallback
}
```

`DenseCache::get`:

1. Прочитать `generation`; если digest для этой generation уже кэширован —
   использовать его. Иначе один раз вызвать `repo::chunk_hashes`, посчитать
   digest и сохранить в `DenseCache`. На каждом поиске `chunk_hashes` **не**
   вызывается.
2. cache hit с тем же ключом → `Ann`/`Ready`/`TooLarge`/`Building`/`Failed`.
3. Собрать `AnnKey` из generation/model/count/digest и `dim` из model/meta
   (допустимо прочитать один вектор). `AnnStore::load` → `AnnIndex::load` →
   `Ann`. Полный `VectorCache`-скан здесь **не** выполняется.
4. Если `BuildOrLoad`, coverage полное, лимиты ок и ANN ещё нет — запустить
   `warm` в фоне и вернуть `Building`; текущий поиск идёт exact streaming по
   закэшированным hashes. Синхронного build в `get` нет.
5. Если ANN нет/невалиден — exact `Ready` только если entry укладывается в
   `max_bytes`; иначе `TooLarge` (streaming). `max_bytes` — общий резидентный
   cap по всем entries, включая exact rows.
6. Любая ошибка ANN → exact + диагностика; не `Err`. Ошибка фонового build
   переводит entry в `Failed(retry_at)`, логируется, следующий retry — после
   backoff; проект не переводится в error.

Лимиты:

```rust
struct DenseCacheLimits {
    max_vectors: usize,   // 1_000_000 — per-project
    max_bytes: u64,       // 1.5 GiB — global resident cap (Delta 0.3.1/0.3.2)
    max_projects: usize,  // 8
    min_vectors: usize,   // ANN_MIN_VECTORS, internal/test only
}
```

- LRU по `last_used` и по байтам; при превышении `max_bytes` удаляем
  старейшие entries, пока сумма не войдёт в cap.
- `ann.estimated_bytes = count * (dim*4 + graph_overhead)`; exact
  `Ready.bytes = rows * dim * 4`.
- Если один проект сам больше `max_bytes` — `TooLarge`, не `Ready`.
- `TooLarge` хранит hashes, чтобы не вызывать `repo::chunk_hashes` каждый
  запрос.
- `DenseIndex` rows не держим одновременно с `Ann`: это две копии векторов.
- `DenseCache::warm(project_id, model_id, dim, key, hashes)` идемпотентен:
  один build на `(project, generation, model)`; результат публикуется только
  после `READY` и проверки, что generation/model всё ещё актуальны.
- `DenseRows::Building` хранит hashes, статус и **полный** учёт памяти:
  оценка графа плюс exact-rows, которые остаются живы в треде сборки
  (Delta 0.3). `enforce_byte_cap` видит эту сумму, поэтому сборка не может
  вывести проект за `max_bytes`; `dense_candidates` идёт через `top_k_iter`
  (exact streaming), без `DenseIndex.rows`.
- Release-профиль не может включать `panic = "abort"`: `catch_unwind` вокруг
  `hnsw_rs` — это и есть FR-5/SC-4 seam, а с abort паника парсера дампа
  становится abort'ом процесса. Инвариант закреплён const-assert в
  `src/hybrid/ann.rs` и release-тестом в CI (Delta 0.3).
- Резидентный кап `DenseCacheLimits::max_bytes` (дефолт 1.5 GiB, Delta 0.3.1)
  покрывает пик сборки: `ann_estimated_bytes + exact_rows` (1.22 GiB на
  75k×2048). Admission проверяется общей функцией `ann_build_admitted`, её же
  использует пин backfill-лиза.
- `DenseRows::Failed { retry_at }` хранит hashes/статус; search стримит exact,
  retry запускает новый build после `retry_at` (не переиспользует старый
  `Building`).

## 9. Invalidation / lifecycle

- `index_generation` change → другой `AnnKey`.
- model/dim change → другой `AnnKey`.
- backfill завершён → key-based invalidation + `warm`: stale on-disk indexes
  отвергаются по generation/digest/format, старый ANN не используется.
  Явный `DenseCache::invalidate(project_id)` остаётся для удаления проекта
  (вызывающего пути в коде пока нет; backfill инвалидируется ключом).
- format/hnsw version mismatch → rebuild.
- corrupt/missing files → exact; daemon может rebuild.
- старые generation-каталоги: keep last 2 **на model**, cleanup best-effort;
  глобальный disk budget ограничивает сумму, вытесненные проекты чистятся.
- cleanup не удаляет `tmp-*` активного build: только stale по mtime или
  отсутствующие в active-set.
- Windows: не удалять mmap-файлы, пока `HnswIo` жив; cleanup после drop.

## 10. Direct CLI / daemon

- Daemon: `AnnPolicy::BuildOrLoad`; после `run_sync_job`/backfill —
  `DenseCache::warm(project, model)` **в фоне**. Пока build не завершён,
  `search_docs` идёт exact; ошибка build логируется, проект не становится
  error.
- Cleanup после warm запускается только с guard-ом (mtime/active-set) и не
  удаляет `tmp-*` строящегося индекса.
- `docsbase search` direct: `AnnPolicy::LoadOnly`; если persisted ANN нет —
  exact. Это защищает одноразовый процесс от build latency.
- Если daemon запущен, CLI идёт через него и получает ANN.

## 11. Diagnostics

Should (FR-9):
- `status.projects[].ann`: `{state: "disabled"|"loading"|"ready"|"stale"|"fallback", count, bytes}`.
- Лог: `ann load/build failed: ...; falling back to exact`.
- Никаких текстов документов/ключей в логах.

Если `status` расширять не хочется, на первом этапе достаточно лога; но
состояние должно быть наблюдаемым.

## 12. Tests

Unit:
- `ann_build_load_roundtrip` (dump/load, same neighbors).
- `ann_mmap_and_heap_load`.
- `ann_rejects_dim_mismatch` / empty query.
- `ann_metadata_mismatch_rejected`.
- `ann_corrupt_file_falls_back`.
- `ann_atomic_ready_marker_required`.
- `ann_lru_evicts_old_project`.
- `ann_too_large_does_not_allocate_rows`.
- `ann_direct_cli_never_builds`.
- `ann_034_dump_rejected_by_035` (metadata/format gate; no panic).
- `ann_search_during_build_uses_exact` (задержанная сборка, поиск не блокируется).
- `ann_score_is_one_minus_cos` (cos=1/0/-1).
- `ann_recall_ef_curve` (16/32/64/128/256, recall@10 + p95; plus fetch
  10/50/100/1000).
- `ann_build_is_nonblocking` (warm в фоне; ошибка не валит поиск).
- `ann_failed_build_retries_after_backoff` (не вечный `Building`).
- `ann_cold_start_does_not_scan_vectors` (persisted ANN без `VectorCache`).
- `ann_byte_cap_streams_exact_rows` (`TooLarge`, не `Ready`).
- `ann_windows_sync_uses_write_handle` (runtime, Windows CI).
- `ann_cleanup_skips_active_tmp` (delayed build + cleanup).
- `ann_high_fetch_recall` (`limit=100/1000`, dynamic ef).

Integration:
- `hybrid_ranking` with `ann_threshold=1`: exact vs ANN order on separated
  vectors; RRF tie-break.
- `search_bench`: unchanged for 331 chunks; ANN threshold > corpus.
- `ann_e2e` (`#[ignore]`): 75k × 2048 через `DenseCache`/`VectorCache`/
  `search_docs`; build/query p50/p95; recall@10; real RSS; persisted reload;
  coverage=100%.

CI:
- `cargo test --locked`, clippy, fmt; Linux/macOS/Windows; MSRV 1.88.
- `cargo tree` default без `reqwest`; no network.

## 13. Acceptance mapping

| SC | Test/measure |
|---|---|
| SC-1 | `ann_e2e` end-to-end через `DenseCache`/`VectorCache`; recall/latency/build; manual report |
| SC-2 | `search_bench` baseline unchanged; threshold test |
| SC-3 | dump → drop → load; no rebuild; metadata/mtime; zero `VectorCache::get` |
| SC-4 | stale/corrupt/missing tests + exact fallback |
| SC-5 | atomic write/crash test (`READY` absent) |
| SC-6 | global byte-cap/LRU; exact rows → `TooLarge` |
| SC-7 | `metrics_match_baseline` unchanged |
| SC-8 | `cargo tree`, offline test |
| SC-9 | CI matrix + MSRV |
| SC-10 | docs/MCP contract diff empty |
| SC-11 | search during delayed build; exact response, no block |
| SC-12 | 0.3.5 roundtrip; dump чужой версии rejected; fixed-seed double build |

## 14. Risks / mitigations

| Risk | Mitigation |
|---|---|
| `HnswIo` lifetime | `ouroboros` wrapper; spike on Linux + cross-check |
| Random HNSW build | persisted result; recall gate, no byte determinism |
| Windows mmap deletion | generation dirs; cleanup after drop |
| Memory blow-up | byte cap; no rows + Ann together; LRU |
| Corrupt dump | metadata/READY/size checks; exact fallback |
| Format drift | `format_version` + rebuild |
| Build latency spikes | parallel insert default; background warm; exact streaming; CLI LoadOnly |
| New deps | ADR-15; `cargo tree`; MSRV; no C++/FFI; env_logger size measured |
| Version/format drift | 0.3.5 v1; dump чужой версии rejected; rebuild, no migration |
| Build blocks search | publish after `READY`; exact streaming during build |
| ANN changes ranking | threshold > golden corpus; recall test; baseline unchanged |

## 15. Decisions

1. `ANN_MIN_VECTORS = 4096`, `max_bytes = 1.5 GiB` (Delta 0.3.1), `max_projects = 8`;
   `ef_search = max(64, fetch * 4)` с cap (Delta 0.2, T6).
2. `status` vs log: v1 — log-only; публичный контракт не меняется (T5).
3. `ef_search` — dynamic formula выше; high-`fetch` recall проверяется T8/T10.
4. Детерминизм `parallel_insert`: seed API нет; побайтовая идентичность не
   обещается, детерминизм запросов обязателен (T6).
5. 0.3.5 finalization — закрыто T6; T1.5 остаётся merge gate для cooldown.
6. Windows runtime — открыто до T9 (OQ-4).
7. `VectorCache` capacity/pinning — `hybrid-providers` DELTA-9, задача T13.

## 16. References

- `docs/specs/hybrid-providers/design.md` — dense/RRF, `DenseCache`.
- `docs/specs/hybrid-providers/progress.md` — accepted minors.
- `src/hybrid/dense.rs`, `src/daemon/tools.rs`, `src/vector_cache.rs`,
  `src/store/repo.rs`.
- `hnsw_rs` 0.3.5 docs: `Hnsw`, `HnswIo`, `ReloadOptions`, `DistCosine`,
  `AnnT::file_dump`; `Hnsw::search` (`ef` vs `knbn`).
- `ouroboros` 0.18.5 docs.
- Windows `FlushFileBuffers`: требует `GENERIC_WRITE`
  (<https://learn.microsoft.com/en-us/windows/win32/api/fileapi/nf-fileapi-flushfilebuffers>).
