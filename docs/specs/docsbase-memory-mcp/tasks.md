# docsbase-memory-mcp — Implementation Plan

**Goal:** один Rust-бинарь `docsbase`: per-project индекс `.md`, общий daemon для
нескольких агентов, тонкие stdio-MCP-frontend'ы, явная индексация, BM25-поиск.
**Architecture:** daemon владеет реестром, per-project tantivy-индексами, watcher'ами и
jobs; frontend'ы ходят через local socket (platform transport, ADR-9; JSON-RPC, версия
протокола); индексация явная (`index_project`, `auto_index=false`); источник истины —
файлы, MCP read-only.
**Spec:** `docs/specs/docsbase-memory-mcp/requirements.md` (rev 2)
**Design:** `docs/specs/docsbase-memory-mcp/design.md`
**Constitution:** `docs/specs/constitution.md`

**Global constraints (verbatim из design/constitution):**
- Rust edition 2024, MSRV 1.88; `cargo fmt`; `cargo clippy --all-targets -- -D warnings`.
- Linux/WSL2 x86_64; ОС-зависимый код — только в `src/platform/` (ADR-9); один
  статический бинарь; офлайн, ноль сетевых крейтов.
- `unwrap`/`expect` запрещены в библиотечном коде; `thiserror` в libs, `anyhow` на границе.
- Крейты — только из design.md; в шапке задачи указывать `New crates:` с обоснованием.
- Runtime layout: `$CACHE=~/.cache/docsbase-memory-mcp` (0700), socket 0600; один writer;
  `schema_version`/`protocol_version` версионируются.
- Лимиты: `max_file_size` 1 MiB, `max_docs_per_project` 20 000, debounce 1.5 с, grace 5 с.

**Review focus (самые вероятные failure modes, покрываются тестами владельцев):**
1. Привязка сессии к проекту по `cwd` ошибается (поддиректория, symlink, git root) → агент
   ищет не в том проекте (T19).
2. Гонка старта daemon / orphan после краша → два daemon'а или мёртвый индекс (T17).
3. Watcher теряет правку/переименование или тонет в шторме → устаревшие доки (T22).
4. Chunker рвёт code fence или врёт line-range → битые citations (T8, T27).
5. Краш между commit'ами tantivy и SQLite → расходящийся индекс (T13, R2).

## Standard task loop (применяется к каждой задаче)

1. **RED** — написать перечисленные тесты; `Verify`-команда должна упасть по ожидаемой
   причине (feature missing), а не по опечатке.
2. **GREEN** — минимальная реализация из раздела «GREEN».
3. **Verify** — команда из задачи проходит; затем полный `cargo test`.
4. **Commit** — `git add` только файлов задачи; conventional message.

Каждая задача = одна сессия. Порядок строго по `Depends`. Файлы не выходят за `Files`.

---

### T1 — Scaffold + toolchain/supply-chain pin
**Depends:** —
**New crates:** `clap` — CLI-каркас (design §7).
**Files:** Create `Cargo.toml`, `Cargo.lock`, `rust-toolchain.toml`, `.cargo/config.toml`,
`src/main.rs`, `src/lib.rs`, `.gitignore`; Test `tests/smoke.rs`
**Interfaces:** Produces: бинарь `docsbase`, crate `docsbase_memory`, `docsbase --version`
**RED:** `smoke::(version_prints, toolchain_pinned, min_publish_age_configured)` — падают
(бинарника/файлов нет).
**GREEN:** `clap` derive; `rust-toolchain.toml` → `channel = "nightly"` + rustfmt/clippy;
`.cargo/config.toml` → `global-min-publish-age = "14 days"` + `incompatible-publish-age = "deny"`;
lints из конституции.
**Verify:** `cargo test --test smoke && cargo clippy --all-targets -- -D warnings` → OK
**Acceptance:** constitution (сборка/lints/supply-chain); база для FR-30.

### T2 — Error taxonomy
**Depends:** T1
**New crates:** `thiserror` — таксономия (design §7).
**Files:** Create `src/error.rs`, `tests/error_codes.rs`; Modify `src/lib.rs`, `Cargo.toml`, `Cargo.lock`
**Interfaces:** Produces `enum Error { Admission|Protocol|Project|Index|Query|Internal }`,
`fn mcp_code(&self) -> i32`, `type Result<T>`
**RED:** `error_codes::maps_categories` — проверяет коды `-32010..-32014`, `-32603`; падает.
**GREEN:** варианты + `From<std::io::Error>` с сохранением `#[source]` (маппинг ошибок
`serde_json` добавляется в T5, когда появляется крейт); без unwrap.
**Verify:** `cargo test --test error_codes` → OK
**Acceptance:** NFR-8, design §9 (таксономия).

### T3 — Paths + config
**Depends:** T2
**New crates:** `serde`, `toml`, `directories` — конфиг и XDG-пути; dev `tempfile` — temp-dirs в тестах (design §7).
**Files:** Create `src/config/mod.rs`, `src/config/paths.rs`, `tests/config.rs`; Modify `src/lib.rs`, `Cargo.toml`, `Cargo.lock`
**Interfaces:** Produces `struct Config { ignores, max_file_size, max_docs_per_project,
auto_index: bool /*=false*/, hybrid: bool /*=false*/ }`, `Config::load(project_root: Option<&Path>)`, `paths::cache_dir()/config_dir()`
**RED:** `config::(defaults_when_missing, project_overrides_global, cli_overrides_project,
auto_index_default_false)` — падают.
**GREEN:** загрузка global `~/.config/docsbase-memory-mcp/config.toml`, затем
`<project>/.docsbase.toml`; merge в precedence CLI > project > global > defaults.
**Verify:** `cargo test --test config` → OK
**Acceptance:** FR-28, FR-29; OQ-4; ADR-6.

