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
Skill-review: Ruling — S6 (let-chains) был отклонён при MSRV 1.85; ПЕРЕСМОТРЕНО в MSRV-раунде: MSRV поднят до 1.88, let-chains применены
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
Task 15: complete (commit c13035a + fix, tests: cli_read 7, tantivy_index 8, всего 112, clippy clean, fmt clean; review PASS/APPROVED(Med test-gap) → fixed)

Task 16: Ruling: `docsbase index` остаётся direct+lease (сокет-роутинг для чтения); `status` идёт без привязки сессии (`handshake_registry`)
Task 16: minor (deferred): нет таймаута на `UnixStream::connect` (backlog); build_id из Hello не сверяется на клиенте (I1 — на сервере); формы payload search/list/status должны совпадать у daemon и direct (T19/T21); version-mismatch path не запинен тестом
Task 16: complete (commits 2ad12b5+565188d, tests: cli_routing 3, всего 115, clippy clean, fmt clean; review PASS/CHANGES(Med) → fixed → ADDRESSED)

Task 17: Ruling: tokio-фичи расширены (io-util, sync, macros — нужны для lines/mpsc/select); тест-симы `DOCSBASE_DAEMON_EXE`/`ensure_daemon_with`/hidden `--grace-ms`; zombie детектится по `/proc/<pid>/stat` (Z = dead)
Task 17: minor (deferred): unbiased select! (grace vs accept — добавить `biased;` с accept первым); zombie-дети у долгоживущего frontend (NFR-9/T23); liveness-проба коннектом создаёт сессию (T19); нет тестов start-lock concurrency/SIGTERM/cancel-grace
Task 17: complete (commit d8a4cbe, tests: lifecycle 5 + unit 1, всего 121, clippy clean, fmt clean; review PASS/APPROVED(Med → T18))

Task 18: Ruling: `admission::Lease` (Box::leak + guard `'static`) вместо типа `Admission`; cleanup_stale гейтится admission-lock (закрыта T17-раса); conflicts.ndjson {ts,kind,expected,actual,cache_root}
Task 18: fix round — admission-check перед cleanup, schema/build expected+actual в логе, tasks.md синхронизирован; commit d72bd77
Task 18: minor (deferred): root mismatch не сверяется (I1 требует только build/schema); admission_lock_held fail-open при EACCES/EMFILE; нет теста live-gap ветки; 10s wait при wedged ком
Task 18: complete (commits 22c38c0+d72bd77, tests: admission 4, всего 125, clippy clean, fmt clean; review PASS/CHANGES(Med) → fixed → verified)

