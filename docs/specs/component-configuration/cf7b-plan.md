# CF7b — передача владельца векторов потребителям

Исполнение: executing-plans/TDD в текущей сессии, без делегирования реализации.
База: origin/main 254af73 (PR140). Spec: requirements.md FR-2/5/16, SC-3;
CF7a owner contract. Rust1.100.0 stable, cli-full и lean offline. New crates: none.
Нет новых TOML keys, live inference/downloads, baseline/threshold изменений.

## Контракт

Cloneable `VectorCacheSource::{Legacy(CacheLimits), Owned(VectorStore)}` создаёт
connections через `connect(root) -> Result<VectorCache>`. Owned проверяет canonical
root до открытия SQL и никогда не откатывается к legacy; Legacy сохраняет прежние
zero-cap semantics. `validate_root(root)` не создаёт каталог при отказе.

`DenseCache::with_vector_store(policy, &owner, &DenseSettings) -> Result<Self>`
сохраняет immutable owner; cold rows, worker unpin, sharing_query_cache и snapshot
используют его же. `vector_source(project_id)` возвращает Owned либо полную legacy
projection. Canonical identity strict root используется для cold-flight ключа и
проверки metadata; legacy raw-path identity сохраняется. Ошибка другого root
предшествует provider work в search и metadata load в dense.

`set_vector_cache_max_bytes(project_id,max_bytes) -> Result<()>`: legacy принимает
budget, Owned отклоняет любую project override с Admission, даже равное значение.
Это осознанная смена experimental Rust API (constitution: до1.0 library semver
не обещается). CLI/MCP/config/IPC контракты не меняются. Оба daemon caller передают
ошибку; reload проверяет budget до invalidate/config/stages publication.

Jobs получают source-taking variants для text embedding, project backfill и coverage;
старые signatures остаются legacy adapters в тот же core. Реальные search full/scoped/
streaming fallback paths берут source из dense. SQL reservations остаются короткими,
provider вызывается вне vector transaction. TTL/pin identities и worker publication
после закрытия vector connection сохраняются.

Граница: это consumer API и search wiring, не strict startup activation. Existing
production sync/config/provider construction остаётся legacy; полный переход
composition root, daemon status/sync factories, persisted mode, consent/apply и
quiescence/recovery — следующий CF5/CF7 этап. Blockers migration не снимаются.
CF7/#78 не закрываются. Нет live policy changes или новых index recipes.

## Одна reviewable задача CF7b

- [x] RED API: новый source/constructor и fallible setter в
  tests/vector_store_consumers.rs; затем causal RED реальных jobs/dense/search.
- [x] Источник: src/vector_cache/source.rs, export в vector_cache.rs.
  `connect(root)` вызывает validate_root, Owned.connect или legacy open_with_limits.
- [x] Dense: optional immutable VectorStore, clones в sharing/snapshot,
  canonical strict admission/cold key, load_rows/worker через vector_source.
- [x] Jobs: embed_texts_from(root,embedder,texts,source),
  embed_project_chunks_from(db,embedder,project,pin,dense,source),
  coverage_for_key_from(db,model,project,source); wrappers сохраняют поведение.
- [x] Search: заменить два direct vector opens на dense.vector_source;
  stream_exact/hash принимают context; root admission до query embedding.
  Два daemon setter caller с `?`, до публикации и invalidation.
- [x] Реальные тесты: same-root alias/different root, reuse двух проектов,
  общие limits и legacy0, strict setter refusal; backfill/coverage и provider SQL
  BEGIN IMMEDIATE probe; cold dense и full/scoped/too-large search; worker snapshot
  lifetime/owned unpin с сохранением чужих leases. Regression failed reload удерживает
  прежние config/stages/dense state.
- [x] Focused tests, full cli-full, lean focused, full/lean all-target Clippy,
  fmt/diff; dated deltas и ledger, fresh task/final review, fix rounds.
- [ ] Native docs/Linux/macOS/Windows + inventory against PR140, squash delivery.

Команды: source /tmp/docsbase-rust100-env.sh; cargo test --locked --features cli-full
--test vector_store_consumers; cargo test --locked --features cli-full --lib;
полный cargo test --locked --features cli-full; cargo clippy --locked
--features cli-full --all-targets -- -D warnings; lean equivalent; cargo fmt --check.