### T4 — Store: schema + models
**Depends:** T2
**New crates:** `rusqlite` (bundled) — метаданные/реестр (design §7).
**Files:** Create `src/store/mod.rs`, `src/store/migrations.rs`, `src/store/models.rs`,
`tests/store.rs`; Modify `src/lib.rs`, `Cargo.toml`
**Interfaces:** Produces `SCHEMA_VERSION: u32 = 1`, `Db::open(cache)/open_readonly(cache)`,
`Project{id, canonical_root, status, schema_version, …}`, `Doc`, `ChunkMeta`, `SyncJob`;
таблицы `projects/docs/chunks/sync_jobs`
**RED:** `store::(migrate_fresh, reject_newer_schema, readonly_open, recreate_on_missing)`
— падают.
**GREEN:** WAL, `PRAGMA foreign_keys`, миграции v1, статус-энум.
**Verify:** `cargo test --test store` → OK
**Acceptance:** FR-10, FR-16; NFR-3, NFR-7; design §7.

### T5 — IPC protocol types
**Depends:** T2
**New crates:** `serde_json` — payloads (design §7).
**Files:** Create `src/ipc/mod.rs`, `src/ipc/protocol.rs`, `tests/ipc_protocol.rs`; Modify `src/lib.rs`, `src/error.rs`, `Cargo.toml`, `Cargo.lock`
**Interfaces:** Produces `PROTOCOL_VERSION: u32 = 1`, `Request::{Hello, RegisterSession{pid,cwd},
CallTool{name,args}, StopDaemon}`, `Response::{Hello, ToolResult, Error, Stats}`,
`TOOL_ALLOWLIST`, NDJSON `encode/decode`,
`decode_request` явно маппит ошибки парса в `Protocol`, `encode` — в `Internal`,
`CallTool` для неизвестного имени → `Protocol`
**RED:** `ipc_protocol::(roundtrip, unknown_tool_rejected, version_checked)` — падают.
**GREEN:** serde tag/content, length-safe NDJSON, allowlist.
**Verify:** `cargo test --test ipc_protocol` → OK
**Acceptance:** FR-7, FR-33; I8; design §5.

### T6 — `.md` walk с игнорами и границами root
**Depends:** T3
**New crates:** `ignore` — обход, gitignore-семантика (design §7).
**Files:** Create `src/index/mod.rs`, `src/index/walk.rs`, `tests/walk.rs`; Modify `src/lib.rs`, `Cargo.toml`
**Interfaces:** Produces `walk(root: &Path, cfg: &Config) -> impl Iterator<Item = Result<PathBuf>>`;
`resolve_in_root(root, path) -> Result<PathBuf>` (I4)
**RED:** `walk::(finds_md, skips_default_ignores, honors_docsbaseignore, rejects_symlink_escape,
ignore_dotdot_patterns)` — падают.
**GREEN:** `WalkBuilder` с custom ignore filename `.docsbaseignore`; дефолтные игноры;
проверка realpath-префикса; `.md` (case-insensitive).
**Verify:** `cargo test --test walk` → OK
**Acceptance:** FR-14, FR-32; I4, I7; design §9.

### T7 — Frontmatter (flat)
**Depends:** T2
**Files:** Create `src/index/frontmatter.rs`, `tests/frontmatter.rs`; Modify `src/index/mod.rs`
**Interfaces:** Produces `Frontmatter { title: Option<String>, tags: Vec<String> }`,
`fn parse(text: &str) -> (Frontmatter, &str)`; ошибки не фатальны (FR-18)
**RED:** `frontmatter::(absent, flat_keys, malformed_is_ignored, crlf)` — падают.
**GREEN:** опциональный блок `---…---`, только `key: value`, запятые → tags.
**Verify:** `cargo test --test frontmatter` → OK
**Acceptance:** FR-18; A4.

### T8 — Heading chunker
**Depends:** T2
**New crates:** `pulldown-cmark` — парсинг md с offset'ами (design §7).
**Files:** Create `src/index/chunk.rs`, `tests/chunker.rs`; Modify `src/index/mod.rs`, `Cargo.toml`
**Interfaces:** Produces `struct Chunk { doc_id, seq, heading_path: Vec<String>, kind,
line_start, line_end, text }`, `chunk_markdown(body, max_chunk_chars) -> Vec<Chunk>`
**RED:** `chunker::(breadcrumb_path, fence_not_split, table_intact, long_section_split_at_paragraph,
empty_doc, line_ranges_match)` — падают.
**GREEN:** события `pulldown-cmark` с offset'ами; split только на заголовках; длинные
секции — по пустым строкам вне fence; byte→line маппинг.
**Verify:** `cargo test --test chunker` → OK
**Acceptance:** FR-22; NFR-1; R7; design §5 (инвариант chunk).

