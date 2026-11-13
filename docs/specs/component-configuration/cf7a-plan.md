# CF7a: владелец общей базы векторов

Исполнение inline по executing-plans/TDD, без делегирования реализации.
База: origin/main 4c9ff9f (PR139). Rust1.100.0 stable, upstream Cargo.lock,
cli-full и lean offline. Изолированный worktree, ветка feat/vector-store-owner.
Новых crates, provider calls/downloads, baseline/threshold changes нет.

## Граница задачи

Первый этап CF7 даёт явный store owner и проверяет его настоящие соединения.
Обычные v1 callers остаются legacy: их project budgets и zero-cap semantics
сохраняются. File-v2 activation, consent/apply, persisted mode migration и
передача owner во все daemon/jobs consumers — следующие этапы CF5/CF7.
Одного этого PR недостаточно для включения strict-v2 или завершения CF7/#78.

VectorStoreSpec владеет max_entries=1000000, max_bytes=1GiB и busy_timeout=5s.
Typed patch daemon-scoped; strict capacities положительны, timeout допускает
zero и ограничен signed SQLite milliseconds. Validation предшествует I/O;
невалидный patch не меняет settings/origins. Старый max_bytes descriptor
source-preview ссылается на того же owner. Новых TOML keys нет.

VectorStore::acquire(cache_root, &VectorStoreSettings) создаёт один
immutable owner. Его connect() создаёт соединения с общей policy и LRU clock.
Клонирование owner/живые соединения продлевают владение. Другой owner, даже с
той же policy, отклоняется. Разные roots независимы. Root закрепляется через
canonical path; aliased paths не обходят координацию.

## Режимы и lifetime

Постоянный coordination file vectors.lock — идентификатор локального протокола,
не runtime limit. Native nonblocking shared lock на каждом legacy handle;
exclusive lock на strict owner. Используются stable std File locks, без unsafe
или новой зависимости. Конфликт обнаруживается до открытия SQLite. Lease живёт
до закрытия connection, в том числе после drop исходного owner. Failed opens
освобождают lease. File не удаляется/не заменяется при release; process exit
освобождает OS lock, его наличие не означает наличие owner. Успешный guard
явно unlock-ит lock при Drop: временный fork-inherited descriptor иначе способен
задержать release до exec; failed try-lock не создаёт guard и не делает unlock.

Это advisory-протокол для cooperating current callers. Raw SQL, старые binaries
без протокола и внешняя замена cache/lock files им не ограничиваются. Это не
механизм авторизации. Запрет legacy/strict одновременности действует во время
lease, не сохраняет strict mode после последнего release. Persisted activation,
admission/quiescence для migration и stopped-cache maintenance принадлежат CF5/CF7.

Owner не держит SQLite transaction на lifetime: отдельные connections и короткие
SQL-фазы остаются конкурентными. Shared atomic LRU clock инициализируется SQL MAX
под exclusive mode lease. Strict get/touch, put+evict и unpin+evict используют короткую immediate
reservation до выдачи recency; мутации и eviction атомарны; при SQL failure
предыдущее состояние сохраняется. Provider work не входит в SQL transaction.
Leased rows сохраняют прежнюю возможность временно превысить limits: это единая
eviction policy, не обещание hard disk/RSS cap. Vector/model/hash identities и
семантика leases не меняются.

Отклонены: process-global owner registry, mutex одной SQLite connection, запись
per-project cap в strict handles, автоматическое adoption существующей config,
persisted strict marker без recovery/consent, удаление lockfile при release.

## Файлы и интерфейсы

- src/vector_cache/settings.rs: VectorStoreSettings, VectorStorePatch,
  VectorStoreSpec, standalone(); pure owner и max_bytes descriptor.
- src/vector_cache/owner.rs: VectorStore::acquire/root/settings/connect,
  native lifetime lease, shared clock и private connection access tokens.
- src/vector_cache.rs: owner-backed legacy defaults, guarded compatibility
  constructors, connection helper, strict transactional writes/eviction.
- src/platform/{mod,unix,windows}.rs: private nofollow/nonblocking lockfile open
  через существующий ADR-9 seam, без transport/provider изменений.
- tests/vector_store_owner.rs: реальные SQLite/OS/process regression cases.
- component-configuration specs/tasks/progress и docs/CONFIG: dated deltas.

src/daemon, src/ipc, src/cli, src/watch и provider orchestration не меняются.
Второй агент может реализовывать #79 на прежних explicit constructor signatures.

## Task CF7a

- [x] Baseline: реальные vector/cache/lifetime suites на точной базе.
- [x] RED API и domain/scope/rollback/no-I/O checks.
- [x] Native mode exclusion: оба направления, multiple legacy handles,
  owner clones/connection retention, failed constructor cleanup, roots/aliases
  и subprocess probe без retries/sleeps.
- [x] Реальные connections: entry/byte eviction, shared LRU, busy_timeout=0
  под SQLite writer lock, same-model/text reuse и независимые model/lease keys.
- [x] Causal RED/GREEN SQL failure: put+evict rollback и unpin+evict rollback.
- [x] GREEN focused suites, full cli-full, lean focused, Clippy/fmt/diff.
- [x] Dated deltas/ledger, fresh task review и fix rounds.
- [ ] Fresh final review, required native CI с inventory audit и PR delivery.
