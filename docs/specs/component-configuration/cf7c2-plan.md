# CF7c2 — prepared context для sync/index/status composition

Исполнение: executing-plans/TDD в существующем изолированном worktree. Base:
origin/main3b34732 после PR145; reviewed7c2c9b7 имеет тот же tree.
Spec FR-2/3/5/16, SC-3 и R2/W103. Rust1.100.0 stable; cli-full и lean offline.
New crates: none; no production unwrap/expect/panic; no baseline/lock/threshold delta.

## Контракт

Новые tools::run_project_index_with_vectors, run_sync_job_in_with_vectors и
execute_sync_job_with_vectors получают Config + HybridStages + VectorCacheSource
и реальную dense/DB policy там, где она нужна. Prepared stages authoritative для
embedding, Config supplies indexing limits. Одна source используется в changed
plan и полном backfill. Root проверяется перед DB opens/status/job mutations;
оба explicit cache и Db.cache_root должны принадлежать source. Существующие
APIs сохраняют lazy configured wrappers и прежние state/error/provider timing.
Status/writer lease/terminal job persistence вынести в общие private callback cores,
не дублировать алгоритм. Provider runs outside registry/vector SQL transactions.

Daemon sync/index paths захватывают coherent config/stages из project_runtime.
Перед enqueue stage construction/admission завершается; конфигурационная ошибка
теперь синхронно возвращается до создания job (intentional scoped behavior change).
Spawned worker держит config/stages/source snapshots до конца. Legacy source policy
проецируется из захваченного Config, а не нового mutable dense project map после
reload; Owned clone удерживает тот же immutable owner. Auto-index после регистрации получает config/stages через per-project authority:
raw config не может вне этого authority переписать budget concurrent reload. Strict
legacy project config всё ещё rejected до v2 activation, не silently ignored.
Shared DB guard освобождается до provider preparation в sync_start.
CLI index сохраняет существующий startup from_config, но использует его результат
через prepared path без второго построения. Configured coverage_from выбирает
канонический key без provider creation; все3daemon coverage paths используют source.

Watcher consumer/config lifecycle остаётся CF7c3. Это не strict Shared startup,
v2/persisted adoption/migration/apply или полное завершение CF7/#78. No new keys.

## Одна reviewable задача

Files: src/daemon/{tools,server}.rs, src/hybrid/jobs.rs, src/cli/index.rs;
новые tests/vector_sync_context.rs, private server regressions и dated specs/ledger.
- [x] RED source-aware APIs и actual owner-backed project/index+sync outcome.
- [x] Shared callback cores и prepared APIs; legacy wrappers retained.
- [x] Real full+changed indexing status, queued sync backfill ранее offline chunks,
  unchanged no-op reuse, child connection и persisted terminal job statistics.
- [x] Provider error: failed generation не публикуется, markers retry unchanged
  bytes, job Error сохраняется; следующая job достигает Done и полного coverage.
- [x] Wrong root before project/job/status/registry mutations; provider zero work.
- [x] Source-aware coverage disabled/enabled, metadata/query key без provider builds;
  real daemon status/panel paths не legacy-open при injected owner construction.
- [x] Actual server/direct CLI propagation; old snapshot legacy budget не берётся из
  более нового dense map; owned source retains lifetime. Нет нового startup mode.
- [x] Full/lean focused+full suite, Clippy/fmt/diff и fresh task review.
- [x] Fresh final review, native four checks и inventory сохранения parent/
  выполнения всех additions; squash delivery.
