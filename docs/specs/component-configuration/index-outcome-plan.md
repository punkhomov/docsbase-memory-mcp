# IO1: подтверждённый результат индексации для #79

**Goal:** последний завершённый проход различим от исторического успеха при той же
index_generation; server/readiness получает проверяемую metadata без выдуманной
полноты. Это A-owned prerequisite FR-S2, не endpoint или весь #79.
**Architecture:** bookkeeping в существующей projects row; begin инвалидирует
прежний proof до работы; end публикует реальную JobStats и Indexed одной короткой
SQL transaction. Writer lease остаётся у нынешнего production caller.
**Tech stack:** Rust 1.100.0, существующие rusqlite/serde/blake3; новых crates нет.
**Spec:** [requirements.md](requirements.md), dated IO1 delta; website-search FR-S2
и core FR-16/18/19/26. No live APIs/downloads, baseline/thresholds не меняются.

## Проверенные source facts и границы

run_full/run_incremental могут вернуть Ok с errors>0. Generation увеличивается
только при applied plan. Direct CLI index не создаёт sync row. Watcher сохраняет
Indexed при recoverable error, иначе существующий bounded retry блокируется.
Full core используется также напрямую embedded callers; только wrapper-финализации
недостаточно. Incremental при recreated Tantivy сам переходит в full core.

Общий jobs core записывает begin/end; wrappers очищают proof до index open и при
ручной смене статуса, но не публикуют устаревший результат поверх core. Empty filtered
watcher batch записывается лёгким subset outcome без повторного corpus scan.
Production writer/read leases не расширяются; embedded caller по-прежнему сам
сериализует свои Db/index handles. Attempt ID не заменяет lease/generation и не
доказывает terminal hybrid sync success (backfill идёт после lexical job) или
актуальность `.md` после scan: при watcher off нужен явный новый sync.

Первый slice принимает полноту только из zero-error full survey. Любой subset,
включая успешный/no-op, консервативно отзывает предыдущий completeness proof.
Наследование full baseline после subset не реализуется и не подразумевается.
Это соответствует начальной приёмке B только по attributable complete full sync.

## IO1-1 — additive schema и bounded projection

Files: Modify src/store/migrations.rs; Create src/index/outcome.rs;
Modify src/index/mod.rs и packages/client/test/fixtures/protocol.json (только
новая metadata schema version); Create tests/index_outcome.rs.

Schema3 добавляет projects.index_attempt INTEGER NOT NULL DEFAULT0 с CHECK
nonnegative integer и projects.index_outcome_json TEXT nullable. Migration
одновременно повышает только прежний compatible project stamp2→3; stamp0/1/newer
не исправляет. Existing lexical index/content/generation не пересобираются и не
инвалидируются. Для старых projects outcome=NULL: никакого historical proof.
Старые бинарники откажут по DB schema3; downgrade требует прежний cache/backup,
а не ручной user_version. PR не меняет vector DB или Tantivy format.

IndexOutcome schema1 содержит attempt_id, index_generation, canonical_root,
recipe_id, scope FullSurvey/Subset и полную JobStats. Recipe — BLAKE3 domain/version
+ max_file_size/max_docs_per_project/упорядоченные ignores, tokenizer version,
frontmatter metadata recipe и chunk policy version/MAX_CHUNK_CHARS. Provider/
credential/search timeout/cache budget в recipe не входят. Ignore files являются
частью обследованного source tree, а не обещанием его дальнейшей неизменности.

Fixed serialization bound1MiB — граница outcome format/защита от corrupt oversized
row, не скрытый runtime resource default. Запись сверх неё отказывает, не усекaет
stats. SQL ограничивает BLOB byte length до чтения JSON и canonical_root; status
возвращается SQL boolean без загрузки произвольной строки. Некорректный/unknown JSON,
неправильная version/scope/attempt/root/generation/stamp/status, oversized root/status, pending doc markers
или warnings>MAX_WARNINGS => None (unknown). SQLite I/O/schema errors остаются Err.

Public API: index::outcome::{IndexOutcome, SurveyScope, recipe_id, read}.
`read(&Db, project_id)->Result<Option<IndexOutcome>>` использует один SELECT;
`IndexOutcome::is_complete()` => FullSurvey && stats.errors==0. Consumer сравнивает
recipe_id с собственной indexing policy и берёт существующий project read lease,
если связывает metadata с Tantivy. API не обещает continuous-serving snapshot.

