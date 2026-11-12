# I133 — schema observation сохраняет SQLite locks

Дата: 2026-11-12. Ограниченное исправление #133 после CF4c1; исполняется inline
по executing-plans/TDD, без implementation delegation. New crates: none.

**Цель:** `Db::inspect_readonly` не снимает POSIX locks других connections, не
пишет DB/sidecars и остаётся bounded schema-only observer.

**Выбор:** переиспользовать существующий native lease внутри store, добавить
fixed 100-byte xRead через тот же borrowed sqlite3_file. Raw File open/close
отклонён воспроизведённым lock loss. Operational reader отклонён из-за recovery/
sidecar effects. Child-process inspector потребовал бы отдельный execution/IPC
lifecycle. Native Windows retries и handle-to-namespace checks наследуются из CF4c1.
Private lease остаётся в текущем файле, доступ расширяется только до
`crate::store`; public API и имена существующих тестов не меняются.

**Контракт:** settings validate before I/O; metadata-only missing/nonregular
preflight; directory canonicalization и recheck; immutable pager + native lease
без SQL/schema loading; xRead ровно100bytes, metadata size/mtime и native namespace
проверки до/после; sidecars/занятая DB/unsupported backend → Unknown. Missing и
orphaned-sidecar NeedsRecovery сохраняются. Ready — schema identity, не reader.
Readonly permissions могут дать Unknown: exclusive native lease требует пригодного
handle. Нет implicit recovery/fallback. Это уточнение прежнего CF2 no-SQL contract:
SQLite handle теперь открыт, SQL statements по-прежнему не исполняются.

**Files:** Modify `src/store/{inspect.rs,mod.rs}`, private
`src/store/registry_inspect{.rs,/lease.rs,/tests.rs}`; add
`tests/schema_inspection_locks.rs`; dated requirements/design/tasks/progress delta.

## Task I133

- [x] RED: public schema observation при rollback reader позволяет отдельному
  процессу UPDATE; перед observation тот же UPDATE получает BUSY. Evidence
  `/tmp/issue133-red.log`. Test uses actual Db/project and native SQLite, no mocks.
- [x] Private lease exposes `pub(in crate::store)` ownership/methods and
  `header(&self) -> Result<[u8;100], UnavailableReason>`. xRead callback/FILE_POINTER
  checked, SAFETY documents live connection and fixed writable buffer; no pointer
  escapes, SQL, new VFS registration or native descriptor outside SQLite.
- [x] `inspect::schema_with` observes two fixed headers on the same leased handle;
  metadata preflight handles nonregular files before open, cache aliases rechecked.
  `observe` no longer owns a Rust File. Keep existing identity/version encoding.
- [x] Tests preserve external reader lock through schema inspection and permit
  write after its release. Also invoke schema inspection during the existing
  registry lease and require an external writer still BUSY. Keep symlink/FIFO/
  schema mismatch/live WAL/no-write coverage. Existing changed-header fixture
  uses explicit raw test mutation because a cooperating SQL writer is now blocked;
  do not weaken production lock guarantees to make a mutation fixture work.
- [ ] Full cli-full, focused lean, full/lean Clippy/fmt; fresh task review/fixes,
  task ledger/commit; fresh final review and required native CI/inventory audit.
  PR closes #133, refs #78; merge only after reviewed head and all required checks.

Constraints: MSRV1.88, native Linux/macOS/Windows; no production unwrap/expect/panic,
new crates, baselines/threshold changes or provider/model/network activity.