### T9 — Identifier tokenizer
**Depends:** T1
**New crates:** `tantivy` — FTS + кастомный токенизатор (design §7).
**Files:** Create `src/index/tokenizer.rs`, `tests/tokenizer.rs`; Modify `src/index/mod.rs`, `Cargo.toml`
**Interfaces:** Produces `IdentifierTokenizer` (tantivy `Tokenizer`) + регистрация в
`TokenizerManager`; правила из design §5
**RED:** `tokenizer::(camel, snake, xml_header, dunder, lowercased)` — `defineStore`,
`__bt_tt_getProp`, `X-Request-ID`, `assessment_plan_id`; падают.
**GREEN:** split по non-alphanumeric, границам camelCase/acronym, цифрам; lowercase-фильтр.
**Verify:** `cargo test --test tokenizer` → OK
**Acceptance:** FR-21; design §5.

### T10 — Tantivy schema + index handle
**Depends:** T4, T9
**Files:** Create `src/index/tantivy_index.rs`, `tests/tantivy_index.rs`; Modify `src/index/mod.rs`
**Interfaces:** Produces `IndexHandle::open_or_create(dir)`, `add_chunks(&[Chunk])`,
`delete_doc(doc_id)`, `commit()`, `reader() -> &IndexReader`, `search(&str, limit) -> Vec<Hit>`
(строка, парсится внутри настроенным `QueryParser`; отклонение от эскиза `&Query`
зафиксировано в ledger), `Hit{chunk_id, doc_id, score}`; поля
`text^1.0 title^2.0 heading_path^1.5 identifiers^2.5`
**RED:** `tantivy_index::(add_and_search, delete_by_doc, reload_after_commit,
exact_identifier_beats_prose)` — падают.
**GREEN:** схема, writer с одним экземпляром на проект (I5), `ReloadPolicy::Manual` +
явный `reader.reload()` в `commit()` (в tantivy 0.25 нет `OnCommit`).
**Verify:** `cargo test --test tantivy_index` → OK
**Acceptance:** FR-19, FR-21, FR-26; NFR-1; I5.

### T11 — Full index job
**Depends:** T6, T7, T8, T10, T4
**New crates:** `blake3` — content hash (design §7).
**Files:** Create `src/index/job.rs`, `tests/index_job.rs`; Modify `src/index/mod.rs`, `Cargo.toml`
**Interfaces:** Produces `JobStats{docs, chunks, skipped, removed, errors}`,
`run_full(db, idx, project, cfg) -> Result<JobStats>`; порядок commit: tantivy → SQLite (R2)
**RED:** `index_job::(fresh_corpus, skip_identical, remove_deleted, corrupt_file_nonfatal,
idempotent_rerun)` — падают.
**GREEN:** walk → hash → diff → pythonless pipeline; ошибка файла → warning в stats, job жив;
in-flight marker (`content_hash = ''`) перед tantivy-коммитом для R2-сходимости.
**Verify:** `cargo test --test index_job` → OK
**Acceptance:** FR-16, FR-18; A4; R2; design §6.

### T12 — Project registry
**Depends:** T4
**Files:** Create `src/daemon/mod.rs`, `src/daemon/registry.rs`, `tests/registry.rs`
**Interfaces:** Produces `ensure_project(db, path) -> Result<Project>` (realpath + git root +
валидация I7), `list_projects(db)`, `set_status(db, id, status)`,
`resolve_by_cwd(db, cwd) -> Result<Project>` (I6, ближайший ancestor)
**RED:** `registry::(register_normalizes_root, idempotent, rejects_fs_root_and_home,
rejects_cache_dir, resolve_from_subdir, resolve_unknown)` — падают.
**GREEN:** canonicalize; git root через `.git`-поиск вверх; уникальный индекс по
`canonical_root`; статусы.
**Verify:** `cargo test --test registry` → OK
**Acceptance:** FR-10, FR-13; I6, I7; C8.

### T13 — Incremental job
**Depends:** T11, T12
**Files:** Modify `src/index/job.rs`; Test `tests/index_incremental.rs`
**Interfaces:** Produces `run_incremental(db, idx, project, changed: &[PathBuf]) -> Result<JobStats>`
**RED:** `index_incremental::(changed_file_updates_chunks, unchanged_skipped,
deleted_file_purged, rename_counts_as_delete_plus_add)` — падают.
**GREEN:** hash-compare по затронутым, `delete_doc` старых чанков, upsert; сходимость по
hash при повторе после «краша» (R2).
**Verify:** `cargo test --test index_incremental` → OK
**Acceptance:** FR-16; R2.

### T14 — CLI `docsbase index`
**Depends:** T12, T13
**New crates:** `anyhow` — граница CLI; `fd-lock` — per-command lease; dev `assert_cmd` — CLI-тесты (design §7).
**Files:** Create `src/cli/mod.rs`, `src/cli/index.rs`, `tests/cli_index.rs`; Modify `src/main.rs`, `Cargo.toml`
**Interfaces:** Produces CLI `docsbase index [path]` (default cwd) → печатает `project_id`,
`JobStats`; direct-режим с per-project lease (без daemon)
**RED:** `cli_index::(registers_and_indexes, second_run_incremental, outside_root_error,
lease_blocks_second_writer)` — падают (`assert_cmd`).
**GREEN:** clap subcommand + `run_full`; flock `projects/<id>/.writer.lock` на время команды.
**Verify:** `cargo test --test cli_index` → OK
**Acceptance:** FR-11, FR-30 (частично); I5.

