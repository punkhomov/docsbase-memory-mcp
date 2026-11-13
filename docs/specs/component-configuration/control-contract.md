# CF5b1: контракт управляющего состояния и допуска к миграции

Дата: 2026-11-13. Продолжение [CF5b0](adoption-recovery.md). Это design gate и
изолированный эксперимент, без product API, записи пользовательских источников,
v2 startup или activation. База исходников: main e911fd2; CF5a — reviewed PR149.

## Граница решения

Первый executor выполняет только stopped initial source/store adoption и его
resume/restore. Он не меняет vector/chunk identities, данные индекса, generation,
scoring или провайдеров. `strict` означает постоянное владение vector policy и
завершённую миграцию конфигов; это не readiness корпуса и не CF11 publication.
Работа другого агента по server/IPC/CLI остаётся отдельной поставкой.

Проверка только внутри Db::open слишком поздняя: Config::global/for_project могут
быть вызваны до неё. Проверка одного daemon PID/admission также недостаточна:
прямые CLI, vector handles и старые captured Config переживают эту проверку.
Поэтому будущая admission boundary предшествует чтению конфигов и удерживается
до освобождения всех порождённых ресурсов. Это обязательный prerequisite CF5c,
а не уже существующее свойство BootstrapContext.

## Namespace и постоянная отметка

Поддерживаемый домен — cooperating callers одной установки с явными canonical
cache/config/data roots. Для первого executor эти три директории уже существуют;
initialization отсутствующих roots — отдельная операция до нового planning.
Root и parent identities удерживаются native handles и проверяются до/после I/O.
Пути — абсолютный UTF-8, без NUL/`..`; Windows case/volume aliases сравниваются
по native identity, а не lowercase строки. Final symlink/reparse и hardlinked
config targets отклоняются. Missing source допускается только при существующем
проверенном parent. Exact platform handle/replacement proof остаётся CF5b2.

Постоянная отметка — `<cache>/state/config-activation/`. Внутри неё `control.db`,
отдельно от disposable vectors.db и registry. Coordination gate files находятся
снаружи: `<cache>/state/config-session.lock` и
`<data>/locks/config-session.lock`. Их никогда не unlink/recreate.

| Наблюдение под session gates | Обычный startup |
|---|---|
| activation directory отсутствует; v1 sources | Legacy допускается |
| directory есть, DB отсутствует/неполна/повреждена/неизвестна | Блокируется; explicit recovery |
| sealed `preparing`, `applying`, `restoring` | Блокируется; explicit resume/restore |
| валидное `strict` | Только managed context с совпадающей полной policy |
| валидное `restored_legacy` | Legacy допускается после явного полного restore |
| sidecar `-journal`/`-wal`/`-shm`, unsealed operation, namespace mismatch | Блокируется |

Отсутствие directory после первой durable отметки не считается допустимым способом
сброса режима. Кооперативный код её не удаляет; CF5b2 обязан доказать durability
создания самой отметки до source replacement. Прямая внешняя потеря всего cache
namespace неразличима с новой установкой по одному cache. Поэтому v2 startup
также требует managed control state: v2 sources никогда не запускают legacy/default
fallback. Удаление одновременно всех sources и namespace внешним инструментом
не входит в cooperating guarantees.

`namespace_id` — BLAKE3 domain `docsbase-config-namespace-v1` над length-prefixed
canonical root strings и versioned native root identities. Повторное открытие
проверяет сами поля, не только digest. Aliases, указывающие на тот же root,
разрешаются к одному gate. Новый data/config override при прежнем cache не
обходит latch: roots mismatch блокирует до capture/construction.

Два cache с общим data root разделяют data gate; cache gate координирует разные
data overrides для одного cache. Установки с разными cache **и** data roots,
которые разделяют source files, в первом executor не поддерживаются. Его явная
admission precondition — единственный installation domain для этих sources;
не заявляется глобальная координация произвольных независимых installations.
Эта граница показывается в maintenance diagnostics, а не выводится из PID.

## Session lifetime и порядок locks

Будущая `ConfigSession` владеет двумя native shared gates и проверенной namespace
projection. Их получает launch boundary после захвата bootstrap paths, **до**
любого TOML read/provider/Db/vector construction. Gate paths canonicalized и
берутся в общем порядке по UTF-8 path bytes; одинаковые physical gates deduplicate.
Сравнение native root identities ловит alias split. Отказ — fail-fast, без backoff
или автоматического daemon stop. Gate open использует private regular nofollow
file; primitive — стабильный `File::try_lock_shared/try_lock` Rust1.100.

