# Tasks: ann-index (initial plan)

**Goal:** добавить persisted HNSW для dense-канала RRF, сохранив exact fallback
и текущие публичные контракты.
**Architecture:** `src/hybrid/{ann,ann_store}.rs` + расширение `DenseCache`;
`hnsw_rs 0.3.5` + `ouroboros 0.18.5`; 0.3.5 — единственная версия; daemon строит/загружает,
direct CLI только загружает.
**Spec:** `docs/specs/ann-index/requirements.md` (0.1)
**Design:** `docs/specs/ann-index/design.md` (0.1)
**Status:** initial plan; T1 complete; T1.5 scheduled 2026-10-23; T2 next. Числа — provisional.

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

## Definition of done

- `hnsw_rs 0.3.5` + `ouroboros` compile on MSRV 1.88 and all CI platforms.
- Persisted ANN: atomic write, metadata, `READY`, load, stale/corrupt fallback.
- `DenseCache`: `Ann`, LRU/byte-cap, `TooLarge` hash cache.
- Daemon build/load; direct CLI load-only; backfill invalidation.
- 75k scale numbers recorded (parallel build, recall-vs-ef, binary size);
  recall/latency targets met or explicitly adjusted; warm non-blocking.
- `search_bench` baseline unchanged; no MCP/CLI changes.
- ADR-15 + docs + ledger complete.