### T15 — CLI read-only snapshot (`search`/`list`/`status`)
**Depends:** T10, T12, T3
**Files:** Create `src/cli/search.rs`, `src/cli/status.rs`, `tests/cli_read.rs`; Modify `src/cli/mod.rs`
**Interfaces:** Produces `docsbase search <query> [--limit]` (JSON на stdout, схема
`{path, heading_path, lines, score}`), `docsbase list`, `docsbase status`
**RED:** `cli_read::(search_without_daemon, not_indexed_message, status_lists_projects)` —
падают.
**GREEN:** `Db::open_readonly` + tantivy reader; при незарегистрированном проекте — текст с
инструкцией `index_project` (C8).
**Verify:** `cargo test --test cli_read` → OK
**Acceptance:** FR-20, FR-23, FR-25, FR-26, FR-30.

### T16 — CLI socket-first routing
**Depends:** T5, T15
**Files:** Modify `src/cli/mod.rs`; Create `src/ipc/client.rs`; Test `tests/cli_routing.rs`
**Interfaces:** Produces `ipc::client::connect(cache) -> Option<Client>`,
`Client::call(Request) -> Result<Response>`; CLI: daemon жив → через socket, иначе direct
**RED:** `cli_routing::(routes_through_daemon_when_alive, falls_back_when_dead)` — падают.
**GREEN:** `UnixStream::connect` с коротким timeout; прозрачный фолбэк.
**Verify:** `cargo test --test cli_routing` → OK
**Acceptance:** FR-30; design §6 (CLI).

### T17 — Daemon lifecycle S1
**Depends:** T5, T4
**New crates:** `tokio` — runtime для socket/watcher/signal (design §7).
**Files:** Create `src/daemon/lifecycle.rs`, `tests/lifecycle.rs`; Modify `src/main.rs`, `src/cli/mod.rs`, `Cargo.toml`
**Interfaces:** Produces `ensure_daemon(cache) -> Result<()>`, `run_daemon(cache) -> Result<()>`,
`stop_daemon(cache) -> Result<()>`; `state/daemon.json`, `daemon.sock`, `daemon.start.lock`;
CLI `docsbase serve`, `docsbase daemon stop`
**RED:** `lifecycle::(first_start_creates_daemon, second_start_reuses, grace_shutdown_after_last_session,
stop_command_terminates, stale_state_recovered)` — падают.
**GREEN:** flock на `daemon.start.lock`; spawn detached; ожидание socket ≤10 с; grace 5 с;
проверка pid во избежание orphan (R5).
**Verify:** `cargo test --test lifecycle` → OK
**Acceptance:** FR-2, FR-6, FR-9; NFR-2; R5; ADR-2.

### T18 — Admission S1
**Depends:** T17
**Files:** Create `src/daemon/admission.rs`, `tests/admission.rs`; Modify `src/daemon/lifecycle.rs`
**Interfaces:** Produces `admission::Lease::acquire(cache, build_id, schema_version) -> Result<Lease>`;
`logs/conflicts.ndjson`
**RED:** `admission::(build_mismatch_refuses_and_logs, schema_mismatch_refuses, lock_recovered_after_kill,
second_daemon_refused)` — падают.
**GREEN:** `admission.lock` flock; сверка `daemon.json`; запись конфликта; отказ до работы.
**Verify:** `cargo test --test admission` → OK
**Acceptance:** FR-4, FR-9; C1/S2; NFR-7.

### T19 — IPC server + session registry
**Depends:** T17, T5, T12
**Files:** Create `src/daemon/server.rs`, `src/daemon/session.rs`, `tests/ipc_server.rs`
**Interfaces:** Produces `SessionRegistry{join, leave, stats}`, сервер: accept → `Hello` →
`RegisterSession{cwd}` → `resolve_by_cwd` (I6) → маршрутизация `CallTool`; EOF → cleanup;
при незарегистрированном проекте и `auto_index=true` — авто-регистрация и запуск
`run_full` (FR-12), иначе `Project`-ошибка с инструкцией
**RED:** `ipc_server::(hello_version_mismatch_rejected, session_bound_to_project_by_cwd,
unknown_tool_rejected, eof_removes_session, project_not_registered_message,
auto_index_registers_and_indexes)` — падают.
**GREEN:** tokio accept-loop; per-session state; `Stats{fd_count, sessions}`; socket 0600.
**Verify:** `cargo test --test ipc_server` → OK
**Acceptance:** FR-3, FR-7, FR-8 (частично), FR-12, FR-33; I2, I6, I8.

### T20 — MCP frontend (stdio proxy)
**Depends:** T19, T16
**New crates:** `rmcp` — MCP SDK (design §7).
**Files:** Create `src/mcp/mod.rs`, `src/mcp/frontend.rs`, `src/mcp/tools.rs`, `tests/mcp_frontend.rs`; Modify `src/main.rs`, `Cargo.toml`
**Interfaces:** Produces `docsbase mcp` — stdio MCP server; tools-схемы из design §8;
ensure_daemon → connect → proxy; stdout только JSON-RPC
**RED:** `mcp_frontend::(initialize_and_list_tools, search_proxied_to_daemon,
stdout_has_no_logs, unknown_tool_not_proxied)` — падают (MCP-клиент в тесте по stdio).
**GREEN:** rmcp handler; tracing в stderr; маппинг `Error::mcp_code`.
**Verify:** `cargo test --test mcp_frontend` → OK
**Acceptance:** FR-7, FR-34; I8; design §5.

