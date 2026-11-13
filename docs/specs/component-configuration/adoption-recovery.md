# CF5b0: миграция источников и постоянное владение vector store

Статус: исследование по исходникам, 2026-11-13. Runtime-возможность и исполнимый
storage-контракт не заявлены. База: main 6826aaf и reviewed CF5a PR149; последний
предоставляет consent decisions, но не executor.

Первое применение source/store migration выполняется явной локальной maintenance
операцией при остановленных потребителях, с постоянным recovery state. Для журнала
выбрана отдельная private SQLite DB. Точные schema, namespace/lease protocol и
native durability proofs рассматриваются в CF5b1 до mutation APIs и startup activation.

## Проверенные факты об исходниках

- SourceLayer/SourcePreview используются только для planning; Config::global
  разбирает v1. Shared::open сохраняет legacy fallback к defaults при ошибке global
  config. Managed/strict startup нужна отдельная явная граница отказа.
- VectorStore::acquire эксклюзивно владеет vectors.lock, загружает shared LRU и
  открывает vectors.db без постоянного adoption. После освобождения последнего
  handle legacy open снова допустим. Legacy получает shared mode lease до SQLite.
- Production sync/index/status/watcher принимают VectorCacheSource и prepared
  stages. Прежний consumer blocker устранён; persisted strict mode/executor ещё нет.
- MigrationDraft предлагает source edits и shared-budget adoption с Destructive
  grant. Для них не нужны provider calls, embedding, новые chunk/vector identities
  или копирование корпуса/индекса.
- observe::migration_plan возвращает private MigrationPlan с safe JSON. Source
  capture bounded/nofollow; registry обследуется дважды. Это не atomic snapshot;
  байты предлагаемых edits не экспортируются.
- with_config_write_lock хеширует исходные path bytes в data/locks: aliases могут
  выбрать разные locks. Управляющий caller должен нормализовать namespace;
  connect/model writers должны участвовать в будущем maintenance protocol.
- connect пишет через fs::write+secure_file+rename без fsync; model writer вызывает
  file fsync до rename. Готового cross-platform durable multi-file primitive нет.
  Это факты аудита, а не исправление этих writers в данном slice.
- Daemon admission исключает демоны. Direct index/read leases действуют per project;
  остальные direct CLI/vector users не исключаются одной проверкой daemon liveness.
- Изменения registry должны быть исключены стабильной SQLite transaction/snapshot
  во время maintenance. Её владение отлично от no-write observer. PID check или
  заимствованного результата наблюдения недостаточно.

## Решение и альтернативы

Единственный owner manifest без before-image journal не восстанавливает частично
переписанные global/project files. Online generation switch требует отдельного
CF10/CF11 и не даёт общей транзакции для TOML на разных volumes. Filesystem JSON
journal возможен, но требует собственного directory durability/bounded decode/
recovery. Выбран уже используемый SQLite без нового crate; его transaction не
охватывает source files.

Начинаем со stopped maintenance. Требуются native admission, writer leases всех
затронутых проектов, эксклюзивный vector-mode lease и упорядоченные config-write
 guards. При contention — отказ; автоматически останавливать чужой процесс или
ждать model work нельзя. Current/new cooperating callers проверяют persisted
maintenance state до значимых mutations. Держать source/registry admission через
bounded local source I/O допустимо; provider calls отсутствуют. Queries и v1
startup не проходят через частично применённую миграцию.

Private activation/journal DB находится под state отдельно от disposable vectors.db
и metadata generations. Schema/version явные. Выбран DELETE journal с проверенным
PRAGMA synchronous=EXTRA; pinned SQLite/native proof остаётся обязательным gate.
До первого source replacement журнал уже durable. Он содержит operation ID, точные
cache/config roots, registry identity, consent/observation identities, полную vector
policy, before/after bytes+hashes, existence, progress и persisted mode. В source
blobs/backups могут быть секреты: нужны private permissions; Debug/JSON/log projection
исключается.

