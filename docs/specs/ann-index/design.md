# Technical Design: ann-index (итерация 3 hybrid-providers)

**Status:** Draft, ревизия 0.1 · **Дата:** 2026-10-22
**Spec:** `docs/specs/ann-index/requirements.md` (ревизия 0.1)
**Связано:** `docs/specs/hybrid-providers/design.md` (dense + RRF), ADR-12,
ADR-14; **ADR-15 (proposed)** — ANN-зависимость и формат.

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

10k × 64: build 5.1 s, dump 18 ms, mmap/heap load 49 ms, top-10 совпал;
`LoadedAnn: Send + Sync`. Windows/macOS — `cargo check`, runtime в CI.

**Delta T1.1 (2026-10-22, ревью):** v1 — `hnsw_rs 0.3.5` (0.3.4 fallback),
временный 13-дневный cooldown; dump 0.3.4 отвергается, не мигрируется.
`parallel_insert` — дефолт, warm — фоновый, exact streaming во время сборки.
Добавлены `usize`/`u64`, `set_searching_mode`, basename, env_logger size,
`DistCosine`/recall-ef. ADR-15 — после T6.

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

## 3. Crate decision (ADR-15, proposed)

**Выбрано:** `hnsw_rs` 0.3.5 + `ouroboros` 0.18.5 для owning-wrapper.
0.3.4 — только fallback, если T3 на 0.3.5 не пройдёт.

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
  0.3.4/0.3.5 несовместим. Для persisted-индекса это нормально: старый dump
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
`chunk_set_digest` — защита от stale generation/backfill; считается из
`repo::chunk_hashes` только при build/load, не на каждом запросе.
`format_version` входит в ключ; версия `hnsw_rs` живёт в `AnnMeta` и
проверяется при load (FR-3).

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
4. `File::sync_all` для graph/data/meta.
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
ef_search        = max(64, fetch * 4) // provisional; final by recall-vs-ef
ANN_MIN_VECTORS  = 4096        // provisional
MAX_ANN_FILE_BYTES = 2 GiB     // provisional
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
- `ef_search` выбирается по recall-vs-ef кривой (16/32/64/128/256), а не
  только по эвристике `max(64, fetch*4)`.
- Если ANN вернул пустой/недостаточный список при непустом корпусе, это
  трактуется как сбой ANN и уходит в exact fallback.

## 8. `DenseCache` integration

```rust
pub enum DenseRows {
    Ready(Arc<DenseIndex>),
    Ann(Arc<LoadedAnn>),
    TooLarge(Arc<Vec<(u64, String)>>), // hashes cached
}

pub enum AnnPolicy {
    BuildOrLoad, // daemon
    LoadOnly,    // direct CLI
    Disabled,    // tests/explicit fallback
}
```

`DenseCache::get`:

1. `generation`, `chunk_hashes`, digest.
2. cache hit с тем же ключом → `Ann`/`Ready`/`TooLarge`.
3. `AnnStore::load` → `Ann`.
4. Если `BuildOrLoad`, coverage полное, лимиты ок → build + persist → `Ann`.
5. Иначе exact `Ready` (если под cap) или `TooLarge`.
6. Любая ошибка ANN → exact + диагностика; не `Err`.

Лимиты:

```rust
struct DenseCacheLimits {
    max_vectors: usize,   // 1_000_000
    max_bytes: u64,       // 1 GiB
    max_projects: usize,  // 8
}
```

- LRU по `last_used`; при превышении удаляем старейший проект.
- `ann.estimated_bytes = count * (dim*4 + graph_overhead)`.
- `TooLarge` хранит hashes, чтобы не вызывать `repo::chunk_hashes` каждый
  запрос.
- `DenseIndex` rows не держим одновременно с `Ann`: это две копии векторов.

## 9. Invalidation / lifecycle

- `index_generation` change → другой `AnnKey`.
- model/dim change → другой `AnnKey`.
- backfill завершён → `DenseCache::invalidate(project_id)`.
- format/hnsw version mismatch → rebuild.
- corrupt/missing files → exact; daemon может rebuild.
- старые generation-каталоги: keep last 2, cleanup best-effort.
- Windows: не удалять mmap-файлы, пока `HnswIo` жив; cleanup после drop.

## 10. Direct CLI / daemon

- Daemon: `AnnPolicy::BuildOrLoad`; после `run_sync_job`/backfill —
  `DenseCache::warm(project, model)` **в фоне**. Пока build не завершён,
  `search_docs` идёт exact; ошибка build логируется, проект не становится
  error.
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
- `ann_recall_ef_curve` (16/32/64/128/256, recall@10 + p95).
- `ann_build_is_nonblocking` (warm в фоне; ошибка не валит поиск).

Integration:
- `hybrid_ranking` with `ann_threshold=1`: exact vs ANN order on separated
  vectors; RRF tie-break.
- `search_bench`: unchanged for 331 chunks; ANN threshold > corpus.
- `ann_scale` (`#[ignore]`): 75k × 2048; build/query p50/p95; recall@10;
  estimated memory; persisted reload.

CI:
- `cargo test --locked`, clippy, fmt; Linux/macOS/Windows; MSRV 1.88.
- `cargo tree` default без `reqwest`; no network.

## 13. Acceptance mapping

| SC | Test/measure |
|---|---|
| SC-1 | `ann_scale` recall/latency/build; manual report |
| SC-2 | `search_bench` baseline unchanged; threshold test |
| SC-3 | dump → drop → load; no rebuild; metadata/mtime |
| SC-4 | stale/corrupt/missing tests + exact fallback |
| SC-5 | atomic write/crash test (`READY` absent) |
| SC-6 | LRU/byte-cap tests |
| SC-7 | `metrics_match_baseline` unchanged |
| SC-8 | `cargo tree`, offline test |
| SC-9 | CI matrix + MSRV |
| SC-10 | docs/MCP contract diff empty |
| SC-11 | search during delayed build; exact response, no block |
| SC-12 | 0.3.5 roundtrip; 0.3.4 dump rejected; fixed-seed double build |

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
| Version/format drift | 0.3.5 v1; 0.3.4 dump rejected; rebuild, no migration |
| Build blocks search | publish after `READY`; exact streaming during build |
| ANN changes ranking | threshold > golden corpus; recall test; baseline unchanged |

## 15. Open decisions

1. `ANN_MIN_VECTORS` / `max_bytes` / `max_projects` (T6).
2. `status` vs log only (FR-9).
3. `ef_search` по recall-vs-ef кривой; окончательное значение — T6.
4. Детерминизм `parallel_insert` (fixed seed) — T6; если недетерминирован,
   это допустимо для persisted-индекса.
5. 0.3.5 vs fallback 0.3.4 — условное решение; финализация в T6.

## 16. References

- `docs/specs/hybrid-providers/design.md` — dense/RRF, `DenseCache`.
- `docs/specs/hybrid-providers/progress.md` — accepted minors.
- `src/hybrid/dense.rs`, `src/daemon/tools.rs`, `src/vector_cache.rs`,
  `src/store/repo.rs`.
- `hnsw_rs` 0.3.5 docs: `Hnsw`, `HnswIo`, `DistCosine`, `AnnT::file_dump`.
- `ouroboros` 0.18.5 docs.