### T21 — MCP tools: registry, status, sync jobs
**Depends:** T20, T12
**Files:** Modify `src/daemon/server.rs`, `src/mcp/tools.rs`; Test `tests/mcp_registry.rs`
**Interfaces:** Produces `index_project(path?)`, `list_projects()`, `status()`,
`sync_start(project_id?) -> job_id`, `sync_status(job_id)`; sync-джоб — фоновой `run_full`,
состояние пишется в `sync_jobs` (переживает рестарт daemon), не более одного активного на
проект (FR-17)
**RED:** `mcp_registry::(index_project_registers_and_indexes, list_projects_statuses,
status_reports_sessions_watcher_versions, sync_job_runs_and_reports,
second_sync_start_returns_current, sync_job_persists_across_restart)` — падают.
**GREEN:** handlers поверх registry/job; `tokio::spawn` для фонового job; атомарный переход
`queued → running → done/error` в `sync_jobs`.
**Verify:** `cargo test --test mcp_registry` → OK
**Acceptance:** FR-11, FR-17, FR-25, FR-26; P1.

### T22 — Watcher + инкремент
**Depends:** T13, T17, T19
**New crates:** `notify` — watcher; debounce/коалесценция — собственный quiet-collector (Ruling T22 в progress.md).
**Files:** Create `src/watch/mod.rs`, `tests/watcher.rs`; Modify `src/daemon/lifecycle.rs`
**Interfaces:** Produces `spawn_watcher(project, tx) -> WatcherGuard`; debounce 1.5 с;
коалесценция burst; игнор `projects/<id>/` и cache
**RED:** `watcher::(edit_becomes_searchable_within_2s, burst_one_job, delete_purges,
rename_delete_plus_add, index_dir_ignored)` — падают.
**GREEN:** notify + debouncer; батч изменённых путей → `run_incremental`.
**Verify:** `cargo test --test watcher` → OK
**Acceptance:** FR-15, FR-16; NFR-4; SC-6.

### T23 — Session cleanup hardening + NFR-9
**Depends:** T19
**Files:** Modify `src/daemon/session.rs`; Test `tests/cleanup.rs`
**Interfaces:** Produces `Stats{fd_count, threads, sessions}`; cleanup ≤2 с; per-session
resources (locks, snapshot readers, jobs) освобождаются
**RED:** `cleanup::(kill9_frontend_frees_resources, thousand_cycles_no_fd_growth,
dead_session_does_not_block_shutdown)` — падают.
**GREEN:** EOF + периодическая проверка pid; RAII-guard'ы; подсчёт fd.
**Verify:** `cargo test --test cleanup -- --test-threads=1` → OK
**Acceptance:** FR-8; NFR-9; C6; SC-5 (часть).

### T24 — Version/schema mismatch UX + conflict polish (S2/S3)
**Depends:** T18
**Files:** Modify `src/daemon/admission.rs`, `src/cli/daemon.rs`, `docs`; Test `tests/admission_ux.rs`
**Interfaces:** Produces понятные сообщения «run `docsbase install`» / «index rebuild»;
конфликт-лог дополняется полями `{build_id, schema_version, cache_root, pid}`
**RED:** `admission_ux::(mismatch_message_actionable, conflict_log_fields, rebuild_hint_on_schema_bump)` — падают.
**GREEN:** форматирование + структурная запись.
**Verify:** `cargo test --test admission_ux` → OK
**Acceptance:** FR-4; C1/S2-S3; C4; NFR-8.

### T25 — Install/uninstall + update coordination (S4)
**Depends:** T17, T24
**Files:** Create `src/cli/install.rs`, `tests/install.rs`; Modify `src/main.rs`
**Interfaces:** Produces `docsbase install|uninstall`; install останавливает daemon, ждёт
выхода процессов, подменяет бинарь; uninstall показывает индексы и удаляет после подтверждения
**RED:** `install::(install_owned_artifacts, uninstall_preserves_foreign, uninstall_lists_indexes_with_confirmation,
update_stops_and_waits)` — падают.
**GREEN:** `Admission` write-lease + ожидание sessions=0; owned-манифест.
**Verify:** `cargo test --test install` → OK
**Acceptance:** FR-1, FR-5; C1/S4.

### T26 — Project config + reload semantics
**Depends:** T3, T19
**Files:** Modify `src/config/mod.rs`, `src/daemon/server.rs`; Test `tests/config_runtime.rs`
**Interfaces:** Produces `Config::for_project(root)`; global читается на старте daemon,
project — при открытии проекта (OQ-6); изменение global требует `daemon stop`
**RED:** `config_runtime::(project_config_applied_on_open, global_change_requires_restart_hint,
ignores_from_project_config)` — падают.
**GREEN:** кэш конфигов по проекту + инвалидация при открытии.
**Verify:** `cargo test --test config_runtime` → OK
**Acceptance:** FR-28, FR-29; OQ-6.

