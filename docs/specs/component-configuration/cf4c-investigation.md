# CF4c — safe registry observation before migration CLI

Дата: 2026-11-12. Исследование после CF4b; **не завершённый design/implementation
plan**, runtime/API здесь ещё не меняются. Base: CF4b f314efc, PR132.

## Что должен доказать следующий slice

1. Registry содержит ВСЕ зарегистрированные projects, включая неоткрытые,
   неиндексированные и отсутствующие на диске roots. Current cwd/open sessions
   не являются полным inventory.
2. Root membership взято из одного короткого SQLite snapshot. Sources читаются
   отдельно с exact-byte revisions; изменение registry после observation должно
   стать stale precondition, не невидимым расширением migration scope.
3. Captured cache/config roots привязаны к plan identity. Header schema observation
   CF2/CF4b не доказывает registry rows и не заменяет runtime generation.
4. Missing source отличается от observed empty source. Ошибка чтения не превращается
   в Missing; чтение ограничено существующим source cap, ошибки редактируются.
5. Observation не создаёт DB/directories/WAL/SHM/journal/conflict logs, не вызывает
   recovery/checkpoint, не запускает daemon/model/provider. Не читает vector payload
   и не копирует всю metadata DB ради нескольких registry rows.
6. Registry count/encoded bytes, SQLite page/WAL-index memory и lock wait должны
   иметь явные budgets; unknown/over-limit/recovery states остаются blocked.

## Уже проверенные варианты

| Вариант | Проверено | Решение |
|---|---|---|
| Нынешний `Db::inspect_readonly` | Header100bytes, metadata и отсутствие sidecars | Сохранить для schema fact; registry rows он не читает |
| Operational `Db::open_readonly` | Может писать conflict log/sidecars | Не использовать в dry-run |
| SQLite `mode=ro&readonly_shm=1` | Реальный bundled SQLite3.53.2 создал empty WAL и затем ошибся | Недостаточно для no-write даже на error path |
| `immutable=1` на исходной DB | Не доказывает отсутствие writer/WAL/checkpoint races | Не использовать как shortcut |
| Copy/deserialize всей metadata DB | Память/диск зависят от document payload, snapshot consistency тоже нужна | Не подходит для bounded metadata observer |
| Самописный SQLite page parser | Нужны B-tree/overflow/WAL/recovery/validation semantics | Не вводить параллельный storage engine |
| Только переданные roots/cwd | Не перечисляет registry | Может быть explicit partial input, но не complete survey |
| Existing list_projects IPC | Читает operational Db, current DTO включает лишние данные | Не считать готовым pure observer |
| Persisted registry manifest | Нужен cross-store dirty/publication/recovery protocol и старые writers | Не добавлять незаметно отдельный source of truth |

### Воспроизведение SQLite side effect

Throwaway Rust probe `/tmp/cf4c-readonly-probe.rs`, собранный Rust1.88 с реальным
librusqlite из target/deps (`rusqlite0.40.2`, `libsqlite3-sys0.38.2`, bundled
SQLite3.53.2). Создать metadata.db в WAL, table projects/one row; закрыть writer.
Открыть URI `file:<path>?mode=ro&readonly_shm=1` с READ_ONLY|URI|NO_MUTEX,
выполнить SELECT count(*) FROM projects, закрыть reader. Результат:

```text
SQLite 3.53.2 / before [("metadata.db", 8192)]
Result Err(SqliteFailure(... CannotOpen, extended_code: 14 ...))
After [("metadata.db", 8192), ("metadata.db-wal", 0)]
```

Probe удаляет только собственную temp directory. Log:
`/tmp/cf4c-readonly-probe.log`. Независимая проверка Python/system SQLite3.46.1
показала тот же результат; она не подменяет bundled доказательство.

Причина подтверждена исходником bundled sqlite3.c:
`sqlite3WalOpen` около69161 всегда начинает с READWRITE|CREATE|WAL;
`readonly_shm` управляет только shared-memory open. `unixOpenSharedMemory` около
45219 делает O_RDONLY для shm, но уже созданный WAL остаётся. Windows
`winOpenSharedMemory` около53080 также учитывает readonly_shm. Номера относятся
к pinned3.53.2, не к универсальному SQLite API.

## Направление для проверки, ещё не принято как production design

Read-denying-write VFS adapter над bundled native VFS может запретить create/write/
truncate/delete ДО OS calls. Нужна проверка всего I/O пути, не только open flags:

- все WAL/journal/temp xOpen должны исключать CREATE/WRITE, xDelete запрещён;
- xShmMap/xShmUnmap нельзя доверять writable mapping, уже открытому другой
  connection того же процесса; readonly URI alone не изолирует process cache;
- recovery of unreliable SHM может строить heap WAL-index по всему WAL — такой
  fallback тоже должен быть bounded или возвращать unavailable;
- отсутствующий WAL у clean closed DB не разрешает подменить его пустым файлом
  без реального исключения concurrent checkpoint/writer. Virtual empty WAL с
  private SHM сам по себе небезопасен: writer не видит reader marks;