Legacy и managed sessions одинаково удерживают shared gates через всю работу.
Child Db, VectorStore, prepared index/watch context и background work несут
`Arc` proof; drop внешнего caller не освобождает admission раньше этих handles.
Prepared Config без proof после adoption не принимается managed constructors.
Новая session перечитывает current sources, не использует старый ambient Config.
Строгий legacy constructor проверяет persisted mode под shared gates даже после
drop последнего strict owner. Нельзя добавить gate только в CLI и оставить
обход через library constructors. Pure parsing/resolution может оставаться без
I/O, но его результат сам по себе не является construction/admission proof.

Maintenance выполняет этот порядок, используя только nonblocking acquisition:

1. Оба session gates exclusive в общем порядке. При отказе отпустить все guards.
2. Native daemon admission lock; проверка не создаёт conflict diagnostics и не
   вызывает нынешний Lease::acquire с его дополнительными effects.
3. Registry no-write native exclusive lease на существующей DB, либо явно
   observed Missing. Использовать нынешний `registry_inspect/lease.rs` protocol:
   immutable private pager + SHARED→EXCLUSIVE native SQLite xLock **того же**
   descriptor, проверенный native VFS/method table и namespace. До открытия и
   после lock sidecars должны отсутствовать; иначе отдельное recovery до plan.
   Захватить полную bounded inventory и удерживать этот handle через publication.
   Не использовать operational Db::open, SQL BEGIN, journal_mode setter или второй
   File descriptor этого DB: его close может снять POSIX locks процесса.
   Immutable здесь допустим только после native exclusive admission и отсутствия
   sidecars; он не используется, чтобы пропустить WAL recovery.
4. Все project writer leases по возрастанию project_id, включая read users;
   затем exclusive `vectors.lock` существующим mode primitive.
5. Config-write guards для canonical source targets, в общем порядке path bytes.
   Connect/models должны получать session shared **до** своих write guards;
   прежний hash raw aliases больше не считается достаточным protocol.
6. Контрольная DB: короткие отдельные transactions. Никогда не удерживать её SQL
   transaction через filesystem replacement, provider work или ожидание lock.

После пункта5 initial adoption повторно обследует все source bytes, Missing,
registry inventory, roots, settings и весь original plan/consent context.
Stale/failure отпускает guards до создания activation directory/control DB.
Native registry lease не выполняет migrations/checkpoint/recovery, проверяет
namespace/sidecars при release. При Missing registry создание запрещено; current
callers, включая register, должны участвовать в session gate. Resume/restore
вместо совпадения original source bytes проверяют persisted journal/namespace/
consent binding и actual Old/New/Both classification: ожидаемая смесь после crash
не является stale initial plan. Registry snapshot всё ещё должна совпадать.

Существующие lower-level lease orders normal callers не требуется массово менять:
они берут session shared первыми. Maintenance уже удерживает exclusive, поэтому
новые нормальные операции до внутренних locks не дойдут. Старый процесс или
библиотека, ещё не участвующие в session protocol, не поддерживаются; lower locks
дают дополнительный отказ, но не заменяют эту precondition. При shutdown session
живёт до физического окончания work, включая timeout/cancel хвосты.

У registry и control разные SQL connections; только control выполняет короткие
write transactions. Долгий bounded local native maintenance reservation на registry
допустим только для stopped source adoption: provider calls, downloads, corpus
walk/reindex и online generation apply здесь запрещены.

## Точная SQL форма и decode

[control-state-v1.sql](control-state-v1.sql) — research schema1:
`application_id=0x44424346` (`DBCF`), `user_version=1`, page size4096, STRICT tables,
foreign_keys=ON. Она независима от registry schema (DB3 предлагается в PR153).
Connection policy:
DELETE journal, synchronous=EXTRA (прочитать обратно:3), busy_timeout=0,
trusted_schema=OFF, mmap_size=0; без extension loading. Unknown VFS/schema/settings
не допускают ordinary open. Product implementation обязана явно установить и
проверить эти значения на каждой writable connection, а не полагаться на defaults.

- `namespace`: одна permanent immutable row, exact canonical roots и native IDs.
- `operation`: максимум одна текущая операция; identity, plan/observation/consent,
  полная registry snapshot, source count/totals, **вся** vector policy
  (`max_entries`, `max_bytes`, `busy_timeout_ms`) и state/seal.
