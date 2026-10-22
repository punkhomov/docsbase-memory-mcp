# Tasks: ann-index (initial plan)

**Goal:** добавить persisted HNSW для dense-канала RRF, сохранив exact fallback
и текущие публичные контракты.
**Architecture:** `src/hybrid/{ann,ann_store}.rs` + расширение `DenseCache`;
`hnsw_rs 0.3.5` + `ouroboros 0.18.5`; 0.3.5 — единственная версия; daemon строит/загружает,
direct CLI только загружает.
**Spec:** `docs/specs/ann-index/requirements.md` (0.2)
**Design:** `docs/specs/ann-index/design.md` (0.2)
**Status:** T1 complete; T2–T7 complete (2026-10-22); T1.5 deferred by human
(13-day cooldown stays in effect until scheduled); T8 complete; T9 code and T10 (8k CI + 75k manual e2e) complete; T11 close-out/T1.5 and Windows CI evidence remain.
Числа T6 — core-`AnnIndex`, end-to-end подтверждает T10.

**Global constraints:**
- Rust edition 2024, MSRV 1.88; `cargo fmt`; clippy `--all-targets -- -D warnings`.
- Новые крейты — только через ADR-15; default build остаётся offline.
- В `src/**` нет `unwrap`/`expect`/`panic!`.
- MCP/CLI/`search_docs` контракты не меняются.
- ANN — производный кэш; `vectors.db` — source of truth.
- Golden baseline не трогаем: 331 чанк < `ANN_MIN_VECTORS`.

**Review focus:**
1. Persistence-контракт: ключ, metadata, `READY`, atomic write, stale/corrupt.
2. Exact fallback на любой ошибке ANN; поиск не падает.
3. Direct CLI не строит HNSW; daemon строит/загружает.
4. Mmap/Windows lifecycle и очистка generation-каталогов.
5. Recall/latency измерены, а не заявлены; baseline не изменился.

---

## T1 — Spike: `hnsw_rs` + owning-wrapper (persistence + mmap)

**Status:** complete (2026-10-22), spike in `/tmp/opencode/ann-spike` (not committed).

**Goal:** до интеграции доказать, что выбранный wrapper компилируется на MSRV
1.88, пишет/читает dump, работает с mmap и не ломает Windows/macOS.

**Files:**
- `/tmp/opencode/ann-spike` (temporary; no repo dependency yet).
- Later T2: `Cargo.toml`/`Cargo.lock` after ADR-15.

**Steps (spike, not production):**
1. `hnsw_rs = "=0.3.5"` + `ouroboros = "0.18.5"`.
2. `Hnsw::new(16, n, 16, 200, DistCosine)`, insert 10k × 64.
3. `file_dump`; `ouroboros` load; `mmap=true` и `mmap=false`.
4. `Send + Sync` wrapper; drop/cleanup; Windows/macOS `cargo check`.
5. Замерить build/load; записать факты в `progress.md`.

**Result:** top-10 совпал; build 4.8 s, dump 15 ms, load 43/42 ms (mmap/heap),
10.3 MB dump, `LoadedAnn: Send + Sync`. `self_cell` не подошёл
(`&mut HnswIo` + invariant `Hnsw`); `ouroboros` работает.

**Acceptance:** dump/load даёт те же соседи; mmap работает; MSRV/CI
компилируются; известен точный wrapper-код. **PASS.**

**Verify:** temp crate `cargo test -- --nocapture`; `cargo check` на MSRV 1.88
для Linux/Windows/macOS targets.

**Depends:** OQ-1/OQ-2. **Blocks:** T2, T3.

---

## T1.5 — Revert temporary 13-day cooldown (2026-10-23)

**Goal:** вернуть supply-chain policy к 14 дням после того, как `hnsw_rs 0.3.5`
зафиксирован в `Cargo.lock`; 0.3.5 при этом не откатывается.

**Why:** Cargo 1.101 nightly поддерживает `min-publish-age` только глобально
или per-registry; per-package декларативного allowlist нет. Поэтому 13 дней —
временный глобальный workaround, а не новая policy. `CARGO_RESOLVER_INCOMPATIBLE_PUBLISH_AGE=allow`
тоже не per-package (отключает проверку на весь resolve), поэтому не
используется как постоянное решение.

