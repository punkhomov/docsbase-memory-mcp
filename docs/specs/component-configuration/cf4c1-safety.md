# CF4c1: native inspection lease safety argument

Дата: 2026-11-12; bundled SQLite3.53.2, rusqlite0.40.2, libsqlite3-sys0.38.2.
Это аргумент для ревью реализации и characterization tests, не обещание поддержки
произвольного VFS или записи напрямую в файлы в обход SQLite locking protocol.

## Владение и FFI

`LockedConnection` владеет одной private rusqlite Connection. FILE_POINTER и
VFS_POINTER возвращают заимствованные pointers внутрь этой connection; они
используются только синхронно при её жизни и не выдаются наружу. Connection
NO_MUTEX не разделяется между потоками (`Connection` не Sync); callback table
и function pointers проверяются. Новых C callbacks/allocations/VFS registrations,
динамических глобальных таблиц или process-global settings не добавлено.

Известны только native `unix` и `win32`. Их xLock вызывается SHARED→EXCLUSIVE
сразу после open и до любого SQL. Это реальные locks того же sqlite3_file, через
который затем читаются rows, а не дополнительный descriptor с pathname race.
SQLite inode bookkeeping учитывает другие native connections того же процесса;
OS locks — отдельные процессы. Ошибка любого уровня закрывает Connection и
native file, освобождая partial/PENDING/full locks. `xClose` принадлежит SQLite;
в Rust нет самодельного освобождения чужого pointer или дополнительного fd.

## Почему native READWRITE не означает SQL-запись

Flags не содержат CREATE. URI содержит только escaped filename и immutable=1;
пользовательские `?/#/%/backslash` не могут добавить параметры. Native descriptor
открывается с поддержкой exclusive OS locking. В SQLite pager immutable включает
read-only/noLock; `is_readonly(main)` проверяется до lease. Код не вызывает SQL до
native lease и отсутствия sidecars. Pager не снимает external lock между SELECT;
закрытие native file освобождает его в конце. Read-only pager отклоняет SQL writes.

Bundled source: `sqlite3PagerOpen` около64553 открывает descriptor перед immutable
branch около64594; `sqlite3BtreeOpen` около75822 читает только bootstrap header,
pPage1 остаётся null. `lockBtree` около76418 впервые читает настоящий page1;
page-size reconciliation около76525. Между open и lock header мог измениться,
поэтому тест меняет page size4096→1024, auto_vacuum и membership в этом промежутке
и требует новые rows после lease. Registry queries не используют до-lock rows.

Это НЕ immutable shortcut для live/WAL-bearing database: native exclusivity и
отсутствие WAL/SHM/journal обязательны до чтения. При active writer, unknown facts,
recovery или недостаточных правах возвращается Unavailable, без recovery/fallback.

## Namespace и граница snapshot

Unix HAS_MOVED должен подтвердить текущий native filename до и после capture;
неизвестный backend/control отклоняется. Windows native open использует
FILE_SHARE_READ|FILE_SHARE_WRITE без FILE_SHARE_DELETE (sqlite3.c около54198);
исключение NOTFOUND применяется только к win32. Нативный Windows test проверяет
запрет переименования и самого файла, и его parent directory при открытом handle.
Sidecars повторно проверяются перед публикацией. Это snapshot observation, не
долговременная готовность cache: future apply обязан перепроверять revision.
Произвольные writers, игнорирующие SQLite locks и напрямую переписывающие bytes,
не входят в database-coherence contract, как и у operational SQLite readers.

## Budgets

Лимиты явные, без Default/fallback: row count; retained UTF-8 context/paths + scalar
IDs/schema; SQLite individual-value limit; soft page-cache KiB target. При LIMIT+1
или byte excess нет Ready(partial). Borrowed row text проверяется до PathBuf copy.
Массив не reserve(max_projects); выделения пропорциональны фактически принятым rows.
Mmap выключен как фиксированная стратегия observer. Весь DB/vector/document
payload не копируется. Объектные overhead и внутренний SQLite schema heap не
выдаются за измеренный/жёсткий process RSS cap. Nonblocking lease не вводит retry
budget или неявное ожидание, отличие от operational busy timeout намеренное.

## Evidence

- Initial RED: absent registry_inspect API.
- Behavioral RED: removing native lock levels makes competing-write regression
  fail (write succeeds). Restoring the lease makes it pass.
- Real persisted projects include unindexed/missing-on-disk roots.
- Before/after filesystem contents and directories agree on success/error paths.
- Gated same-process writer after open refuses capture and leaves its SHM unchanged;
  write after observer error succeeds, proving partial-lock release.
- Same- and separate-process competing writes under capture return BUSY; after
  observer closes, write succeeds.
- Unix file replacement before lock refuses capture; Windows namespace semantics
  await its native test, not a Linux mock.
- Count/byte exact boundaries, SQLite value cap, URI escaping, symlink, future
  schema and actual external WAL writer have dedicated tests.

До final native CI/review утверждение о завершении CF4c1 не делается. CLI migration,
active-WAL snapshots, source writes/consent и apply не входят в этот slice.

### POSIX descriptor-close correction

Metadata-only preflight does not reuse Db::inspect_readonly. Regression opens a
DELETE-journal reader transaction, confirms an external writer gets BUSY, invokes
observation, then requires the writer still gets BUSY. The initial raw-header
preflight failed this test: closing its unrelated descriptor released the process
locks. SQLite-native handles use deferred close bookkeeping and pass. Independent
raw opens/closes by other callers remain outside this boundary and the existing
header inspector needs its own correction (#133). URI slash escaping also prevents a
leading `//` from being interpreted as a URI authority (behavioral RED→GREEN).

### Проверка конкретного Unix locking backend (review correction)

Имя VFS `unix` само по себе недостаточно: на macOS autolockIoFinder выбирает
методы по filesystem. В pinned SQLite3.53.2 только posixIoMethods имеет xShmMap;
все no-lock/dotfile/flock/AFP/NFS/proxy/sem tables имеют null. Поэтому до первого
xLock observer проверяет iVersion>=2 и non-null xShmMap для распознанного Unix
VFS. Сам xShmMap не вызывается. Unsupported table не получает ни одного xLock,
не создаёт `.lock` directory и не выдаёт Ready. Это discriminator конкретных
bundled tables, не обещание для arbitrary plugin VFS. Bundled build не задаёт
SQLITE_PREFER_PROXY_LOCKING, open flags не содержат SQLITE_OPEN_AUTOPROXY.

Causal RED→GREEN использует реальные `unix-none` и `unix-dotfile` native tables
(именно их macOS finder может выбрать под именем `unix`); проверяет refusal
до lock и неизменность DB/directory inventory. Не используются mock callbacks
или process-global VFS registration. Source: sqlite3.c45980–46145,46157–46209.

### Symlink ancestors

NOFOLLOW SQLite проверяет и ancestors, включая macOS `/var -> /private/var`.
Observer canonicalize только cache directory, не DB entry; SQLite открывает DB
с NOFOLLOW внутри разрешённого каталога. Sidecar/namespace checks относятся к
этому каталогу; перед Ready повторная canonicalize caller cache должна совпасть.
Retarget alias после open даёт Changed, а revision сохраняет exact caller context.
Тесты: actual ancestor symlink (RED→GREEN), alias retarget after open, DB symlink
rejection, long persisted root превышает value cap, допускающий normal schema.