Состояния для доказательства: no adoption, preparing, applying, strict active,
restoring, blocked/unknown. Incomplete/unknown journal после process death блокирует
и legacy, и strict ordinary opens. Legacy не возобновляется после drop strict owner.
Strict acquisition однократно проверяет persisted immutable policy; connections
используют proof/lease владельца без SQL control lookup на каждом vector access.
Legacy check bounded и выполняется один раз на приобретённый shared lease.

Успешный apply меняет только sources и store ownership. Immediate eviction, удаление
vectors, schema rewrite, re-embedding, ANN/index rebuild не подразумеваются.
Budget shrink описывается отдельно и не превращается в скрытый eager eviction.

## Durable boundaries для уточнения

1. До consent, при fail/missing grants — никаких новых файлов или изменений state.
   Stale review, обнаруженный после admission, не меняет sources/control state;
   нынешние helpers могут создать только coordination directories/lockfiles.
   CF5b1 точно перечисляет эти записи и проверяет отсутствие прочих изменений.
   No-write plan никогда не получает создающие locks.
2. Получить native/SQL/config admission; повторно обследовать registry/source bytes;
   сравнить полный consent/observation context, включая Missing, root и cap.
3. До staging проверить полные source/journal/metadata/disk peaks; частичный survey
   не принимается.
4. Сохранить полный journal и pending mode; завершить короткую SQL transaction.
5. Создать уникальный private stage рядом с source; полностью записать и flush.
6. Под guards проверить, что target всё ещё observed old либо already-new;
   выполнить native replacement и обязательную durability operation.
7. Прочитать фактический результат и записать per-source progress. Ошибка rename
   может изменить namespace, особенно ReplaceFileW: проверять пути, не return code.
8. Проверить все sources, registry/context и полную policy; затем commit strict
   active mode. Его публикация отделена от consumer startup.
9. Освободить guards. Explicit launch получает strict owner и готовит один runtime
   snapshot.
10. Удалять только допустимые private stage/backup данные с явным bounded retention.

Recovery классифицирует каждый source как Old/New/Neither/Missing по реальным
байтам независимо от recorded progress. Resume/restore явные, под тем же admission.
Neither/unknown не перезаписывается. Committed strict mode не допускает legacy
fallback при missing/corrupt state. Изменённые после commit settings проходят обычные
managed plan rules, а не объявляются повреждённым source. Adoption history и текущие
editable configuration revisions различаются.

## Namespace и ограничения durability

Native locks координируют current callers, но не произвольные editors/raw SQL/старые
бинарники. Path hash не является directory capability; nofollow final file не
предотвращает замену parent namespace. Поддерживаемый maintenance scope и причины
отказа явные. Filesystem compare+rename нельзя объявлять CAS. Backups сохраняют
только captured bytes, а не незамеченную конкурентную правку.

Process-kill tests доказывают process recovery, но не power-failure durability.
Strict commit нельзя объявлять power-safe до доказательства source replacement
contract на каждой платформе. Failed/unsupported flush блокирует commit.
Cross-volume copy fallback, implicit escalation и global volume flush исключены.
Normal v2 startup/admission отказывает при incomplete sources, если storage не
может доказать coherent committed adoption.

## Проверенные первичные источники, 2026-11-13

- https://www.man7.org/linux/man-pages/man2/fsync.2.html — file fsync не обязательно
  сохраняет directory entry; нужен отдельный directory fsync.
- https://sqlite.org/atomiccommit.html — SQLite atomic commit касается собственной
  DB/journal, но не сторонних TOML. SQL transaction не делает их запись общей atomic.
- https://sqlite.org/pragma.html#pragma_synchronous — EXTRA/DELETE directory sync;
  проверять реальные PRAGMA values: неизвестные pragmas могут молча игнорироваться.
- https://learn.microsoft.com/en-us/windows/win32/api/winbase/nf-winbase-movefileexw —
  WRITE_THROUGH документирован; copy/delete может сохранить source при success.
  Stages остаются на том же volume, COPY_ALLOWED fallback запрещён.
