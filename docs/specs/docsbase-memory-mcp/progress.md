# docsbase-memory-mcp ledger — plan: docs/specs/docsbase-memory-mcp/tasks.md

BASE (before T1): 1e02a47

## Task log

Task 1: fix round 1/5 (1 addressed: cargo fmt; 0 open — tester: cargo test 3/3, clippy clean, fmt check OK)
Task 1: minor (deferred): smoke.rs:19-34 — config checks are substring-based, not TOML-parsed
Task 1: Ruling: NFR-6 (static binary) had no owning task — added T32 (release/static hardening); coverage updated
Task 1: complete (commits 1e02a47..a5f6265, tests: cargo test --test smoke → 3/3, clippy -D warnings → clean, fmt → clean)

Task 2: Ruling: plan defect — `serde_json::Error` conversion moved from T2 to T5 (crate appears only in T5); tasks.md updated
Task 2: minor (deferred): `From<io::Error>` maps to `Internal` (-32603), drops `ErrorKind`/source; T11/T29 must remap file-IO to `Error::Index`
Task 2: minor (deferred): Cargo.toml/Cargo.lock absent from T2 `Files` (plan oversight; Files list updated)
Task 2: complete (commits a5f6265..f70aa04, tests: cargo test → 6/6, clippy clean, fmt clean; review PASS/APPROVED)

Task 3: Ruling: dev-dep `tempfile` moved T4→T3 (config tests need temp dirs); plan updated
Task 3: Ruling: design §2 "один крейт на задачу" противоречил конституции — приведён к правилу «крейты из §7 с обоснованием»
Task 3: minor (deferred): CLI-precedence — opt-in `with_overrides`, вызывающий может её пропустить (src/config/mod.rs:97)
Task 3: minor (deferred): paths::cache_dir/config_dir не покрыты юнит-тестами; проверить через DOCSBASE_* env в T17/T19
Task 3: minor (deferred): ошибки чтения/парсинга конфига маппятся в Internal (-32603); T29 должен дать actionable категорию
Task 3: complete (commits f70aa04..3567290, tests: cargo test → 11/11, clippy clean, fmt clean; review PASS/APPROVED)

Task 4: Ruling: review Med (cache root umask) — fixed via chmod 0700 + regression test; fix commit d685040
Task 4: minor (deferred): migrate/read_version messages lack DB path; wrap in T11
Task 4: minor (deferred): FK cascade + UNIQUE constraints untested; cover in T11/T21
Task 4: minor (deferred): ProjectStatus::as_str/parse round-trip untested
Task 4: complete (commits 3567290..d685040, tests: store 6/6, clippy clean, fmt clean; review PASS + fix ADDRESSED)

Task 5: Ruling: T5 `Files` дополнены `src/error.rs` — сюда перенесена отложенная из T2 конверсия `From<serde_json::Error>`
Task 5: minor (deferred): только `Response::Hello` в roundtrip; нет raw wire fixture и `\r`-кейса (tests/ipc_protocol.rs)
Task 5: minor (deferred): wire использует i32/u64 вместо ErrorCode/usize из эскиза design §5 — обновить эскиз или зафиксировать отклонение
Task 5: minor (deferred): асимметричный API (`encode` generic, только `decode_request`); T20 добавит декод ответов
Task 5: complete (commits d685040..7d8a8f4, tests: cargo test → 21/21, clippy clean, fmt clean; review PASS/APPROVED)