### T27 — Citations + boosts + golden SC
**Depends:** T10, T15
**New crates (dev):** `insta` — golden-снапшоты (design §7).
**Files:** Create `tests/fixtures/bench/` (корпус), `tests/search_golden.rs`; Modify `src/index/tantivy_index.rs`, `src/cli/search.rs`
**Interfaces:** Produces citation `{path, heading_path, lines, score}`; бусты из T10;
штраф за чанк > `max_chunk_chars`
**RED:** `search_golden::(exact_identifier_top3, error_message_top1, semantic_top3,
code_api_top3)` — падают (SC-1..SC-4).
**GREEN:** подбор бустов на фикстурах; снапшоты через insta.
**Verify:** `cargo test --test search_golden` → OK
**Acceptance:** SC-1…SC-4; FR-19, FR-26, FR-20.

### T28 — `list_docs` pagination + `read_neighbors`
**Depends:** T21
**Files:** Modify `src/mcp/tools.rs`, `src/store/mod.rs`; Test `tests/mcp_docs.rs`
**Interfaces:** Produces `list_docs(limit, cursor)`, `read_neighbors(chunk_id, before, after)`,
`get_doc(path)`; `get_doc` внутри roots (I4)
**RED:** `mcp_docs::(pagination_stable, neighbors_window, get_doc_rejects_escape,
get_doc_outside_project)` — падают.
**GREEN:** keyset-пагинация по `(rel_path)`; выборка соседей по `seq`; realpath-check.
**Verify:** `cargo test --test mcp_docs` → OK
**Acceptance:** FR-23, FR-24, FR-25; FR-32; P4.

### T29 — Error mapping + non-fatal file errors
**Depends:** T2, T11
**Files:** Modify `src/error.rs`, `src/index/job.rs`, `src/daemon/server.rs`; Test `tests/error_surface.rs`
**Interfaces:** Produces стабильные MCP-коды; `status` показывает warnings по файлам
**RED:** `error_surface::(index_error_does_not_fail_job, mcp_error_codes_stable,
project_error_has_instruction)` — падают.
**GREEN:** маппинг из design §9; warning-агрегация.
**Verify:** `cargo test --test error_surface` → OK
**Acceptance:** FR-18; NFR-8; design §9.

### T30 — Perf benches (NFR-1)
**Depends:** T27
**New crates (dev):** `criterion` — бенчмарки (design §7).
**Files:** Create `benches/search.rs`, `benches/index.rs`, `tests/perf_budget.rs`
**Interfaces:** Produces замеры: search p95 ≤200 мс на 50k чанков; индекс 1 000 md ≤30 с
**RED:** `perf_budget::(search_budget, index_budget)` — падают на заглушке/медленной версии.
**GREEN:** профилирование и оптимизации (allocation, batch commit) без смены стека.
**Verify:** `cargo test --test perf_budget --release && cargo bench` → в бюджете
**Acceptance:** NFR-1, NFR-2; SC-7.

### T31 — Soak + offline CI
**Depends:** T20, T22, T23, T25
**Files:** Create `tests/soak.rs`, `tests/offline.rs`, `.github/workflows/nightly.yml`
**Interfaces:** Produces soak-сценарий 3 агента + watcher 1 час; offline-проверка (network
namespace / отсутствие connect)
**RED:** `soak::(three_agents_one_daemon_no_corruption)`, `offline::(no_network_syscalls)` — падают.
**GREEN:** стабилизация + nightly workflow.
**Verify:** `cargo test --test soak -- --ignored && cargo test --test offline` → OK
**Acceptance:** SC-5, SC-8; NFR-5.

### T32 — Release/static artifact hardening (NFR-6, NFR-2)
**Depends:** T30
**Files:** Modify `Cargo.toml` (release profile), `.cargo/config.toml`, `.github/workflows/release.yml`; Test `tests/artifact.rs`
**Interfaces:** Produces release-бинарь; проверка отсутствия запрещённых динамических зависимостей и бюджета размера
**RED:** `artifact::(static_link_check, size_budget)` — падают, пока профиль не настроен.
**GREEN:** ADR в design.md о способе статики (`x86_64-unknown-linux-musl` или `+crt-static`);
release profile `lto = true`, `codegen-units = 1`, `strip = true`, `panic = "abort"`.
**Verify:** `cargo release-static && ldd target/x86_64-unknown-linux-gnu/release/docsbase && stat -c%s target/x86_64-unknown-linux-gnu/release/docsbase` → в бюджете (ADR-8)
**Acceptance:** NFR-6, NFR-2.

---

## Task-list self-review

1. **Spec coverage:** FR-1→T25; FR-2→T17; FR-3→T19; FR-4→T18/T24; FR-5→T25; FR-6→T17;
   FR-7→T5/T20; FR-8→T19/T23; FR-9→T17/T18; FR-10…FR-13→T12/T14/T21; FR-14→T6;
   FR-15→T22; FR-16→T11/T13; FR-17→T21; FR-18→T7/T11;
   FR-19→T10; FR-20→T15/T27; FR-21→T9/T10; FR-22→T8; FR-23→T28; FR-24→T28; FR-25→T28;
   FR-26→T21; FR-27→сознательно не в v1; FR-28/29→T3/T26; FR-30→T14/T15/T16;
   FR-31→не в v1 (фаза 3); FR-32→T6/T28; FR-33→T19; FR-34→T20. NFR-1→T30; NFR-2→T17/T30;
   NFR-3→T4/T10; NFR-4→T22; NFR-5→T31; NFR-6→T32; NFR-7→T4/T18; NFR-8→T2/T24/T29;
   NFR-9→T23. SC-1…4→T27; SC-5→T31; SC-6→T22; SC-7→T30; SC-8→T31; SC-9→T27; SC-10→T12/T21.