- https://learn.microsoft.com/en-us/windows/win32/api/winbase/nf-winbase-replacefilew —
  REPLACEFILE_WRITE_THROUGH не поддерживается. Некоторые ошибки перемещают файлы
  под другими именами: проверять все пути. Attribute/DACL preservation отличается
  от MoveFileExW и не может молча отбрасываться.
- https://learn.microsoft.com/en-us/windows/win32/api/fileapi/nf-fileapi-flushfilebuffers —
  flush касается указанного writable handle; whole-volume flush требует admin
  privileges и не является допустимым fallback для обычного пользователя.

## Приёмочные сценарии до activation

- Live daemon/direct reader/writer/backfill lease/vector handle: admission refusal;
  configs/vector bytes прежние, старый runtime пригоден.
- Config writer и maintenance получают locks в обоих порядках, включая root aliases.
- Registry addition/root move между planning/admission и во время apply.
- Missing source отличается от empty/newly created после review.
- Unknown schema, hot journal, missing sidecar, corrupt policy/plan identity.
- Process death до/после каждого durable boundary, включая namespace success до
  progress row; per-source resume/restore на всех трёх ОС.
- Disk-full/permissions/flush/rename errors, Windows open handles/alternate names,
  parent symlink replacement и unexpected external bytes.
- Decline/no-TTY/missing grant/stale identity не меняют sources/control state;
  допускаются только явно перечисленные coordination writes.
- Default/explicit cap, прежние vector content/model hashes и leases; ноль
  provider/download calls и eager eviction при adoption.
- После release последнего strict handle legacy startup всё ещё отказывает;
  owner settings mismatch не меняет cap молча.
- Low-risk v2 edit после завершённого adoption отличим от interrupted migration;
  artifact-affecting edits блокируются до CF11.
- Source count/bytes и peak disk/RAM проверяются до необратимых шагов. Planning
  limit в 256 registered projects не является лимитом в 20k документов.

## Следующие обязательные задачи

CF5b1: точные SQL schema/application/user version, state transition table,
source identity/namespace и полный lease order по всем daemon/direct callers.
Bounded prototype — только isolated scratch; проверить PRAGMA и native locks.
Определить no-write inspection/recovery hot/corrupt/partial state и все callers,
которые проверяют maintenance до construction. По этому исследованию activation нет.

CF5b2: platform staging/replacement/durability seam с сохранением attributes;
causal subprocess/injected-failure evidence до использования в activation.

CF5b3: stopped local migration/recovery executor и observed-plan adapter;
настоящие per-boundary failure tests, без providers/index generation changes.

CF5c: controlling CLI policy/consent и согласованный strict startup; реальные
subprocess no-TTY/prompt/apply/exit tests и native recovery acceptance.

CF11: отдельные data-generation/rebuild/re-embedding/online publication guarantees;
stopped source/ownership adoption их не предоставляет.

## Карта исходников и приёмка этапа

- [Prepared vector ownership](../../../src/vector_cache/owner.rs),
  [legacy/owned constructors](../../../src/vector_cache.rs).
- [Actual observation](../../../src/config/observe.rs),
  [bounded nofollow capture](../../../src/config/observe/source.rs),
  [safe output](../../../src/config/observe/view.rs).
- [Source proposals](../../../src/config/migrate.rs),
  [consent boundary, reviewed PR149](https://github.com/punkhomov/docsbase-memory-mcp/blob/6155a87/src/config/policy.rs).
- [Daemon admission](../../../src/daemon/admission.rs),
  [project leases](../../../src/daemon/lifecycle.rs),
  [config writer guards](../../../src/config/paths.rs).
- [Provider writer](../../../src/cli/connect.rs), [model writer](../../../src/cli/models.rs).

Приёмка CF5b0 — documentation/source/reference audit и fresh review. Runtime tests
для prose не добавляются. Документированное поведение API не является native
experimental evidence. References и process-kill tests не доказывают power-failure
guarantees; это gates следующих задач. CF5/CF7/FR-13/14/15/16 и #78 не завершены.
CF10/CF11, #81 и работа другого агента по #79 остаются самостоятельными.