Task 6: Ruling: `walk` сигнатура — `Result<impl Iterator>` (eager-валидация `..`), а не голый iterator из плана
Task 6: fix: `.git` добавлен в `DEFAULT_IGNORED_DIRS`; тесты на hidden/dist/build и `resolve_in_root` (review Low #1)
Task 6: minor (deferred): вложенные `.docsbaseignore` с `..` не валидируются (escape невозможен); пост-фильтрация default-папок — перф-долг в T30
Task 6: complete (commits 7d8a8f4..ca85ec8+fix 290363b, tests: walk 7/7, clippy clean, fmt clean; review PASS/APPROVED)

Task 7: minor (deferred): unquote снимает непарные кавычки (`'Twas` → `Twas`); `--- ` с хвостовым пробелом не распознаётся как закрытие (frontmatter.rs:83,32)
Task 7: minor (deferred): нет регрессионных тестов для `"---"`, `"---\n---"`, без trailing newline, multibyte body
Task 7: complete (commits 290363b..ee1dacb, tests: frontmatter 5/5, clippy clean, fmt clean; review PASS/APPROVED)

Task 8: Ruling: fix loop 2 rounds — F1 High (длина fence), F2 Med (kind на чанк), F4–F7; fix commits 15fb51a, 588a416; re-review ADDRESSED
Task 8: minor (deferred): F3 (table-часть) — различающего table-теста не существует (в таблице нет пустых строк)
Task 8: minor (deferred): latent false positive — fence → prose → чужой closing fence классифицируется Code (был и до фикса)
Task 8: complete (commits ee1dacb..588a416, tests: chunker 19/19, clippy clean, fmt clean; review FAIL(High) → fixed → ADDRESSED)

Task 9: Ruling: fix round — сплит по любому non-alphanumeric (High), точные offsets, дедуп, `register(&TokenizerManager)`, защитный `token()`; fix commit b9b6042; re-review ADDRESSED
Task 9: complete (commits a5e866e..b9b6042, tests: tokenizer 13/13, clippy clean, fmt clean; review FAIL(High) → fixed → ADDRESSED)

Task 10: Ruling: `search(&str, limit)` вместо `&Query`; `ReloadPolicy::Manual` + явный reload (в tantivy 0.25 нет `OnCommit`); план обновлён
Task 10: fix round — пустой запрос → `Error::Query` (-32014), `limit = 0` → `[]`, доступ `reader()`; fix commit 26e6c90; re-review ADDRESSED
Task 10: minor (deferred): `extract_identifiers` пере-/недоинклюзивен (слова с пунктуацией получают boost; чисто camelCase-идентификаторы в поле не попадают)
Task 10: minor (deferred): `title` = последний heading, а не title документа; уточнить в T11/T27
Task 10: minor (deferred): schema-mismatch проверяется только по именам полей; усилить при I1/T17
Task 10: minor (deferred): `Hit.chunk_id` — композит `doc_id<<32|seq`, а SQLite `chunks.id` — autoincrement; согласовать в T11/T28
Task 10: complete (commits e760d25+fix 26e6c90, tests: tantivy_index 7/7, clippy clean, fmt clean; review PASS/CHANGES(Med) → fixed → ADDRESSED)

Skill-review T1–T10 (router → coding-guidelines, m06, m15): S1–S11, fix commit 6409d25
Skill-review: S1 Med — `ChunkMeta.kind`/`SyncJob.state` String → enum'ы `ChunkKind`/`SyncState` (+`as_str`/`FromStr`, строки БД не менялись)
Skill-review: S2 Med — глобальный `From<serde_json::Error>` удалён: `encode` → `Internal`, `decode_request` → явный `Protocol`
Skill-review: S3–S5, S8, S11 — HashSet-дедуп в токенизаторе; в `.docsbaseignore` глотается только `NotFound`; `trim_ascii_end`; `HEAP_SIZE` → `WRITER_HEAP_BYTES`; missing stored field → `Internal` (без тихого 0)
Skill-review: S9 — `Config::validate` (нулевые лимиты → `Admission`), вызывается в `load_from`; после `with_overrides` — повторная валидация
Skill-review: S10 — `Error::Internal` получил `#[source]` + конструкторы `internal`/`internal_with_source`; io/rusqlite/tantivy/ignore/toml ошибки сохраняют цепочку
Skill-review: S7 Ruling — `ProjectStatus::parse` → `impl FromStr` c общим `ParseEnumError`; T11 обязан маппить `index::chunk::ChunkKind` → `store::models::ChunkKind` (имена совпадают, модули разные)
Skill-review: Ruling — S6 (let-chains в `Request::validate`) отклонён: design §2 MSRV 1.85, let_chains стабильны с 1.88
Skill-review: scoped re-review PASS, Critical/Important нет; minors (deferred): нет тестов на S4 non-NotFound и S11 corrupt-index (нужен фабрикованный индекс); docs синхронизированы (tasks.md T2/T5, Config::load `# Errors`)

Task 11: Ruling: files — дополнительно `src/store/repo.rs` и `pub(crate)`-доступ к `Connection`; `Serialize` на `Frontmatter`; `MAX_CHUNK_CHARS = 4000`
Task 11: Ruling: R2-идемпотентность — in-flight marker (`content_hash = ''`) для плана и удалений перед tantivy-операциями, точная аллокация `doc_id = MAX(id)+1`; FR-19-лимиты (oversized/over-budget) считаются как warnings в `JobStats.errors`
Task 11: fix round 1 — frontmatter-сдвиг строк, marker, walk-error не чистит поддерево, budget учитывает removed, `pub(crate) repo`; commit 6151e9b
Task 11: fix round 2 — tombstone удалений (краш после purge + восстановленный файл); commit 27a6bca
Task 11: fix round 3 — unit-тест `mark_pending` (mutation-verified: удаление loop ломает тест); commit 7971898
Task 11: minor (deferred): wiring `run_full → mark_pending` и порядок «marker до tantivy» не запинены (нужен fault-injection seam); walk-error purge suppression без теста
Task 11: complete (commits a1859c6..7971898, tests: index_job 11 + unit 1, всего 86, clippy clean, fmt clean; review FAIL/CHANGES(3xMed) → fixed → PASS)

Task 12: Ruling: `Db` хранит `cache_root` (для I7 при сигнатуре `ensure_project(db, path)`); git root — поиск `.git` вверх без git CLI; категория невалидного root — `Project` (-32012)
Task 12: minor (deferred): F1 — `$HOME` сравнивается без canonicalize (symlink HOME обходит I7); F2 — non-UTF8 пути через `to_string_lossy`; F3 — нет busy_timeout (гонка двух писателей → SQLITE_BUSY); F4 — unit-тест home флаки, если TMPDIR внутри git-репы; F5 — имена RED-тестов слегка отличаются от брифа
Task 12: complete (commit 67431a9, tests: registry 7 + unit 1, всего 94, clippy clean, fmt clean; review PASS/APPROVED, minors deferred)

Task 13: Ruling: `run_incremental` + `run_incremental_with(cfg)`; общий `finish_plan` (marker/tantivy/apply) для full/incremental
Task 13: minor (deferred): F1 — бюджет incremental игнорирует удаления этого прогона (rename у лимита теряет документ; починить до T22); F2 — дефолтный `Config` в `run_incremental` (T22 обязан звать `_with`); F3 — проверка root лексическая (`..`-пути не нормализуются)
Task 13: complete (commits 3d60d5c..382f746, tests: index_incremental 6, всего 100, clippy clean, fmt clean; review PASS/APPROVED, minors deferred)

Task 14: Ruling: lease `projects/<id>/.writer.lock` (fd-lock, non-blocking `try_write`), статус-машина под lease, конфиг грузится от `project.canonical_root`; T11/T13-тесты переведены на `projects/<id>/tantivy` (layout дизайна §3)
Task 14: fix round — статус `Error` при любой ошибке (включая open index), конфиг от git root, исходная ошибка не маскируется, `instruction` в Display, `last_indexed_at` стампится при `Indexed`; commit a6de5af
Task 14: minor (deferred): падение `set_status(Indexed)` оставляет `indexing` (самоизлечимо следующим прогоном); порядок «регистрация → валидация конфига»
Task 14: complete (commits 8a61489..a6de5af, tests: cli_index 4, всего 104, clippy clean, fmt clean; review PASS/CHANGES(Med) → fixed → ADDRESSED)

Task 15: Ruling: `ReadIndex` (read-only tantivy handle, без writer) + `chunk_id_parts`; read-команды отвергают статус != Indexed (C8); `status` выводит hint при пустом реестре
Task 15: fix round — тест «read path не берёт writer/lease», CLI-кейс cwd вне root, `status` hint, unit `chunk_id_parts`; commit включён в T15
Task 15: minor (deferred): hits без citation молча выпадают (R2-окно); missing index dir → -32603 без recovery-hint; FR-23 в acceptance T15 vs traceability (T28)
Task 15: complete (commit c13035a + fix, tests: cli_read 7, tantivy_index 8, всего 111, clippy clean, fmt clean; review PASS/APPROVED(Med test-gap) → fixed)

