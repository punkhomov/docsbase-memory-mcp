# CF7c1 — explicit embedding context в index jobs

Исполнение: executing-plans/TDD самостоятельно в текущем изолированном worktree.
Spec: component-configuration FR-2/5/16, SC-3; core index crash/recovery contract.
Base: origin/main bbf650d после PR142 (CF7b/C12 final6d6e3bc имеет тот же tree). Rust1.100.0 stable, cli-full и lean offline. New crates: none.
No production unwrap/expect/panic; Cargo.lock, baseline/thresholds не меняются.

## Контракт и граница

IndexEmbedding<'a> хранит borrowed explicit source и Option<&dyn Embedder>,
разрешённый composition root. None означает явно выбранный lexical-only режим;
Config отвечает за indexing limits, prepared embedding context — за embedding.
Нельзя делать fallback в from_config или legacy после ошибки owner. Source
проверяется до SQL/markers/index writes даже если embedder None или texts пусты.
Provider выполняется через jobs::embed_texts_from вне SQL transaction.

Новые public run_full_with_embedding и run_incremental_with_embedding принимают
context. Crate-private run_incremental_checked_with_embedding сохраняет watcher
stale gate. Один private full/incremental core принимает FnOnce(&Path,&[String])
->Result<u64>; legacy adapters выбирают прежний configured jobs::embed_texts,
new adapters — context. Алгоритм plan/apply/markers не дублируется. Recreated
incremental fallback передаёт тот же closure и gate в full core.

Existing APIs сохраняют lazy config-resolution и прежние ошибки. Эта задача
предоставляет реальный embedded indexing path с owner; daemon/watcher/sync
composition переводятся следующей CF7c2, не выдаются за уже переведённые.
Нет file-v2/startup/persisted adoption, migration/grants/recovery coordinator,
live policy resizing, artifact recipe или search behavior change. #78 открыт.

## Одна reviewable задача

Files: Modify src/index/job.rs; Create tests/index_vector_source.rs; Modify
component-configuration/{requirements,design,tasks,progress}.md; plan здесь.
- [x] RED public API: integration реальных full/incremental jobs под strict owner;
  временный accidental legacy closure обязан давать Admission mode conflict.
- [x] Minimal shared-core implementation и borrowed IndexEmbedding.
- [x] Full unchanged skip и incremental replacement: SQLite chunk/vector coverage,
  committed Tantivy hits и стабильное поколение при no-op; provider не дублируется.
- [x] Full/incremental embedding failure: marker сохраняет retry неизменного файла,
  generation не публикуется; retry восстанавливает metadata/search/coverage.
- [x] Recreated index incremental fallback: хотя передан один path, full core
  восстанавливает все документы через тот же embedding owner и снимает marker.
- [x] Wrong owner/empty disabled work: Admission до marker/SQL generation и provider;
  lexical-only prepared context не разрешает Config providers самостоятельно.
- [x] Crate-private controlled stale gate: rejected payload не пишет markers,
  не вызывает embedder и не меняет generation; recreated fallback тоже guarded.
- [x] Focused full/lean, complete cli-full, full/lean all-target Clippy/fmt/diff;
  fresh task review/fix round и most-capable final review. Native four checks и
  inventory retaining parent tests/new regressions; authorized squash delivery.

Пример interface:
```rust
let source = dense.vector_source(project.id);
let embedding = IndexEmbedding::new(&source, stages.embedder.as_deref());
run_full_with_embedding(db, index, project, config, &embedding)?;
```
Tests используют controlled Embedder только вместо внешнего inference. Настоящие
SQLite/Tantivy/cache/provider error semantics остаются настоящими. Failures не
маскируются retries/sleeps/ignores. Runtime closure выбирается явным adapter,
не optional fallback в середине index algorithm.