2. **Type consistency:** `Project`, `Chunk`, `JobStats`, `Request/Response`, `IndexHandle`,
   `run_full/run_incremental`, `resolve_by_cwd`, `TOOL_ALLOWLIST` используются одинаково
   во всех задачах.
3. **Review focus:** 5 failure modes привязаны к T19, T17, T22, T8/T27, T13.
4. **Proportion:** план не содержит тел функций — только сигнатуры, имена тестов и команды.
5. **Phase 3 prep (T33…T37):** ADR-9 platform seam — behavior-preserving рефактор без
   новых FR/крейтов/wire-изменений; секция идемпотентно проверяема `platform_boundary`
   (T37) и не меняет acceptance T1…T32.

## Открытые риски плана

- C1 (полный CBM-протокол) разбит на T17/T18/T24/T25 (S1→S4); при перерасходе первым
  режется T24, затем S3-часть T18 — с ADR.

---

## Phase 3 prep — platform seam (T33…T37)

**Goal:** изолировать весь ОС-зависимый код (local transport, signals, process
lifecycle, fs-permissions, path semantics) в `src/platform/` за внутренним `pub`-фасадом
(design ADR-9), не меняя поведение Linux, wire-контракты, `schema_version` и
CLI/MCP-поверхность. Новых крейтов нет; macOS/Windows-реализации — фаза 3.
**Depends:** T1…T32 (фаза 1 converged, commit 4587f9d).
**Review focus (failure modes):** (1) шов протекает — ядро импортирует `std::os::unix`;
(2) transport меняет NDJSON-фрейминг или таймауты; (3) `Endpoint` ломает совместимость
`daemon.json`; (4) path-семантика меняет containment-проверки (FR-32, I4); (5) рефактор
задел поведение T17–T25 (lifecycle/session/cleanup).

Инвариант секции: вне `src/platform/` нет transport/process/perms-вызовов ОС;
проверяется `tests/platform_boundary.rs` (паттерны расширяются в T34/T35) и
grep-шагом в CI (T37).

### T33 — Spec delta: ADR-9 + план (docs-only)
**Depends:** T32
**New crates:** —
**Files:** Modify `docs/specs/docsbase-memory-mcp/design.md` (§2, §3, §4, §5, §7, §12
ADR-9, §13, §14), `docs/specs/docsbase-memory-mcp/tasks.md` (эта секция),
`docs/specs/docsbase-memory-mcp/progress.md`
**Interfaces:** Produces ADR-9 и задачи T34…T37; код и тесты не меняются
**RED:** — (docs-only; инвариант проверяется начиная с T34)
**GREEN:** delta в текущий спек: ADR-9, §5-sketch фасада, компонент/дерево/трассируемость `platform/`,
global constraints; отдельная спека не заводится
**Verify:** `grep -n "ADR-9" docs/specs/docsbase-memory-mcp/design.md docs/specs/docsbase-memory-mcp/tasks.md`
→ ссылки в §2/§4/§12/§13/§14 и T33…T37
**Acceptance:** решения зафиксированы (швы без смены поведения, модуль в крейте,
тесты на фасад); phase-3 follow-ups перечислены в ADR-9.

### T34 — Transport seam (`Endpoint`, listener/stream фасад)
**Depends:** T33
**New crates:** —
**Files:** Create `src/platform/mod.rs`, `src/platform/unix.rs`, `tests/platform_transport.rs`,
`tests/platform_boundary.rs`; Modify `src/lib.rs`, `src/ipc/client.rs`, `src/ipc/mod.rs`,
`src/daemon/server.rs`, `src/daemon/lifecycle.rs`; Test `tests/cli_routing.rs`,
`tests/mcp_frontend.rs`, `tests/ipc_server.rs`, `tests/cleanup.rs`,
`tests/admission_ux.rs`, `tests/install.rs`, `tests/lifecycle.rs`
**Interfaces:** Produces `pub mod platform` (`pub` как остальные внутренние модули, ADR-9),
`platform::Endpoint` (serde как строка; для Unix — путь, совместим с `daemon.json.socket`),
`platform::daemon_endpoint(cache)`, `bind/accept/connect_blocking/connect_probe/remove/exists`,
`Listener`/`Stream` (Stream: `AsyncRead + AsyncWrite`, сервер использует `tokio::io::split`),
`BlockingStream::set_read_timeout` (сохраняет `SO_RCVTIMEO`-таймауты `Client`);
Consumes tokio `net`/`io-util`; `ipc::client::socket_path` заменяется на
`platform::daemon_endpoint` (breaking для lib-потребителей — допустимо в 0.1.0)
**RED:** `platform_transport::(roundtrip_and_remove, endpoint_serde_roundtrip)` — падают
(модуля нет); `platform_boundary::(no_os_transport_outside_platform)` — падает на `ipc/daemon`;
boundary-скан: `src/**` без `src/platform/**` и без `#[cfg(test)]`-блоков, паттерны
`std::os::unix::net`, `tokio::net::Unix`, `std::os::unix::net` в импортах
**GREEN:** перенос Unix-кода как есть; тест-фейки (fake daemon) — через `platform::bind`
**Verify:** `cargo test --test platform_transport --test platform_boundary && cargo test && cargo clippy --all-targets -- -D warnings` → OK
**Acceptance:** `ipc/`/`daemon` не импортируют `std::os::unix::net`/`tokio::net::Unix`;
NDJSON-фрейминг, IO-таймауты, порядок handshake и содержимое `daemon.json` (поле `socket`
строкой) не изменились.