- `source`: global ordinal0, затем projects по project_id; exact target/parent,
  existence и private before/after bytes+BLAKE3, progress hint. Missing — NULL;
  empty present file — zero-length BLOB с hash, это разные состояния.

Schema1 — точный **research** byte/state contract, без target attribute images.
CF5b2 обязан до CF5b3 executor расширить versioned journal format ожидаемыми
native target attributes/absence и всеми данными для их восстановления, определить
bounded encoding (включая ACL/xattrs, если они поддерживаются) и hash/consent
binding. Одного parent_identity для этого недостаточно. Следующие требования
проверки attributes — gate этого расширения; schema1 сама их не обеспечивает.

Операция создаётся одной transaction: namespace при первом adoption, preparing+
sealed0, все sources, затем sealed1. Seal проверяет count/totals и global row.
Нельзя commit sealed0; observer отвергает её как unknown. Initial insertion,
seal, invalid transitions, policy/image immutability защищены SQL constraints/
triggers. BEFORE INSERT guards дополнительно запрещают replacement существующих
namespace/operation rows: DELETE guards сами по себе обходятся через INSERT OR
REPLACE при recursive_triggers=OFF. Защита не зависит от этого PRAGMA.
Terminal state требует соответствующих progress hints. **Ни trigger,
ни progress не доказывают filesystem состояние**: перед terminal commit executor
повторно проверяет реальные bytes/attributes/namespace/durability всех sources.

`registry_snapshot` — versioned deterministic bounded encoding существующей
observation: Missing либо supported schema+полная упорядоченная project inventory,
canonical roots и relevant revision fields. Его comparator сверяет actual
captured projection, не timestamps/row count. `operation_id` — BLAKE3 domain
`docsbase-config-adoption-v1` над namespace, plan/observation/consent IDs и exact
ordered source identities/hashes/existence. Encoding — byte-length-prefixed
UTF-8 и fixed-width little-endian integers, NULL с отдельным presence tag;
никаких неоднозначных join strings или serialization order из HashMap.
Семантический decoder сверяет fingerprints и data fields, а также scopes,
unique project IDs/dense ordinals, hashes, root relationships и complete inventory.
SQL schema не заменяет эти проверки.

Limits — явно проверяемые границы формата1, а не новые runtime defaults:
до257sources (global+256projects), суммарно16MiB before и16MiB after;
**вся** root/path/ID/registry metadata суммарно до1MiB. Это сохраняет текущий
planning bound16MiB на все исходники, а не 256 отдельных файлов по1MiB.
Увеличенный caller planning limit не разрешает переполнить journal format;
отказ происходит до latch/source writes. Numeric policy values должны быть
представимы в signed SQLite integers; busy timeout0..i32::MAX миллисекунд.

Decoder сначала проверяет file/page cap и фиксированные scalar counts/lengths/
`typeof`, затем читает admitted blobs; LIMIT258 обнаруживает лишние rows.
Одна SQLite row может содержать оба16MiB images: SQLITE_LIMIT_LENGTH34MiB
(32MiB images + до1MiB metadata + запас для record encoding);
SQLITE_LIMIT_SQL_LENGTH64KiB; page cache256KiB, max_page_count16384 (64MiB DB),
mmap0. Aggregate metadata/source bounds проверяются в приложении до записи и
повторно до decode allocation, не только per-cell CHECK. Точные preflight RAM/
per-volume disk peaks для retained images, rollback journal, stages и backups
принадлежат CF5b2/3; cap DB не означает cap всех одновременно живых файлов.

Blobs содержат возможные credentials. Private permissions обязательны до первой
записи, включая DB/journal/stages/backups; SQL tracing, Debug и JSON этих полей
запрещены. Safe projection выводит IDs/state/counts и typed reasons. Format1
хранит не больше одной операции, без растущей history. После strict history не
сверяется с текущими editable settings: обычный low-risk v2 edit не является
повреждением adoption. Изменение persisted owner policy — будущий explicit apply.

## Переходы и recovery

| До | Допустимый следующий state | Предварительное доказательство |
|---|---|---|
| Нет latch | preparing+sealed1 | Admission, свежий consent, полный private durable journal |
| preparing | applying | Journal durable; source staging/publication разрешены |
| applying | strict | Все actual after sources/namespace/policy verified+durable |
| preparing / applying | restoring | Явный restore, actual sources классифицированы |
| restoring | restored_legacy | Все actual before sources/namespace verified+durable |
| strict | Нет в initial executor | Обычный drop не меняет mode; restore запрещён |
| restored_legacy | Новая preparing operation | Новый plan/consent, fresh capture, атомарная замена journal rows |