RED cases: DB2 upgrade сохраняет пригодный project stamp/content/generation,
без результата; старые stale stamps не оживают. Corrupt/oversize/unknown/mismatched
outcome — unknown без allocation oversized JSON. Rust↔Node protocol fixture обновляется ровно в schemaVersion/Hello schema_version
2→3; shape/protocol/search baseline прежние. Изменение recipe только по
релевантным параметрам; secrets отсутствуют. Begin/end helper stale attempt CAS
не публикует поверх новой работы.

## IO1-2 — все jobs и coherent completion

Modify src/index/job.rs, src/daemon/registry.rs, src/daemon/tools.rs и
src/watch/mod.rs только в существующих index wrapper/run_batch границах. Search
pipeline/daemon server/providers/lifecycle/IPC/CLI syntax не меняются.

Private `outcome::run(db,project,config,scope,work)->Result<JobStats>`:
1. UPDATE projects: index_attempt+1, outcome=NULL; RETURNING attempt/root. Нулевое
   число rows/overflow/refused mismatch — typed error до job work.
2. Вызвать настоящий work без SQL transaction через walk/embedding/Tantivy.
3. При Err proof уже NULL; вернуть исходную ошибку, не менять watcher status/retry.
4. При Ok stats: короткая transaction читает actual generation, проверяет attempt/
   root и записывает bounded JSON + Indexed/last_indexed_at; full обновляет stamp,
   subset оставляет его. При stale CAS/record failure readiness остаётся unknown;
   failure не маскируется успешным return.

Full/incremental `*_using` оборачивают прежний core. Recovery recreated-index
выбирает FullSurvey до begin, без двух attempts. Root validation prepared APIs
остаётся раньше begin. Существующий config check остаётся раньше corpus mutation;
rejected stale attempt может только отозвать proof, generation не меняется.

registry::set_status/mark_indexed очищают proof и повышают index_attempt в том же
UPDATE, чтобы поздний end не воскресил result после внешней status invalidation.
ID — токен, а не счётчик запусков: разрешены gaps от wrapper/status invalidations. Full wrapper
по-прежнему ставит Indexing до open; успешный core уже публикует статус атомарно,
поэтому повторный mark_indexed убирается. Error path сохраняется. Watcher после root admission и до finalize/index open вызывает лёгкий
outcome::invalidate (token+NULL, без status change), сохраняя retry. Затем убирает
дублирующий Indexed UPDATE; empty filtered path после check_config записывает
zero-stat Subset через тот же helper. Любая ошибка до core оставляет unknown,
а historical status не превращается в completeness proof.

RED/acceptance реальных путей:
- full success и second skipped-only scan: новый attempt, та же generation,
  настоящие stats, fresh read-only Db видит согласованную Indexed+outcome;
- oversized/budget-skipped file: Ok/errors>0 и реальный partial outcome; повторный
  unchanged scan не возвращает прежний zero-error proof;
- subset без complete baseline/после partial/full: не полон; errors0 не повышает
  partial до full. Recreated index → full recovery c реальным scope;
- fatal invalid walk/config failure, interrupted child после begin, stale apply:
  прежний proof отозван даже без generation bump;
- actual direct CLI index, daemon full/sync и watcher success/error/retry/empty batch
  доходят до общего core и сохраняют исходную search/status семантику;
- result publication внутри существующего writer lease: gated embedder/reader
  проверяет old proof уже недействителен; read lease конфликтует до completion;
- no provider calls для outcome observation, никаких SQL guards через embedding.

## IO1-3 — verification и delivery

Каждый meaningful тест сначала RED, затем минимальный GREEN. Точечные causal
mutations: убрать begin clear; не записывать новый outcome на skipped-only;
объявить subset full; убрать errors>0; сравнение attempt/root. Затем восстановить.

Focused cli-full/lean: index_outcome, index_job, index_incremental, watcher,
config_runtime, sync_jobs, ipc_server и private watcher/project-state tests.
Полный cargo test --locked --features cli-full и lean; all-target Clippy в обеих
конфигурациях, fmt/diff. Frontend/client assets собрать до Cargo по AGENTS.
Fresh task/final review по .opencode reviewer, исправить Critical/Important;
required docs/linux/macos/windows CI и downloaded parent/test/ignore inventory.
Никаких новых ignores, baseline/threshold changes или live APIs. Ledger с каждым
commit. Готовый PR человеку; merge не выполняется агентом.
