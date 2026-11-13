# CF5b2a: source replacement и native metadata

2026-11-13. Research перед product publication seam. Основание:
[CF5b1](control-contract.md), [план этапа](source-publication-plan.md).
No source-mutation API или activation в этом этапе нет.

## Выбранный путь и отклонённые упрощения

Нужен private stage на том же volume и в удерживаемом target parent namespace,
полная запись+metadata+flush до replacement, затем native namespace operation,
parent durability и проверка фактического результата. Внешний executor удерживает
полный stopped admission и оставляет журнал pending до всех доказательств.
Нет copy-across-volume, eager index/vector rewrite или provider work. Работа
ограничена зарегистрированными configuration sources, а не документами корпуса.

In-place overwrite сохраняет inode/часть attributes, но process failure может
оставить частичные bytes, которые не являются ни Old, ни New. Это не выбранный
путь. Обычный temp+rename без metadata переносит inode stage: атрибуты старого
target автоматически не переносятся. Private before/after bytes в SQLite этого
не исправляют. Двухфазный rename старого target в backup создаёт missing-target
окно и дополнительный recovery namespace; без отдельного доказательства не нужен.

Перед replacement stage должен иметь admitted content+expected attributes.
Неподдерживаемый/read-inaccessible metadata domain — typed refusal до source
mutation; нельзя молча отбрасывать ACL/xattrs или менять владельца. Private stage
не становится публичным ради сохранения исходного permissive mode. Первый
product adapter должен явно определить поддерживаемые private-file attributes;
всё остальное блокируется либо требует отдельного видимого migration action.
Нынешний Windows `secure_file` — no-op и такого доказательства не предоставляет.

Linux listxattr documentation также разрешает не перечислять недоступные caller
attributes. Поэтому даже successful enumeration всех **видимых** names не
доказывает отсутствие скрытых metadata namespaces. CF5b2b должен явно определить
admitted metadata contract и способ его доказательства; нельзя назвать такой
список полным preservation proof. Этот вопрос не решается копированием одного
user xattr из scratch и остаётся prerequisite product adapter.

## Платформенные факты и границы

| Платформа | Что подтверждено | Чего пока недостаточно |
|---|---|---|
| Linux | rename заменяет target атомарно для path readers; stage inode становится target; file fsync не заменяет parent fsync; scratch ниже проверяет user xattr/old descriptor/process boundaries | Это не полное metadata/namespace/power-failure доказательство |
| Linux | Missing target допускает renameat2(RENAME_NOREPLACE), EEXIST сохраняет неожиданно появившийся file | Нет общей CAS для existing target; полного metadata/ACL/namespace/storage proof пока нет |
| macOS | Apple fsync docs отличают buffer flush от F_FULLFSYNC с drive-cache flush | Архивная документация не доказывает актуальный APFS adapter; требуется native probe и platform acceptance |
| Windows | ReplaceFileW сохраняет DACL, security attributes, encryption/compression и часть streams; WRITE_THROUGH flag у него не поддерживается | Ни successful return, ни выбор MoveFileExW автоматически не доказывают required source+directory durability/attributes; требуется отдельный adapter/proof |

Linux existing-target comparison+rename не является CAS. Session/config leases
исключают только cooperating callers. Held parent handle и nofollow не означают
защиту от всех внешних parent renames/независимых installations. Supported namespace
scope, native identity rechecks и отказ при unknown должны оставаться явными.
NFS/remote filesystem guarantees нельзя вывести из Linux syscall имени или
запуска эксперимента на локальном overlay. Failure/unsupported flush не понижает
гарантию до best effort и не разрешает strict commit.

## Что требуется добавить в control format до executor

Schema1 CF5b1 хранит byte/state research. До CF5b3 необходим reviewed versioned
extension, не reinterpretation прежнего blob:

- Для Existing — native original file identity и expected target attributes;
  для Missing — явная absence и derived private attributes нового файла.
- Перед/после attributes представлены platform-tagged bounded encoding, включены
  в observation/plan/consent binding. POSIX owner/group/mode и поддерживаемые
  xattrs/ACL; Windows DACL/прочие admitted attributes и streams требуют explicit
  support/refusal. Один `parent_identity` или Unix mode не заменяет этот capture.
- Имена/значения attributes могут быть binary/secret; length-first capture,
  aggregate accounting, private journal/stages, никакого Debug/JSON/log payload.
  Exact numeric caps/SQLite record/file/RAM peaks выбираются и проверяются вместе
  с полной enumeration; schema1 byte limits нельзя автоматически переиспользовать.
- Durable stage intent: операция/source/parent/name и expected bytes/attributes
  до создания. Stage native identity фиксируется до его публикации. Future
  generated IDs не являются пользовательскими settings и не меняют plan intent.
- Сбой после stage creation до записи его identity оставляет uncertain stage:
  recovery не удаляет произвольный совпавший файл по имени/bytes. Требуется явная
  ownership/recovery классификация или остановка для manual resolution; повторные
  попытки не создают неограниченное число orphan stages.
- Связь stage/backup/target identities, progress, очищаемых side paths и retention
  хранится так, чтобы crash до progress row не делал replacement невидимым.
  Automatic cleanup не следует за одним return code или совпадением content hash.

Byte классификация Old/New/Both сохраняется, но становится необходимой частью
полной проверки identities+attributes. Both не даёт повода переписать unchanged
target. Неожиданный existing target для planned Missing не перезаписывается.
Attribute mismatch не превращается в допустимое совпадение только из-за bytes.
Ожидаемые generated stage identities и пользовательские before identities имеют
разные lifecycle; их нельзя сравнивать как один immutable original inode.