### T35 — Process/signals/permissions seam
**Depends:** T34
**New crates:** —
**Files:** Modify `src/platform/unix.rs`, `tests/platform_boundary.rs` (расширить паттерны);
Create `tests/platform_process.rs`; Modify `src/daemon/lifecycle.rs`, `src/conflict.rs`,
`src/daemon/session.rs`, `src/store/mod.rs`, `src/cli/install.rs`;
Test `tests/lifecycle.rs`, `tests/admission.rs`, `tests/store.rs`, `tests/install.rs`,
`tests/cleanup.rs`
**Interfaces:** Produces `platform::{ShutdownSignal, process::{detach, process_alive,
fd_count, thread_count}, fs::{secure_dir, secure_file, secure_executable, open_private_log}}`;
Consumes tokio `signal`
**RED:** `platform_process::(alive_detects_self, alive_rejects_dead_pid, private_log_is_0600)`
— падают (модуля нет); boundary-паттерны process/perms (`/proc/`, `SignalKind`,
`process_group`, `PermissionsExt`, `DirBuilderExt`, `OpenOptionsExt`) — падают на
`lifecycle/conflict/session/store/install`
**GREEN:** перенос `/proc/{pid}/stat` (`lifecycle::pid_alive` → `platform::process::process_alive`,
session использует его же), `/proc/self/{fd,task}` (`fd_count`/`thread_count`),
chmod 0700/0600/0755 (`secure_dir`/`secure_file`/`secure_executable`/`open_private_log`),
`process_group(0)`, `SignalKind`; `#[cfg(unix)]`-блок `store/mod.rs::ensure_private_dir`
заменяется на `platform::fs::secure_dir`; boundary-скан исключает `#[cfg(test)]`
(в `session.rs` тестовый `child.kill` — не ОС-слой)
**Verify:** `cargo test --test platform_process --test platform_boundary && cargo test && cargo clippy --all-targets -- -D warnings` → OK
**Acceptance:** счётчики fd/thread остаются `u64` (0 на не-Linux); режимы 0600/0700 и
SIGTERM/SIGINT-grace не изменились; `install`-chmod переведён на `secure_executable`;
`secure_*` идемпотентны на существующих путях.

### T36 — Path semantics seam
**Depends:** T34
**New crates:** —
**Files:** Create `src/platform/paths.rs` (unit-тесты внутри); Modify `src/platform/mod.rs`,
`src/daemon/registry.rs`, `src/daemon/tools.rs`, `src/daemon/admission.rs`, `src/watch/mod.rs`
**Interfaces:** Produces `platform::paths::{home_dir, is_under, normalize_for_compare}`;
windows-нормализация (case-insensitive key) — чистая функция под
`#[cfg(any(windows, test))]` (тестируется на Linux)
**RED:** unit `paths::(home_dir_available, is_under_rejects_siblings, windows_key_normalizes)`
— падают (модуля нет)
**GREEN:** `directories::BaseDirs::home_dir` вместо `$HOME` (`registry::home_dir`);
`is_under` в `resolve_by_cwd`/`normalize_root` (`registry.rs`), `get_doc`
(`tools.rs:152`), containment watcher'а (`watch/mod.rs:inside_root`/`is_pruned`);
`normalize_for_compare` в root_mismatch-сравнении (`admission.rs`)
**Verify:** `cargo test platform::paths && cargo test --test registry --test watch --test admission && cargo test` → OK
**Acceptance:** `$HOME` не читается вне `platform/paths.rs` (`config/paths.rs` уже через
`directories::ProjectDirs`); containment-семантика Linux (FR-32, I4) и `root_mismatch`
не изменились.

### T37 — CI guard + docs sync
**Depends:** T34, T35, T36
**New crates:** —
**Files:** Modify `.github/workflows/nightly.yml`, `.github/workflows/release.yml`,
`docs/specs/docsbase-memory-mcp/design.md` (§7/§13 при расхождениях),
`docs/specs/docsbase-memory-mcp/progress.md`
**Interfaces:** Consumes `tests/platform_boundary.rs`; Produces обязательный CI-шаг
**RED:** — (wire-up: boundary-тест существует с T34/T35)
**GREEN:** шаг `cargo test --locked --test platform_boundary` в nightly и release
workflow; синхронизация design §7/§13; ledger
**Verify:** `cargo test --test platform_boundary && cargo fmt --check` → OK
**Acceptance:** инвариант шва проверяется в CI (nightly + release); секция закрыта в ledger.

**Self-review секции:** T34→T35/T36 (T36 не зависит от T35) и все зависят от T33;
каждая задача независимо верифицируема `Verify`-командой; новых крейтов и wire-изменений
нет; macOS/Windows impl и транспортный ADR фазы 3 сознательно вне секции; пропорция —
сигнатуры, имена тестов и команды, без тел функций.