**Files:**
- Modify: `.cargo/config.toml` (`13 days` → `14 days`; убрать temporary comment).
- Modify: `tests/smoke.rs` (ожидание `14 days`).
- Modify: `docs/supply-chain.md` (убрать/переписать temporary note).
- Modify: `docs/specs/ann-index/progress.md` (revert entry).

**Steps:**
1. Убедиться, что `Cargo.lock` содержит `hnsw_rs 0.3.5` и lock audit пройден.
2. Вернуть `14 days` в config/test; обновить docs note.
3. `cargo test --locked --test smoke min_publish_age_configured`.
4. Записать `T1.5: complete (revert 14 days; 0.3.5 pinned)`.

**Acceptance:** config/test/docs снова 14 days; `hnsw_rs 0.3.5` остаётся в
lock; temporary note удалён; smoke test зелёный.

**Depends:** T1, successful 0.3.5 lock update. **Blocks:** final PR.

---

## T2 — `AnnStore`: paths, key, metadata, atomic write/load

**Goal:** безопасный persistence-слой без HNSW-логики.

**Files:**
- Create: `src/hybrid/ann_store.rs`.
- Modify: `src/hybrid/mod.rs` (`pub mod ann_store;`).
- Test: `tests/ann_store.rs`.

**Interfaces:**
```rust
struct AnnKey { project_id, generation, model_id, dim, chunk_count,
                chunk_set_digest, format_version }
struct AnnMeta { format, hnsw_rs_version, key..., count, m, ef_construction, created_at }
struct AnnSlot { dir: PathBuf, meta: AnnMeta }
struct AnnStore { root: PathBuf }
impl AnnStore {
    fn load(&self, key: &AnnKey) -> Result<Option<AnnSlot>>;
    fn persist_dump(&self, key: &AnnKey, write: impl FnOnce(&Path) -> Result<String>)
        -> Result<AnnSlot>;
    fn invalidate(&self, project_id: i64);
    fn cleanup(&self, project_id: i64);
}
```

**Steps (TDD):**
1. RED: metadata missing/mismatch → `None`/stale; no `READY` → invalid;
   corrupt file → exact fallback; atomic temp not visible.
2. GREEN: path/key, `meta.json`, `READY`, temp+rename, size checks.
3. Add `chunk_set_digest` helper (blake3 over sorted `(chunk_id, hash)`).
4. Tests: stale generation/digest, format mismatch, `hnsw_rs_version`
   mismatch, cleanup old generations; `file_dump` basename is `"index"` (or
   fresh temp-dir on collision).

**Acceptance:** FR-2/FR-3/FR-5; SC-4/SC-5; no HNSW build logic in this task:
`persist_dump` only provides temp dir/atomic publish; `AnnIndex` writes the dump
in T3.

**Verify:** `cargo test --locked --test ann_store`;
`cargo clippy --locked --all-targets -- -D warnings`.

**Depends:** T1.

---

## T3 — `AnnIndex`: build/search + `LoadedAnn`

**Goal:** HNSW-обёртка с `DistCosine`, recall-тестами и безопасным load.

**Files:**
- Create: `src/hybrid/ann.rs`.
- Modify: `src/hybrid/mod.rs`.
- Test: `tests/ann_recall.rs`, `tests/ann_spike.rs`.

**Interfaces:**
```rust
struct AnnIndex { /* LoadedAnn or in-memory Hnsw */ }
impl AnnIndex {
    fn build(rows: &[(u64, Arc<[f32]>)], dim: usize) -> Result<Self>;
    fn search(&self, query: &[f32], fetch: usize) -> Result<Vec<(u64, f32)>>;
    fn estimated_bytes(&self) -> usize;
}
```

**Steps (TDD):**
1. RED: dim mismatch/empty query; deterministic tie-break; recall@10 ≥ 0.99
   on separated vectors; `score = 1 - cos` for cos=1/0/-1.
2. GREEN: `hnsw_rs 0.3.5`; `parallel_insert` default; `set_searching_mode(true)`;
   `AnnIndex::dump` через `AnnStore::persist_dump`; `AnnIndex::load(AnnSlot)`;
   `ouroboros` load; mmap/heap fallback. Map `chunk_id` via
   `usize::try_from`; `Neighbour.d_id` via `u64::try_from`; no `as`.
3. Add synthetic recall test 5k × 64 in CI; 75k × 2048 ignored scale;
   recall-vs-ef curve 16/32/64/128/256.