## Result contract и порядок доказательств

До attempt replacement ошибка может доказать, что target прежний; private stage
и его cleanup всё равно учитываются отдельно. После **попытки** namespace mutation
ошибка означает inspect/recovery-required, пока owning adapter не доказал actual
state. Особенно это относится к Windows ReplaceFileW с documented alternate-name
outcomes и сетевым filesystem ошибкам. Даже success требует проверок.

После rename parent flush может завершиться ошибкой при уже новом target. Поэтому
низкоуровневый API не должен возвращать такую ошибку как «ничего не изменено».
Ни failed operation, ни process exit не дают terminal strict/legacy proof.
Прежний журнал сохраняется, ordinary opens блокируются, explicit recovery повторно
читает actual source/stage/backup states под полным admission.

Протокол должен раздельно проверять: stage data/attributes flush; stage intent/
identity journal durability; actual namespace change; target/parent durability;
target verification; per-source progress; финальную aggregate verification и
terminal control commit. Успешный flush не заменяет identity/attribute checks.

## Linux scratch evidence

Исходник [source_publication.rs](experiments/source_publication.rs) компилируется
только в отдельном scratch с existing locked libc0.2.191, Rust1.100.0. Это не
publisher implementation: он копирует один известный synthetic user xattr,
использует private fixture directory и path APIs; full enumeration/ACL/native
parent capabilities/control journal/session admission отсутствуют.

| Experiment | Результат |
|---|---|
| Naive rename | Новый inode/0600 mode, user xattr старого target утрачен; old descriptor сохраняет его |
| Copy known xattr перед fsync/rename | Известное значение сохранено, old open reader читает old bytes, current path — new bytes; owner/group совпадают только в same-owner fixture |
| Parent flush EIO после real rename | Инъецированный error, но actual target уже new и stage path отсутствует |
| Process exit до rename / после rename до parent flush / после parent flush | Exit81/83/82 без destructors: Old+stage / New без stage / New без stage; actual bytes/known xattr проверены после выхода |
| Missing-target no-replace | EEXIST; unexpected target и stage bytes сохранены |
| Bound/nofollow/hardlink | Ограниченный xattr capture отказывает; symlink и nlink2 target отвергаются до staging |

Нет physical disk fault/power loss; EIO — explicit injection. Проверенные fsync
вызовы вернули success на Linux sandbox storage, но не доказывают hardware flush,
APFS или Windows. Открытый old descriptor — наблюдение конкретного rename, не
проверка многопоточной atomicity всех файлов. Full multi-file migration остаётся
control protocol с blocked startup, а не atomic filesystem transaction.

Воспроизведение из repository root:

```sh
cf5_source_scratch=$(mktemp -d)
mkdir "$cf5_source_scratch/src" "$cf5_source_scratch/runs"
cp Cargo.lock "$cf5_source_scratch/Cargo.lock"
cp docs/specs/component-configuration/experiments/source_publication.rs "$cf5_source_scratch/src/lib.rs"
cat > "$cf5_source_scratch/Cargo.toml" <<'TOML'
[package]
name = "docsbase-source-experiment"
version = "0.0.0"
edition = "2024"
rust-version = "1.100.0"

[dev-dependencies]
libc = "=0.2.191"
TOML
CF5B2_SCRATCH="$cf5_source_scratch/runs" CARGO_TARGET_DIR="$cf5_source_scratch/target" \
  cargo +1.100.0 test --offline --manifest-path "$cf5_source_scratch/Cargo.toml" -- --nocapture
```

Первый run7/0/0, включая helper и6substantive tests; затем уточнена отдельная
process boundary между rename и parent flush. Final evidence фиксируется в ledger.
Scratch `/tmp/docsbase-source-prototype`; logs `/tmp/cf5b2a-source-first.log` и
`/tmp/cf5b2a-source-clippy.log`. Product dependencies/lock/test inventory и реальные
source files не меняются. Code/commands включены для воспроизведения вне сессии.

## Первичные references, проверены 2026-11-13

- https://www.man7.org/linux/man-pages/man2/rename.2.html — atomic path replacement,
  открытые descriptors, symlinks, NOREPLACE и ограничения/NFS errors.
- https://www.man7.org/linux/man-pages/man2/fsync.2.html — file и directory durability
  различаются; success предполагает соблюдение storage/device flush contract.
- https://www.man7.org/linux/man-pages/man2/listxattr.2.html — access policy может
  скрыть имена attributes; length query не резервирует неизменный список.
- https://developer.apple.com/library/archive/documentation/System/Conceptual/ManPages_iPhoneOS/man2/fsync.2.html
  и [fcntl](https://developer.apple.com/library/archive/documentation/System/Conceptual/ManPages_iPhoneOS/man2/fcntl.2.html)
  — архивные primary semantics fsync/F_FULLFSYNC; не current APFS acceptance.
- https://learn.microsoft.com/en-us/windows/win32/api/winbase/nf-winbase-replacefilew
  — unsupported WRITE_THROUGH, preserved attributes. IGNORE_MERGE_ERRORS и
  IGNORE_ACL_ERRORS **допускают** successful replacement с потерей ACL информации;
  выбранный adapter обязан запрещать эти flags. Error namespace нужно инспектировать.

CF5b2a закрывает этот bounded research вопрос. CF5b2b/c, CF5b3/5c и #78 не завершены.