Возобновление в applying/restoring не обязано менять state. Source progress —
лишь подсказка, которую recovery перепроверяет. Каждый target независимо
классифицируется Old/New/Both/Neither/Missing относительно existence+exact bytes;
для before/after Missing состояние учитывает absence. Both — неизменённый source,
не основание переписать его. Neither/неожиданная symlink/parent/attribute identity
блокируют: чужую правку нельзя затереть сохранённой копией. Known Old/New после
crash допускают идемпотентный explicit resume или restore под полным admission.
Ошибка после namespace change не равна отсутствию изменения.

После restored_legacy следующая попытка заменяет source+operation rows в одной
transaction, сохраняя permanent namespace row/directory. Crash откатывает её
к старому restored record; не существует видимого legacy окна с частично новой
операцией. После strict очистка секретных images — отдельный explicit bounded
retention primitive с сохранением strict proof, вне schema1 mutation API.
Автоматического удаления DB/directory, retry loop или legacy fallback нет.

No-write control inspection берёт только уже существующие gates без CREATE;
control DB при отсутствующем gate означает unavailable. При отсутствующих обоих
gate/latch observational planning может предложить initial adoption без создания
файлов, как нынешний migration_plan: observation явно не atomic и требует полной
повторной проверки после creating admission. Такой plan не является startup proof.
Обычный startup сначала получает создающий session admission и лишь затем
проверяет mode. Control inspection проверяет sidecars до SQL и после capture.
Не использует immutable, чтобы игнорировать hot journal.
При sidecar/corruption — typed unknown/needs_recovery без попытки исправления.
Ordinary startup так же отказывает; writable SQLite recovery выполняется только
явной maintenance/recovery операцией под exclusive gates. Recovery самой SQLite
восстанавливает последний committed record, после чего всё равно нужна recovery
частично применённых source files. Нельзя смешивать эти два уровня.

## Точный allowlist admission effects

Decline/no-TTY/missing consent выполняются до создающего admission: zero writes.
No-write plan/inspect также zero writes. После valid consent, но до final stale
check, допускаются **только** coordination dirs/files:

- `<cache>/state/`, `config-session.lock`, `admission.lock`;
- `<data>/locks/`, `config-session.lock` и canonical-target config lockfiles
  `<fnv1a(raw canonical target bytes):016x>.lock`;
- `<cache>/projects/`, `<id>/`, `.writer.lock` для complete observed inventory;
- `<cache>/vectors.lock`.

Не допускаются activation directory/control.db, conflict diagnostics, config temp,
backup, registry/vector writes/recovery/checkpoint, install/model/provider state.
Существующие lockfiles не truncate/unlink/chmod. Parent namespace и private-file
creation проверяются. Если нынешний helper делает больше allowlist, executor не
может его использовать без отдельной корректировки. Полный stale-before/after
tree assertion станет обязательным causal test реализации CF5b3.

## Native эксперимент и незакрытые gates

[Отчёт и воспроизведение](experiments/control-journal.md) используют pinned
rusqlite0.40.2/bundled SQLite3.53.2 и Rust1.100.0 в отдельном scratch. Они проверяют
реальные PRAGMA/SQL constraints, hot rollback journal после process exit,
read-only отказ без изменения bytes, explicit writable recovery, committed state
после process exit и native shared/exclusive межпроцессное исключение.

Linux experiment не доказывает power-failure durability, namespace CAS, полный
observer, semantic hash validation, весь caller protocol или Windows/macOS.
Required native suite этого docs PR проверяет прежний продукт; scratch experiment
не добавлен в product test inventory. До activation остаются обязательны:

1. CF5b2: directory latch/staging/replacement+attributes+flush proof на всех3OS,
   reviewed versioned journal extension для expected native target attributes;
   disk-full, unsupported flush и ambiguous rename outcomes. Unsupported storage
   обязан отказать, даже если это задерживает activation на платформе.
2. CF5b3: product control-store decoder/executor, real source recovery и полный
   allowlist/causal crash matrix с bounded resource accounting.
3. CF5c: ConfigSession во **всех** daemon/direct/embedded/config-writer constructors;
   strict error вместо нынешнего Shared global-config fallback. Обе стороны
   session/maintenance contention, retained child handles, aliases и old captured
   Config проверяются реальными subprocess tests. Только после этого activation.

CF5b1 фиксирует выбранный контракт, но CF5/CF7/FR-13–16/#78 остаются открыты.