4. Verify dump чужой версии rejected (version/format metadata) without
   panic; verify double single-thread build with fixed seed.
5. Measure build on 0.3.5; version fallback не предусмотрен, только
   exact fallback при ошибке ANN.

**Acceptance:** FR-1/FR-5/FR-13; NFR-1/2; SC-1/SC-12; OQ-2 остаётся
условным до T6; no `DenseCache` changes yet.

**Verify:** `cargo test --locked --test ann_recall`;
`cargo test --locked --test ann_spike -- --ignored` (manual scale).

**Depends:** T1, T2.

---

## T4 — `DenseCache` integration + fallback + limits

**Goal:** дать поиску `DenseRows::Ann`, сохранить exact/TooLarge и ввести
LRU/byte-cap.

**Files:**
- Modify: `src/hybrid/dense.rs` (`DenseRows::Ann`, limits, LRU,
  `TooLarge` hash cache).
- Modify: `src/daemon/tools.rs` (`dense_candidates` match).
- Test: `tests/hybrid_ranking.rs`, `tests/hybrid_ann.rs`.

**Steps (TDD):**
1. RED: below threshold exact unchanged; above threshold ANN; ANN error →
   exact; coverage incomplete → exact; stale key → exact/rebuild.
2. GREEN: `DenseCacheLimits` (`max_vectors`, `max_bytes`, `max_projects`);
   digest cache по `(project, generation)`; `AnnPolicy::{BuildOrLoad, LoadOnly,
   Disabled}`; `DenseRows::Building`.
3. `warm` запускает один фоновый build; на время build `dense_candidates`
   использует `top_k_iter` и cached hashes. Cache `TooLarge` hashes; no
   per-query `repo::chunk_hashes`.
4. Tests: digest не пересчитывается на hit; LRU, byte-cap, no rows+Ann double
   memory, `TooLarge` no allocation, `Building` не блокирует.

**Acceptance:** FR-1/FR-4/FR-6/FR-7; SC-2/SC-6; `search_bench` unchanged.

**Verify:** `cargo test --locked --test hybrid_ranking --test hybrid_ann`;
`cargo test --locked --test search_bench`.

**Depends:** T3.

---

## T5 — Daemon/CLI policy + warm/invalidate

**Goal:** daemon строит/загружает, direct CLI только загружает; backfill
инвалидирует.

**Files:**
- Modify: `src/cli/search.rs` (`LoadOnly`).
- Modify: `src/daemon/server.rs` (`BuildOrLoad`, warm after sync/backfill).
- Modify: `src/daemon/tools.rs` (invalidate hook).
- Test: `tests/hybrid_ranking.rs`, `tests/cli_routing.rs`/`tests/mcp_frontend.rs`.

**Steps (TDD):**
1. RED: direct CLI без persisted ANN не строит; daemon строит; после
   `embed_project_chunks` старый ANN не используется.
2. GREEN: `AnnPolicy`; `DenseCache::invalidate`; `DenseCache::warm` after sync
   **в фоне**; `Building` → exact streaming.
3. Status/log diagnostics (FR-9, should) — если решено в T5.
4. Tests: no build in CLI; build in daemon; warm; stale after backfill;
   поиск во время build не блокируется и идёт exact.

**Acceptance:** FR-6/FR-8/FR-9/FR-12; SC-3/SC-11.

**Verify:** targeted tests + `cargo test --locked`.

**Depends:** T4.

---

## T6 — Scale/recall/golden verification

**Goal:** измерить эффект и зафиксировать пороги; не менять baseline.

**Files:**
- Create: `tests/ann_scale.rs` (`#[ignore]`).
- Maybe modify `tests/search_bench.rs` threshold assertion only (no baseline).
- Update `docs/specs/ann-index/progress.md`.

**Steps:**
1. 5k × 64 CI recall test; 75k × 2048 ignored scale test.
2. Measure build (parallel default), p50/p95, exact vs ANN, recall@10,
   recall-vs-ef curve (16/32/64/128/256), estimated bytes.
3. Verify persisted reload (no rebuild), LRU, and non-blocking warm.
4. Verify 0.3.5 dump/load, fixed-seed double single-thread build, and
   refusal of a foreign version/format; parallel determinism as separate
   observation.