- exclusive DB lock требует корректного OS handle/locking, может конфликтовать
  с долгоживущим WAL connection. Нельзя обещать совместную работу по одному тесту;
- VFS callback ABI/allocation/alignment/registration lifetime, no unwind через C,
  per-call limits и cleanup требуют отдельной safety justification и native tests;
- не вводить global mutable config или hidden fallback. Source write/consent/
  stop-daemon не принадлежат observer.

До выбора механизма complete registry остаётся unavailable. Это не физическая
невозможность и не повод выдавать частичный список за полный; следующий шаг —
ограниченный throwaway experiment с no-write seam и реальными writer/sidecar
сценариями, затем маленький выбранный production contract.

## Обязательные characterization cases

Missing DB/cache; clean closed WAL DB; active writer с committed WAL; missing SHM
при WAL; hot journal; future/mismatched schema; unreadable DB; same-process writer;
separate-process writer; concurrent create/checkpoint/unlink; count/byte/page caps;
no file changes on success/error; no stale main-only read; two cache roots with
identical header schema; non-UTF8/malformed source paths; incomplete sources;
interrupt/cancel/close. Tests должны проверять реальные SQL rows и filesystem
state, а не только enum flags или mock observations.

### Throwaway VFS seam: 3 Linux scenarios

`/tmp/cf4c-vfs-probe.rs`, actual bundled3.53.2, отдельный throwaway binary.
Wrapper меняет только xOpen (force READ_ONLY, remove CREATE/WRITE/DELETEONCLOSE)
и xDelete (deny); сохраняет native callbacks/app data. Это **намеренно неполная**
реализация: не подменяет file I/O/shared-memory methods и не задаёт memory limits.

| Сценарий | SQL result | Files после reader close |
|---|---|---|
| Closed clean WAL DB без sidecars | CannotOpen | DB bytes/path inventory неизменны; WAL не создан |
| Живой writer в том же процессе | count=1 | **SHM bytes изменились** — probe assertion RED |
| Живой writer в другом процессе | count=1 | DB/WAL/SHM bytes/path inventory неизменны |

Fixtures/child writer создавались только во временной директории и удалены.
External writer использует тот же bundled binary; READY/stdin handshake держит
его connection открытым до сравнения снимков. Logs `/tmp/cf4c-vfs-{closed,same,
external}.log`. Это доказывает необходимость guards для shared mappings или
явно изолированного инспектора; одного xOpen gate недостаточно. Ни native
Windows/macOS, ни concurrent checkpoint/limit failure ещё не проверялись.

### Further narrowing: no custom VFS registration

Guarded file-method prototype (not production) makes all three prior scenarios
preserve bytes; logs `/tmp/cf4c-vfs-guarded-{closed,same,external}.log`. However,
a dynamically registered VFS introduces allocation/registration lifetime and
same-process lookup concerns. It is not the preferred first production slice.

Two further bundled3.53.2 Linux probes established a narrower direction:

- `/tmp/cf4c-exclusive-probe.rs`: native VFS xOpen READWRITE without CREATE,
  SHARED→EXCLUSIVE locks; under that lease, a no-sidecar immutable read succeeds,
  competing same-process writes fail BUSY, and an existing writer refuses lease.
  A separate guard descriptor still leaves path-replacement identity questions.
- `/tmp/cf4c-connection-lease-probe.rs`: open a PRIVATE_CACHE connection with
  immutable=1 + READWRITE without CREATE; obtain THAT connection's native file
  via SQLITE_FCNTL_FILE_POINTER and acquire SHARED→EXCLUSIVE on the same file.
  The OS descriptor supports locking while SQLite's pager reports read-only.
  No SQL queries precede the lease/sidecar checks. Reader drop closes native file
  and releases partial/full locks. Closed DB returns one real registry row;
  pre-existing same-process writer returns BUSY; competing writes after SELECT
  also fail BUSY. A gated change of page size4096→1024, auto_vacuum and insertion
  after connection open but before lock returns the two NEW rows. All observer
  scenarios preserve files. Logs `/tmp/cf4c-connection-lease-{closed,same,pagesize}.log`.

Source audit: bundled sqlite3.c pager open around64553 opens native descriptor
before immutable switches pager to read-only/noLock; BtreeOpen around75822 reads
only bootstrap header before any SQL, with pPage1 initially null. lockBtree
around76418 reads current page1; page-size reconciliation around76525. Stock
SQLite default opening also reads bootstrap header before its first transaction.
These details need pinned characterization tests, not an unsupported assumption
that immutable on a live WAL DB is safe.

Candidate initial contract: observe a quiescent, sidecar-free metadata DB under
this connection-owned nonblocking exclusive lease; active writers/recovery remain
Unavailable. Bound row count/path bytes/SQLite value and page-cache policy explicitly;
read schema/registry only after lease and recheck namespace/sidecars before publish.
Known native VFS only. On Unix explicitly check SQLITE_FCNTL_HAS_MOVED (native
case around44390); Windows native open shares READ|WRITE but not DELETE (around
54198), which requires native rename/lock tests. No source-of-truth manifest,
copy of document payload, VFS registry, or process-global settings/context map.
This remains a candidate until an implementation plan/safety note/tests are written.