Task 19: Ruling: tool-реализации вынесены в `daemon::tools` (одинаковые payload'ы direct/daemon); `index_project` работает на своём соединении `Db`; `docs.id` аллоцируется в marker-транзакции (`insert_pending_doc`, без `MAX(id)+1`)
Task 19: fix rounds — auto_index от git root; замена сессии при повторной регистрации; статусы до lease (конфликт не флипает `error`); handshake-порядок Hello-first; marker-tx до tantivy + короткая SQLite-транзакция после; retry при гонке auto-index; commits 553cec6, e1d9fcd, 73c5f3d
Task 19: minor (deferred): >5s SQLite-транзакция может дать SQLITE_BUSY; краш job оставляет status=indexing до полного прогона; shared Mutex<Db> для reads освобождён, но сессионная запись живёт до отказа записи
Task 19: complete (commit e07353e+fixes, tests: ipc_server 11 + unit 1, всего 138, clippy clean, fmt clean; review FAIL(High) → fixed → verified)

Task 20: Ruling: на момент T20 был rmcp 2.2.0 (3.5.0 требует Rust 1.88 при MSRV 1.85); ручной `ServerHandler` вместо `#[tool]`-макросов; per-tool IPC-таймауты (30s quick / 600s long); новый `Error::Transport` + реконнект с одним retry
Task 20: fix rounds — таймауты и desync, invalidation/reconnect кэшированного клиента, структурный Transport, retry с ensure_daemon, один префикс hint, allowlist без дубля, rmcp без `macros`; commits 48e0cff, fa911f4
Task 20: minor (deferred): `index_project` без `path` из непривязанной сессии требует path (C8-петля — T21); mid-call смерть daemon лечится следующим вызовом; `get_doc`/`read_neighbors`/`sync_*`/`list_projects` рекламируются, но реализуются в T21/T28
Task 20: complete (commit e2fbf96+fixes, tests: mcp_frontend 6, всего 144, clippy clean, fmt clean; review PASS/CHANGES(High) → fixed → verified)


MSRV-раунд (после T20): design §2/tasks.md/Cargo.toml — MSRV 1.85 → 1.88 (edition 2024 держит 1.85 минимумом, но 1.88 даёт rmcp 3.x и let-chains)
MSRV-раунд: rmcp 2.2.0 → 3.3.0 (`default-features = false`, features server/transport-io; 3.5.0 придержан min-publish-age); адаптация `CallToolResponse` (SEP-2322) в frontend
MSRV-раунд: S6 закрыт — let-chains применены в `Request::validate` и в проектно-сессионной проверке frontend
MSRV-раунд: 144 теста зелёные, clippy/fmt чисты
Task 21: Ruling: новый `Request::RegisterUnbound` — cwd-only сессия, чтобы `index_project()` без path работал из непривязанного frontend (закрывает C8-петлю из T20); `index_project` пишется в `sync_jobs` и выполняется синхронно, `sync_start` — фоном через `spawn_blocking`
Task 21: Ruling: один активный job на проект гарантируется атомарным `INSERT..SELECT..WHERE NOT EXISTS` (без unique-index); при старте daemon `queued/running` от прежнего процесса помечаются `error` ({"orphaned":true}); `sync_start`/`sync_status` переведены в quick-таймаут
Task 21: fix round 1: setup-ошибка job'а пишется как `error` (не виснет queued) + eager `Config::load` в `sync_start`; frontend re-handshake после успешного `index_project` (hint снимается); enqueue retry ×3 (TOCTOU); state-guards в repo; строгие типы args (`opt_str`/`opt_i64`); doc-комментарии repo
Task 21: minor (deferred): unknown `job_id` остаётся `Project` (-32012) до T29; `started_at` = время enqueue, не старта прогона; `sync_jobs` без retention; флак-риск `second_sync_start_returns_current` (40×200 секций, 20/20 локально); `Db::open`-фейл не может записать `error` (теоретический)
Task 21: complete (tests: mcp_registry 9, всего 153, clippy clean, fmt clean; review CHANGES(High) → fixed → verified PASS)
Task 22: Ruling: `notify-debouncer-mini` из брифа отклонён — эмитит события по мере expiry, всплеск распадается на 2-4 батча; вместо него quiet-collector 1.5s (жёсткий кап 2s) поверх `notify`; crate удалён из Cargo.toml, design §7 обновлён
Task 22: Ruling: батч = дедуп сырых путей, директории раскрываются на flush (гонка recursive watch на новых каталогах, EXPAND_MAX_FILES=10k/DEPTH=32); удалённые не-`.md` пути — маркеры purge поддерева; фильтр повторяет ignore-стек walk (`indexable_files`, max_depth=1, кэш на каталог с refresh-on-miss; parent/nested `.gitignore`, `.git/info/exclude`, `.docsbaseignore`); symlink'и не следуются (canonical containment, lstat, FR-32)
Task 22: Ruling: per-batch `IndexHandle` внутри writer-lease (держать writer между батчами = вечный tantivy lock); при lease-конфликте батч не теряется (pending + retry 250ms); починен отложенный дефект T13 (rename на лимите docs) и добавлен purge поддерева в `run_incremental_with` (два прохода, бюджет max - live)
Task 22: fix rounds по ревью: symlink escape (canonical/lstat), starvation кап 2s, dir-rename purge, lease retry вместо drop, filter fidelity, empty-batch skip, self-removal consumer'а через Weak, IndexFilter refresh-on-miss (новые файлы), бюджет через `max - live`, retain planned/still-present от flip-race, expansion caps
Task 22: minor (deferred): статус `error` для умершего watcher'а (T23/T29); mutex в `ensure` на время spawn; unbounded mpsc при долгой lease-блокировке; stale-cache при правке `.docsbaseignore` (лечится full sync); symlink-swap внутри окна; notify-ошибки только логируются
Task 22: complete (tests: watcher 8, mcp_registry 10, index_incremental 8, всего 165, clippy clean, fmt clean; review CHANGES(Critical) → 2 fix rounds → verified PASS)
Task 23: Ruling: grace-shutdown теперь по живым сессиям, а не по соединениям; `in_flight` (RAII-guard) держит daemon от выхода, пока запрос не получил ответ (гонка «deadline vs регистрация» ловилась тестом); deadline не продлевается churn'ом соединений
Task 23: Ruling: janitor (500ms, spawn_blocking) снимает сессии с мёртвым pid и отменяет их соединения через oneshot (fd не переживает 2s-бюджет); daemon без единого клиента само-завершается после grace (осознанно, NFR-9); Stats/threds + `status.threads`, `Response::Stats.threads`
Task 23: fix rounds по ревью: guard in_flight в теле grace-ветки (селект-гонка), cancel-канал сессий, join под одним lock (sessions→cancels), spawn_blocking janitor, Stats::default в direct CLI, RAII-guard, тесты (live-pid flow, +1 fd, grace 3000); regression `registration_survives_armed_grace` падает без фикса
Task 23: minor (deferred): prune по /proc — блокирующий вызов (вынесен в spawn_blocking, но accept-loop ждёт его); SC-5 soak (3 агента + watcher 1 час) — nightly, не в CI-наборе
Task 23: complete (tests: cleanup 4 + unit, всего 171, clippy clean, fmt clean; review CHANGES(Important) → 2 fix rounds → verified PASS)
Task 24: Ruling: conflict-лог вынесен в top-level `src/conflict.rs` (NDJSON, 0600; поля refused-процесса + recorded_*/holder_pid для incumbent); пишут все классы: lock_busy, build/schema mismatch, hello_build_mismatch, db_schema_newer, db_schema_mismatch (hint install/rebuild по newer/older)
Task 24: Ruling: штамп schema_version пишет только `registry::mark_indexed` (полный индекс: daemon/CLI/auto_index); watcher отказывает (Project + rebuild-hint) на stale/error-проектах; per-project schema mismatch остаётся Project (-32012) с инструкцией `docsbase index`, не Admission
Task 24: fix round по ревью: watcher-restamp закрыт regression-тестом `stale_schema_survives_file_edit`; schema_mismatch логирует expected/actual схем; hello-mismatch логируется; Db::open/open_readonly логируют и различают newer/older
Task 24: minor (deferred): store→ipc вызов build_id (слойность); hello-запись schema_version — это ожидание демона (schema фронтенда неизвестна); `docsbase install` появится в T25; root-mismatch не проверяется (от T18)
Task 24: complete (tests: admission_ux 5, всего 176, clippy clean, fmt clean; review CHANGES(Critical) → fixed → verified PASS)
Task 25: Ruling: `docsbase install` копирует current_exe в `$DATA/bin/docsbase` (atomic tmp+rename, 0755) и пишет owned-манифест `$DATA/install.json`; `$DATA` = XDG data / `DOCSBASE_DATA_DIR` (design §3 обновлён); admission-lease берётся до правки бинаря и держится всю операцию
Task 25: Ruling: `uninstall` перечисляет owned-артефакты, конфиг (не удаляется) и проекты; без `--yes` только печатает; `--yes` удаляет бинарь, манифест и cache root (cache целиком owned), чужие файлы вне манифеста не трогает; manifest-paths валидируются (absolute, bin под $DATA/bin, cache-root выглядит как docsbase)
Task 25: fix rounds по ревью: stop_daemon ждёт смерть процесса (не только сокет); drain в StopDaemon идемпотентен через `stopping` (8 параллельных stop больше не вешают daemon); in-flight запросы дренируются, фоновые job'ы держат процесс/lease до конца; install ретраит lease до 600s, чистит stale daemon.json (remedy S4); тесты: update_waits_for_synchronous_job (реальный full index >5s в полёте), concurrent_stops_do_not_wedge, install_recovers_from_stale_state, uninstall без cache
Task 25: minor (deferred): drain без таймаута (SIGTERM остаётся путём отступления при зависшем запросе); TOCTOU read_state exists→read; nested-чужие файлы внутри owned-каталогов удаляются (cache объявлен owned); fsync tmp перед rename
Task 25: complete (tests: install 9, всего 185, clippy clean, fmt clean; review CHANGES(Critical) → 3 fix rounds → verified PASS by production-fix evidence)
Task 26: Ruling: global config читается один раз на старте daemon, project — при открытии проекта и кэшируется по project id; повторное открытие (bind сессии) перечитывает `.docsbase.toml` (OQ-6, FR-28/29)
Task 26: Ruling: невалидный global config на старте не роняет daemon, но блокирует открытия/sync/index с actionable-сообщением (`fix + docsbase daemon stop`), а `status` отдаёт `restart_required`/`notice`; атомарная замена файла подхватывается только рестартом
Task 26: fix rounds по ревью: watcher получает сменяемый config-slot (`Watchers::revise`) вместо рестарта (не плодит notify/threads, не флашит pending-батчи в writer-lease), `IndexFilter.dirs` сбрасывается при смене конфига, stamp global-файла снимается до загрузки
Task 26: minor (deferred): filter/runner не атомарны по конфигу (runner игноры не применяет); stamp false-positive при гонке записи во время старта
Task 26: complete (tests: config_runtime 5, всего 190, clippy clean, fmt clean; review 2 fix rounds → verified PASS)
Task 27: Ruling: токенизатор дополнительно эмитит identifier-токен без обрамляющей пунктуации (`assessment_plan_id`), иначе exact-запрос становится phrase и не матчится (FR-21, SC-1)
Task 27: Ruling: поле `title` = верхний заголовок документа (heading_path.first()), согласовано с SQLite `docs.title`; штраф 0.5 на чанки > max_chunk_chars применяется до усечения top-k (over-fetch limit*4+16), `text_len` в байтах (как границы chunker)
Task 27: Ruling: смена tantivy-схемы — durable `docsbase.rebuild` маркер (пишется до create_in_dir), пересоздание индекса + wipe docs/chunks + полный reindex; ReadIndex отказывает по маркеру; fallback incremental→full у watcher
Task 27: minor (deferred): frontmatter title не индексируется; огромный `limit` без clamp на границе MCP; corrupt meta.json не восстанавливается; fsync маркера/meta на power-loss
Task 27: complete (tests: search_golden 4, tokenizer 14, tantivy_index 11, index_job 11, всего 199, clippy clean, fmt clean; review 3 fix rounds → verified PASS)
Task 28: Ruling: `list_docs` — keyset по последнему `rel_path` (`next_cursor`), ответ `{project, docs, next_cursor}`; CLI `list` листает страницы до конца (без усечения на 50)
Task 28: Ruling: `search_docs` дополнительно отдаёт `chunk_id` (additive) — иначе агенту неоткуда взять id для `read_neighbors`; снапшоты T27 перегенерированы
Task 28: Ruling: `get_doc` — только относительные `.md` внутри canonical root (realpath), absolute/`..`/symlink наружу отвергаются (I4/FR-32); `read_neighbors` — окно seq с cap 100 на сторону и проверкой принадлежности проекту
Task 28: minor (deferred): tasks.md указывает `store/mod.rs` вместо `store/repo.rs`; нет теста на `limit = 0`; косметика `project_name`
Task 28: complete (tests: mcp_docs 4, всего 203, clippy clean, fmt clean; review PASS/APPROVED → minors fixed → verified)
Task 29: Ruling: `status.warnings` берутся из newest job и показываются только при state=done (новый failed job не оставляет старых предупреждений); CLI-direct и watcher логируют per-file ошибки в daemon log, durable per-project warnings отложены (нужны retention/семантика job-строк)
Task 29: Ruling: frontend отдаёт стабильные коды в `structuredContent` ошибочного tool-result `{code, message, instruction}`; IPC `Response::Error` получил `instruction` (сердечно), `user_message` не нужен
Task 29: minor (deferred): нет маркера truncation при >100 warnings; старый daemon + новый frontend дают двойной префикс; harness без read-timeout
Task 29: complete (tests: error_surface 3, всего 206, clippy clean, fmt clean; review CHANGES(2 Important, 1 Ruling) → fixes → verified)
Task 30: Ruling: бюджеты NFR-1 проверяются release-only тестами (`#[cfg_attr(debug_assertions, ignore)]`), criterion-бенчи как отдельные замеры; факт: search p95 594µs/50k чанков, full index 1000 md 122ms (запас >200x)
Task 30: minor (deferred): бюджет меряет `IndexHandle::search` (не e2e IPC+SQLite join); index-бенч включает teardown (консервативно); single cold run без медианы; дублирование корпусов bench/test; нет бенча incremental ≤300ms
Task 30: complete (tests: perf_budget 2 release + cargo bench 2, debug suite 206 (2 ignored), clippy clean, fmt clean; review PASS/APPROVED, minors deferred)
Task 31: Ruling: soak ограничивает корпус (32 слота на агента, `NOTE_SLOTS`) — иначе за час full index вырастает за freshness-бюджет и watcher голодает; `SOAK_SECS` для локального короткого прогона
Task 31: Ruling: offline-тест гоняет весь workflow в `unshare -rn` (fallback: только fd-scan), проверяет `/proc/<pid>/net/*` на TCP/UDP сокеты daemon (fail-closed) и отсутствие `conflicts.ndjson` в soak
Task 31: minor (deferred): tantivy integrity проверяется неявно (поиск после рестарта); namespace-путь не подтверждён на ubuntu-latest
Task 31: complete (tests: offline 1, soak ignored (12s при SOAK_SECS=10), всего 207+3 ignored, clippy clean, fmt clean; review CHANGES(2 Important) → fixes → verified)
Task 32: Ruling: статика — gnu `+crt-static` через alias `cargo release-static` (`--target` + target-scoped `--config`), musl отложен (нет x86_64-linux-musl-gcc для bundled C); обычные host-сборки остаются динамическими (proc-macro не переносят crt-static); ADR-8
Task 32: Ruling: release profile lto/codegen-units=1/strip/panic=abort; artifact-тест всегда прогоняет `cargo release-static` (без exists-shortcut) и жёстко проверяет ldd/размер: 11.9 МБ static, `ldd` → statically linked
Task 32: fix rounds: CI `RUSTUP_TOOLCHAIN=1.88.0` (иначе rust-toolchain.toml nightly перебивает action), components+`--locked`, checksum по basename; doc-backticks для clippy 1.88; STOP_TIMEOUT 5→15s и NO_LISTENER_TIMEOUT 2s (флейк `concurrent_stops_do_not_wedge`)
Task 32: minor (deferred): artifact-пути игнорируют CARGO_TARGET_DIR; отсутствие ldd = panic; RSS-половина NFR-2 не измеряется бенчем
Task 32: complete (tests: artifact 2, всего 209+3 ignored, clippy clean nightly+1.88, fmt clean; review CHANGES(2 Important) → fixes → verified)
Final review fix pass: C1 — `search_docs.limit` клампится до `MAX_HITS = 1000` (в tools и defensively в search_reader); регрессионный тест `limit: u64::MAX` держит daemon живым
Final review fix pass: FR-4 — `Lease::acquire` сверяет `cache_root` (canonicalized) с `daemon.json`, новый kind `root_mismatch`; тест `root_mismatch_refused_and_logs`
Final review fix pass: NFR-8 — detached daemon направляет stdout/stderr в `logs/daemon.log` (0600, создаётся при старте); тест в lifecycle; design §7 без tracing
Final review fix pass: NFR-2 — release-тест `rss_budget`: idle RSS ~9.5 МБ при бюджете 150 МБ; 10k-doc RSS остаётся Ruling (не измеряется)
Final review fix pass: corrupt `meta.json` больше не dead-end: `open_or_create` пересоздаёт индекс через `create_fresh`; тест `corrupt_meta_is_recreated`
Final review fix pass: frontend повторяет bind при stale project hint (после `docsbase index` из терминала) — e2e тест `stale_project_hint_recovers_after_cli_index`
Final review fix pass: FR-30 — `docsbase sync` (daemon `sync_start` + poll `sync_status`, direct-fallback) и `docsbase config`; тесты cli_read/cli_routing; FR-29 CLI-флаги (with_overrides) — Ruling: should-уровень, отложены
Final review fix pass: watcher visibility — `status.projects[].watched`; design §5 (RegisterUnbound, instruction, u64) и §13 (cli/status.rs, store/repo.rs) приведены в соответствие; тест в mcp_registry
Final review fix pass: Ruling: SC-9 (агентская метрика) — вне автотестов; grouped minors финального ревью остаются в deferred
Final review re-review: N1 (sync polling терял сессию и ловил grace-exit) — `docsbase sync` держит одно bound-соединение на sync_start и все poll'ы; тест cli_routing считает accept'ы == 1; e2e-репро (grace 4000ms, 8000 файлов) теперь exit 0
Final review: вердикт ревьюера ПОСЛЕ fix pass + N1 fix — **converged** (все Critical/Important закрыты, residual — только принятые minors/Rulings)

Task 33: Ruling: platform seam вписан дельтой в текущий спек (ADR-9 + T34–T37), отдельная спека отклонена; поведение Linux, wire-контракты, schema_version и CLI/MCP-поверхность не меняются; новых крейтов нет
Task 33: Ruling: инвариант шва — вне `src/platform/` нет transport/process/perms-вызовов ОС; проверка `tests/platform_boundary.rs` (T34/T35) + CI-шаг (T37); Linux-only тесты (offline/perf_budget) остаются под явным cfg
Task 33: minor (deferred): `socket_path` публичный API заменяется на `platform::daemon_endpoint` в T34 (breaking для внешних потребителей lib — допустимо, 0.1.0); точный список путь-семантики для `watch/mod.rs` уточняется в T36
Task 33: complete (docs-only: design §2/§3/§4/§7/§12 ADR-9/§13/§14, tasks.md T34–T37 + self-review секции; код и тесты не затронуты, cargo test/clippy не запускались — нечего проверять)
Task 33: fix round: сверка дельты с кодом — фасад `pub` (а не crate-internal: `tests/platform_transport.rs` и `daemon_endpoint` иначе недоступны), §5-sketch фасада, T34 +`tests/lifecycle.rs` (использует UnixListener/UnixStream), boundary-скан исключает `#[cfg(test)]` (тестовый `child.kill` в session), T35 точные паттерны/файлы, T36 +`admission.rs` (normalize_for_compare), T37 boundary-шаг в nightly и release, main self-review ссылается на T33–T37
Task 34: Ruling: ОС-транспорт вынесен в `pub mod platform` — `Endpoint` (serde строкой, совместим с `daemon.json.socket`), `daemon_endpoint` вместо `ipc::client::socket_path`, async `Listener/Stream` (сервер на `tokio::io::split`) и blocking `BlockingStream/BlockingListener` (фейки тестов тоже через фасад)
Task 34: fix (minor hardening): boundary-скан нормализует пробелы/`{}` (ловит grouped/multiline `use`), добавлен самотест `scanner_catches_grouped_and_multiline_imports`; `platform::bind` документирует требование IO-runtime; design §5-sketch приведён к фактическому API (`exists`/`remove` — свободные функции)
Task 34: minor (deferred): `Incoming` останавливается после первой ошибки accept; `to_string_lossy` в `Endpoint::serialize` для non-UTF8; shutdown-remove молча глотает NotFound (был warning); один short-read в platform_transport
Task 34: complete (tests: platform_transport 2 + platform_boundary 2, всего 220, clippy clean nightly+1.88, fmt clean; review PASS/APPROVED → minor fixes applied)
Task 35: Ruling: process/signals/perms за фасадом — `ShutdownSignal` (SIGTERM+SIGINT в одном cancellable recv), `process::{process_alive, fd_count, thread_count, detach}`, `fs::{secure_dir, secure_file, secure_executable, open_private_log}`; `#[cfg(unix)]` в store убран, `/proc` и chmod-паттерны ушли из lifecycle/session/conflict/store/install; boundary-паттерны расширены
Task 35: fix (minor): `secure_dir` создаёт каталог сразу с mode 0700 (DirBuilder) и затем chmod — без окна world-readable; `open_private_log` идемпотентно ужесточает существующие логи (chmod-ошибка теперь warning в conflict::record, NFR-5)
Task 35: minor (deferred): тесты counts_are_positive/detach требуют /proc (Linux-only по A2); семантика без /proc — alive=true, counters=0 (как раньше)
Task 35: complete (tests: platform_process 7 + platform_boundary 2, всего 227, clippy clean nightly+1.88, fmt clean; review PASS/APPROVED → minor fix applied)
Task 36: Ruling: семантика путей за фасадом — `platform::paths::{home_dir (directories::BaseDirs), is_under (компонентный starts_with), normalize_for_compare (identity на Unix, fold регистра/слэшей на Windows)}`; `$HOME` больше не читается вне paths.rs (boundary-паттерны `var_os("HOME")`/`var("HOME")`/`env!("HOME")`), `is_under` проведён через registry/tools/watch/admission, `normalize_for_compare` — в root_mismatch
Task 36: fix (minor hardening): `walk::resolve_in_root` тоже переведён на `is_under`; boundary-паттерн HOME уточнён до трёх конкретных форм (без ложных срабатываний на комментарии)
Task 36: minor (deferred): `home_dir_available` зависит от resolvable HOME; Windows-рантайм `windows_key`/BaseDirs не проверяется на Linux (только чистая функция)
Task 36: complete (tests: platform::paths 3 + boundary 2, всего 230, clippy clean nightly+1.88, fmt clean; review PASS/APPROVED → minors applied)
Task 37: Ruling: инвариант шва закреплён в CI (`cargo test --locked --test platform_boundary`) в nightly и release на 1.88; design §5/§7/§13 приведены к коду (tokio io-util/sync/macros, platform deps tokio/serde/directories)
Task 37: fix round по ревью: добавлена запись ledger (acceptance «секция закрыта»); boundary-скан больше не пропускает вложенные каталоги с именем `platform` (исключён ровно `src/platform`)
Task 37: complete (tests: platform_boundary 2, всего 230, clippy clean nightly+1.88, fmt clean; review FAIL(missing ledger, §7) → fixes → scoped re-review)
Phase review T33–T37: вердикт **converged** (все acceptance T33–T37 подтверждены; утечек ОС-вызовов вне platform нет; контракты не менялись)
Phase review fix pass: boundary-guard усилен — bare-паттерны (`UnixListener`, `UnixStream`, `UnixDatagram`, `std::os::unix`, `tokio::net`) ловят grouped/sorted/aliased импорты; `production_prefix` усекает только `#[cfg(test)] mod` (прочие cfg(test)-элементы fail-safe); самотест расширен (4 образца); доки: ADR-9 (spawn остаётся portable), §4 deps `tokio, serde, directories`, module-doc T35/T36 → present, requirements daemon-state row (`socket`, без `started_at`)
Tails batch 1 (deferred minors, no new scope): search_docs — строгий `limit` (Protocol на неверный тип, 0 → пустой результат) и `scope` только `project`; тесты `list_docs limit=0` и bad limit/scope; frontmatter — unquote снимает только парные кавычки (`'Twas` цел), закрывающий `--- ` с хвостовым пробелом (+2 теста); `read_state` без TOCTOU exists→read; fsync перед rename (`daemon.json`, `install.json`, установленный бинарь); `admission_lock_held` fail-closed на EACCES/EMFILE; мёртвый `Response::Stats` удалён (design §5 + roundtrip-тест на ToolResult); artifact-тест уважает `CARGO_TARGET_DIR`; tasks.md T28 → `store/repo.rs`; install/soak harness ждут `state/daemon.json` (флейк `update_waits_for_synchronous_job`)
Tails batch 1: сверка ledger — ранее deferred, уже закрыто последующими задачами: title (T27), chunk_id/FR-23 (T28), corrupt meta и clamp лимита (T27-fix), root_mismatch и watcher visibility (final review fix pass), `.docsbaseignore ..` (T6/T22), install (T25). Остаются открытыми: drain без таймаута, retention `sync_jobs`, cap NDJSON-фрейма/запроса, 10k-doc RSS, macOS/Windows (фаза 3)
Tails batch 1: complete (tests: 234 passed, 4 ignored, clippy clean nightly+1.88, fmt clean; minors-батч без отдельного ревьюера — фаза уже converged)
Task 38: Ruling: Windows backend — транспорт через `interprocess` 2.4.4 (`cfg(windows)`, feature `tokio`; ADR-10), `process_alive` через `windows-sys` OpenProcess; рассмотрены и отклонены tokio-pipes без крейта (ручная ротация инстансов) и AF_UNIX на Windows (неоднородная поддержка); деградации: `secure_*` best-effort no-op, счётчики 0, `remove` пайпа no-op; вне фазы — SDDL-ACL, статический `.exe`, 10k-RSS
Task 38: Ruling: границы «первой рабочей версии» — daemon+CLI+MCP и зелёный `cargo test` на Windows x64 в CI; Linux-поведение и Linux-тесты не меняются; macOS остаётся вне scope
Task 38: complete (docs-only: design §2/§4/§7/§12 ADR-10/§13/§14, requirements NFR-6/FR-33/A2/out-of-scope, tasks.md секция T39–T43 + self-review; T39 verify дополнен локальным кросс-чеком `cargo check --target x86_64-pc-windows-gnu`)
Task 39: Ruling: Windows transport — `interprocess` 2.4.4 (`cfg(windows)`, feature tokio), `Endpoint::Pipe(String)` (serde строкой; на Windows десериализация всегда Pipe, на Unix — Unix), `pipe_name(cache)=docsbase-<blake3→32 hex>` (portable, unit-тесты на Linux); `Client::connect` без exists-precheck; probe=connect, remove=no-op; I/O-таймауты пайпов = no-op (документировано)
Task 39: Ruling: локальная валидация Windows-слоя до CI — scratch-крейт `/tmp/opencode/wincheck` (копия `windows.rs` против `interprocess 2.4.4`), `cargo check/clippy --target x86_64-pc-windows-gnu` чисто; полный `--all-targets` кросс-чек — T40, Windows CI — T43 (staging: windows-ветка `mod.rs` ссылается на T40-элементы)
Task 39: fix round по ревью: `endpoint_serde_roundtrip` получил cfg-ветки Unix/Windows (Unix-версия упала бы на Windows из-за cfg-сплита десериализации); doc `windows.rs` без преждевременного windows-sys
Task 39: minor (deferred): no-op таймауты на пайпах (зависший daemon не отсекается клиентом); `exists`/`connect_probe` = реальный коннект (churn в `wait_for_daemon`); `as_path()` для Pipe без префикса `\\.\pipe\`
Task 39: complete (tests: platform_transport 4 + platform_boundary 2, всего 236, clippy Linux nightly+1.88 и scratch windows-gnu clean, fmt clean; review FAIL(1 deliverable+ledger) → fixes → scoped re-review)
Task 39: fix round 2: `PathBuf` в tests/platform_transport.rs под `#[cfg(unix)]` (на Windows был unused import → падал бы `-D warnings` в T43)
Task 40: Ruling: Windows process/signals/perms — `ShutdownSignal` (ctrl_c/ctrl_close), `process_alive` через `windows-sys` 0.61.2 (OpenProcess+GetExitCodeProcess, least privilege, handle закрывается на всех путях), `detach` = DETACHED_PROCESS|CREATE_NEW_PROCESS_GROUP|CREATE_NO_WINDOW, счётчики fd/thread = 0 (контракт u64), `secure_dir` = create_dir_all (приватность %LOCALAPPDATA%), `secure_file/executable` = no-op, `open_private_log` = create+append; `unsafe_code` allow только на `process_alive` с SAFETY-комментариями
Task 40: Ruling: локальная проверка Windows окна — полный крate-чек: `cargo clippy --locked --target x86_64-pc-windows-gnu --lib --bins -- -D warnings` чист (mingw-w64 локально), плюс `--test platform_{process,transport,boundary}`; интегральные тесты целиком — T42/T43
Task 40: fix round по ревью: ADR-10/§7 уточнили, что windows-sys даёт и флаги detach (std их не экспортирует); doc `windows.rs` в present tense; boundary-самотест пинит windows-API образец (`use windows_sys::...::OpenProcess`)
Task 40: minor (deferred): OpenProcess с EACCES на живом процессе трактуется как «мёртвый» (stale-cleanup при несовпадении прав); на Windows нет I/O-таймаутов (см. T39)
Task 40: complete (tests: platform_process 7 (Linux) + boundary 2, всего 236, Linux clippy clean, windows-gnu clippy/check lib+bins и platform-тесты clean, fmt clean; review PASS/APPROVED → minors applied)