5. Measure release/static binary size delta (`env_logger`); 40 MiB budget.
6. Run `search_bench`; baseline must be unchanged (ANN not active).
7. Record numbers; adjust `ANN_MIN_VECTORS`/caps/`ef_search` only with ruling.

**Acceptance:** SC-1/SC-2/SC-3/SC-7/SC-11/SC-12; numbers written to ledger;
OQ-2 finalized and ADR-15 version pinned here.

**Verify:**
```sh
RUSTUP_TOOLCHAIN=1.88.0 cargo test --locked --test ann_recall
RUSTUP_TOOLCHAIN=1.88.0 cargo test --locked --test ann_scale -- --ignored
RUSTUP_TOOLCHAIN=1.88.0 cargo test --locked --test search_bench
```

**Depends:** T5.

---

## T7 — ADR-15 finalization, docs, ledger

**Goal:** зафиксировать выбор, контракт и ограничения; обновить spec relations.

**Files:**
- Create: `docs/specs/ann-index/{requirements,design,tasks,progress}.md`
  (already draft; finalize).
- Modify: `docs/specs/docsbase-memory-mcp/design.md` (finalize ADR-15
  version/format; dependency table already updated).
- Modify: `docs/specs/hybrid-providers/requirements.md`/`design.md` (dated
  delta: ANN moved to iteration 3).
- Modify: `CHANGELOG.md` (performance) if user-visible.
- Maybe `docs/CONFIG.md` only if a user-facing knob is added (not planned).

**Steps:**
1. Finalize ADR-15: version/format, alternatives, MSRV, persistence,
   binary size delta; pin `hnsw_rs 0.3.5` / `ouroboros` (no fallback version).
2. Add dependency table row; update "later" notes.
3. Ledger: `Task T1..T7 complete (...)`.
4. Docs check: no contradictions about ANN being non-goal.

**Acceptance:** docs CI green; no stale "ANN later" statement without link to
this spec.

**Verify:** docs checks; `cargo fmt`; `cargo test --locked`.

**Depends:** T1–T6.

---

## T8 — DenseCache byte-cap + failed-build retry (Delta 0.2)

**Goal:** закрыть FR-7/NFR-4 и FR-5/FR-12: `max_bytes` ограничивает весь
резидентный working set, failed build не залипает в `Building`.

**Files:**
- Modify: `src/hybrid/dense.rs`.
- Test: `tests/hybrid_ann.rs`, unit-тесты в `dense.rs`.

**Steps (TDD):**
1. RED: `max_bytes` меньше exact rows → ожидать `TooLarge`, не `Ready`;
   failed build → после backoff должен быть новый spawn.
2. GREEN: считать `Entry.bytes` для `Ann`/`Ready`/`TooLarge`/`Building`;
   вести общий byte-budget и LRU по байтам; один проект сверх cap → `TooLarge`.
3. Ввести `Failed { retry_at }` (или эквивалент); `start_build` переиспользует
   entry только если build реально в полёте; `warm` ретраит после backoff.
4. Убрать двойной полный `resolve`-скан после истечения backoff.

**Acceptance:** FR-7/FR-12; SC-6/SC-11; нет вечного `Building`; `TooLarge`
стримит exact; byte-LRU покрыт тестом.

**Verify:** `cargo test --locked --test hybrid_ann`; targeted unit-тесты;
`cargo clippy --locked --all-targets -- -D warnings`.

**Depends:** T4/T5.

---

## T9 — Cold start + Windows persistence + cleanup (Delta 0.2)

**Goal:** закрыть FR-8/NFR-3 (ANN-first cold start), FR-11/NFR-5 (Windows sync),
FR-7/lifecycle (cleanup race, model-aware retention, disk budget).

**Files:**
- Modify: `src/hybrid/dense.rs`, `src/hybrid/ann_store.rs`,
  `src/daemon/server.rs`.
- Test: `tests/ann_store.rs`, `tests/hybrid_ann.rs`, daemon unit/E2E.

**Steps (TDD):**
1. RED: persisted ANN + пустой/удалённый `vectors.db` → `get` возвращает
   `Ann`, а не exact; cleanup во время delayed build не убивает tmp.
2. GREEN: `resolve` собирает key/`dim` из model/meta (или одного вектора) и
   грузит ANN до rows; rows читаются только на miss/build.
3. `sync_dump_files` открывает graph/data/meta write-handle; Windows runtime
   тест; READY/dir sync — по решению T9.
4. Cleanup: mtime/active-set для tmp; retention per model; глобальный disk
   budget; вытесненные проекты чистятся.
5. Убрать/переписать `refresh_ann_after_sync` cleanup так, чтобы не было гонки.

**Acceptance:** FR-8/NFR-3/FR-11/NFR-5/FR-7; SC-3/SC-6/SC-9; cold start без
полного vector scan; Windows `ann_store` зелёный.

**Verify:** targeted tests + Windows CI; `cargo test --locked --test ann_store`;
`cargo test --locked --test hybrid_ann`.

**Depends:** T8, hybrid-providers T13.

---

## T10 — End-to-end scale + dynamic ef (Delta 0.2)

**Goal:** доказать 75k×2048 через реальный путь `DenseCache`/`VectorCache`/
`search_docs`, а не только `AnnIndex`; закрыть SC-1/SC-3/SC-11.

**Files:**
- Modify: `src/hybrid/ann.rs` (`ef = max(64, fetch*4)` с cap).
- Create/modify: `tests/ann_scale.rs` (ignored manual) и CI-тест 8k×256.
- Update: `docs/specs/ann-index/progress.md`.

**Steps:**
1. Dynamic `ef` + high-`fetch` recall (10/50/100/1000).
2. CI-тест 8k×256 через `DenseCache` + `VectorCache`: coverage, ANN, cold
   start, fallback.
3. Manual 75k×2048: build/load/query p50/p95, recall, real RSS, persisted
   reload; coverage=100%; `search_bench` baseline unchanged.
4. Записать числа и ruling в ledger; T6 core-числа оставить как core-only.

**Acceptance:** SC-1/SC-3/SC-6/SC-11; end-to-end numbers recorded; no baseline
change.

**Verify:**
```sh
RUSTUP_TOOLCHAIN=1.88.0 cargo test --locked --test ann_recall
RUSTUP_TOOLCHAIN=1.88.0 cargo test --locked --test ann_scale -- --ignored
RUSTUP_TOOLCHAIN=1.88.0 cargo test --locked --test search_bench
```

**Depends:** T8, T9, hybrid-providers T13.

---

## T11 — Delta 0.2 close-out + merge gate

**Goal:** привести спеку/доки/CHANGELOG к фактическому состоянию после T8–T10
и проверить merge gate.

**Files:**
- Modify: `docs/specs/ann-index/{requirements,design,tasks,progress}.md`,
  `docs/specs/docsbase-memory-mcp/design.md`, `CHANGELOG.md`,
  `docs/CONFIG.md` (если появился cache-key).
- Verify: T1.5 (14 days), Windows CI, full targeted suite.

**Steps:**
1. Закрыть OQ-4 после Windows runtime; подтвердить остальные OQ.
2. Убрать противоречия «exact scan remains»/`invalidate` vs warm-only.
3. CHANGELOG: core vs end-to-end числа; без «near-instant» до T10.
4. T1.5: вернуть 14 days, обновить smoke/docs; 0.3.5 остаётся в lock.
5. Fresh whole-branch reviewer; ledger entry; статус Final только после
   зелёных T8–T10 и T1.5.

**Acceptance:** docs не противоречат коду; T1.5 выполнен; targeted suite и
Windows CI зелёные; delta 0.2 закрыта.

**Depends:** T8–T10, hybrid-providers T13, T1.5.

---

## Definition of done

- `hnsw_rs 0.3.5` + `ouroboros` compile on MSRV 1.88 and all CI platforms;
  Windows runtime (sync/mmap/cleanup) actually green.
- Persisted ANN: atomic write, metadata, `READY`, writable sync, load,
  stale/corrupt fallback.
- Cold start loads persisted ANN before any full vector scan.
- `DenseCache`: `Ann`, global byte-cap/LRU incl. exact rows, `TooLarge` hash
  cache, failed-build retry after backoff.
- Daemon build/load; direct CLI load-only; backfill key-based invalidation +
  warm; no `tmp-*` race; model-aware cleanup and disk budget.
- 75k×2048 end-to-end через `DenseCache`/`VectorCache`/`search_docs`:
  coverage=100%, recall/latency/RSS recorded; core `AnnIndex` numbers labelled
  separately.
- `search_bench` baseline unchanged; no MCP/CLI changes.
- T1.5 done (14 days), ADR-15 + delta 0.2 + docs + ledger complete.
