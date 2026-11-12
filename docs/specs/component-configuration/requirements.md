# Компонентная конфигурация и инвалидация состояния

Дата: 2026-11-09. Issue [#78](https://github.com/punkhomov/docsbase-memory-mcp/issues/78).
Статус: архитектурный контракт для поэтапного внедрения. Текущая поставка CF0
содержит исследование и план; runtime-требования ниже ещё не реализованы.

## Проблема и границы

`Config` описывает лишь часть фактического поведения: DenseCache, query cache,
ANN, SQLite, watcher, transport и lifecycle добавляют собственные defaults.
Пользователь не может ни воспроизвести effective configuration, ни заранее
понять последствия её изменения. Центральный TOML parser не должен владеть
правилами всех этих компонентов.

Цель — полностью разрешённые typed settings, принадлежащие компонентам,
и согласованный план изменения их runtime и производных данных. Источники:
пользовательский конфиг, legacy CLI/env overrides, явный bootstrap-контекст.
Новые implicit network requests и автоматическое согласие на расходы запрещены.

Акторы: оператор CLI/MCP, управляющий pipeline, daemon и разработчик компонента.
Публичная Rust-библиотека (#80), HTTP/Node transport (#79), async remote (#81),
исправление DB contention (#66) и изменение ранжирования имеют отдельную приёмку.

## Приоритетные сценарии

| Приоритет | Потребность | Независимая приёмка Given / When / Then |
|---|---|---|
| P1 | Оператор видит фактические настройки | Поле отсутствует / effective config разрешён / показан default и его источник, consumer получает то же значение |
| P1 | Разработчик сохраняет владение компонентом | Добавлена настройка DenseCache / подключена секция / loader не содержит ANN-specific validation/change rules |
| P1 | Pipeline избегает неожиданной инвалидации | Требуется rebuild / выбрана fail-policy / отдельный exit code, сохранённое состояние не изменено |
| P2 | Оператор меняет только необходимые данные | Изменён timeout / построен plan / нет vector fetch, reindex или ANN rebuild |
| P2 | Reload безопасен при активной работе | Новое поколение не готово или устарело / apply прерван / пригодное старое состояние остаётся, stale worker не публикуется |

P1 сначала делает поведение наблюдаемым и даёт основу #66. P2 требует provenance
и опубликованных поколений, поэтому не сводится к одному callback `on_change`.

## Функциональные требования

- **FR-1 (must).** Инвентарь включает эксплуатационные/алгоритмические `const`,
  литералы и library defaults: значение, источник, owner, scope, validation,
  затрагиваемое состояние и правило изменения. Runtime-параметры настраиваемы
  по умолчанию. Исключения и вычисляемые значения имеют проверяемое обоснование.
- **FR-2 (must).** Компонент владеет typed settings, defaults, validation,
  relevant fingerprint и локальным анализом воздействия. Composition root
  знает перечень компонентов, но не их внутренние правила.
- **FR-3 (must).** Defaults вызываются только resolver-ом до создания runtime.
  Источники складываются с precedence CLI > project > global > defaults;
  отсутствие, явное значение и отключение различимы. Обязательные resolved
  поля не являются `Option`; constructor не читает TOML/env и не выбирает fallback.
- **FR-4 (must).** Существующая v1 whole-value замена `hybrid` сохраняется.
  Field-wise merge доступен только через явный новый schema version; mixed
  sources имеют определённую семантику. Bool true сбрасывает hybrid к обеим
  default stages, bool false — к disabled без stages; v1 table replacement
  начинается с enabled table-base без отсутствующих stage sections. Legacy migration
  не меняет настройки или сохранённые данные без reviewable плана.
- **FR-5 (must).** Daemon/machine, provider/model и project scopes различаются.
  Новый project override не перенастраивает общий semaphore/pool/cache.
  Неоднозначный legacy vector budget диагностируется при миграции, без
  скрытого превращения в global cap или per-project quota.
- **FR-6 (must).** Bootstrap путей отделён от обычной конфигурации. Источники
  env overrides имеют provenance; credentials передаются отдельной зависимостью,
  их значения отсутствуют в effective output, fingerprints, plans и errors.
- **FR-7 (must).** Неизвестные ключи/неверные значения отклоняются до применения.
  Feature-disabled settings разбираются без загрузки тяжёлых crates; активация
  недоступной возможности даёт явную admission error. Неподдерживаемый timeout
  не может отображаться как фактически действующий.
- **FR-8 (must).** Effective view показывает значение, единицу, scope, source
  и capability; вычисленные значения отмечены формулой. Старый `docsbase config`
  сохраняет существующий v1 формат; новый view/JSON-контракт версионируется отдельно.
- **FR-9 (must).** Карта различает artifact dependencies и request dependencies.
  Транзитивное влияние вычисляется по relevant identities, с дедупликацией
  действий и проверкой циклов/конфликтов до apply.
- **FR-10 (must).** Plan сравнивает effective settings с artifact provenance
  и активным snapshot. Отсутствие/unknown provenance/частичная готовность,
  algorithm/format version и отсутствие старого TOML учитываются явно.
- **FR-11 (must).** Fingerprints versioned, канонические и scoped: параметры
  семантики плюс upstream identities. Timeout/budget не входят в vector identity;
  model/prefix/normalization/truncation входят. Whole-config hash недостаточен.
- **FR-12 (must).** Dry-run plan не создаёт/мигрирует БД, не пишет markers,
  conflict logs или SQLite sidecars, в том числе при missing/schema mismatch.
  Использует отдельный side-effect-free inspector, не operational read-only helper.
  Не открывает runtime/model и не вызывает providers/downloads. Показывает
  причины, повторное использование, проекты/артефакты, риски и ожидаемые расходы
  без обещания точной стоимости.
- **FR-13 (must).** Policies `prompt/apply/fail` принадлежат управляющему CLI.
  Prompt без TTY завершается до apply; service loop/MCP не задают вопросов.
  Apply разрешает только явно одобренные классы действий; расходы и необратимая
  миграция требуют отдельных grants. Fail имеет отдельный exit code. V1 startup
  без новых flags сохраняет legacy behavior; новый v2/policy path по умолчанию
  разрешает Auto в prompt только для direct CLI с TTY, иначе в fail. Service/MCP
  loop не спрашивает даже при унаследованном TTY; explicit override обрабатывает CLI.
- **FR-14 (must).** Перезапуск runtime, первый запуск и отсутствие disposable
  cache не приравниваются к потере сохранённых данных. На новом v2/policy path
  autostart/MCP имеют явную безопасную policy; не запускают rebuild/re-embed/download неявно.
- **FR-15 (must).** Apply проверяет актуальность плана, готовит замену отдельно,
  публикует согласованное поколение, поддерживает interruption/recovery и
  reclaim после readers/leases. SQL и Tantivy не считаются общей транзакцией.
- **FR-16 (must).** Один запрос захватывает immutable config/stages и совместимые
  data identities. Устаревший worker не публикуется поверх нового поколения.
  Все живые builds, старые snapshots и модели учитываются до освобождения;
  уменьшение budget не объявляет их уже остановленными/вытесненными.
- **FR-17 (must).** Новые настройки #66/#79/#81 имеют одного владельца и resolved
  defaults до появления consumer. Полная миграция ANN/index не блокирует
  небольшой bootstrap/store foundation для #66.

## Нефункциональные требования

- **NFR-1:** Rust 1.88, Linux/macOS/Windows, cli-full; lean default offline.
  Новых crates для базового контракта нет. `unwrap/expect/panic!` в production запрещены.
- **NFR-2:** Default settings сохраняют прежние результаты, бюджеты и format IDs.
  Изменение качества требует отдельного замера/golden review, не baseline regeneration.
- **NFR-3:** Планирование не расходует provider budget и не зависит от размеров
  полного vector payload. Читаются bounded metadata; canonical/hash операции
  ограничены уже разрешёнными input limits. Ошибки не уничтожают старые данные.
- **NFR-4:** Реализация вводится проверяемыми вертикальными slices. Каждый FR
  получает automated regression в owning task; tests управляют событиями/временем,
  watchdog не является требованием быстродействия CI.

## Модель данных и интерфейсы

`SourceLayer` → component `Patch` → `Resolved{Settings, Provenance}` → runtime.
`ArtifactFacts` + `Resolved` → immutable `ChangePlan` → `PreparedChange` →
`PublishedGeneration` → deferred reclaim. Credentials не входят в эти значения.
Подробные типы и CLI/exit codes: [design.md](design.md).

## Критерии и граница поставки

- **SC-1 (CF0):** Все обнаруженные семейства runtime-настроек имеют строки
  инвентаря, fixed exceptions обоснованы; источники проверены на закреплённом commit.
- **SC-2 (CF0):** Есть карта зависимостей, матрица инвалидации, выбранный контракт,
  bootstrap/merge/migration/policy/failure decisions, задачи и FR coverage.
- **SC-3 (runtime):** Override/default доходит без подмены до настоящего consumer;
  malformed config не меняет состояние. Проверены lean/individual/cli-full features.
- **SC-4 (runtime):** Plan/apply/fail/prompt, no-TTY, interruption и concurrent
  reload/rebuild покрыты; timeout не вызывает rebuild, chunk/model/graph settings
  вызывают только доказанные downstream действия с переиспользованием vectors.
- **SC-5 (runtime):** Все runtime-потребители мигрированы; оставшиеся fixed значения
  соответствуют правилам, source inventory обновлён, документация описывает реальность.

CF0 закрывает только SC-1/SC-2. #78 остаётся открытым до SC-3–SC-5;
текущие команды/ключи/поведение этим PR не меняются. [tasks.md](tasks.md)
задаёт implementation gates вместо обещания закончить всё одним refactor.

## Проверка предположений и открытые вопросы

| Challenge | Решение |
|---|---|
| Нужна ли настройка для каждого `4`? | Нет: f32 width/packing — invariant, `fetch*4` — quality policy |
| Любой вызов компонента означает rebuild? | Нет: runtime и artifact edges разделены |
| Полная merge-замена — случайный баг? | Это v1 контракт, меняется только явной v2 migration |
| Можно ли удалить всё при первом provenance? | Нет: Unknown означает plan/adoption proof, не разрешение delete |
| Настройка global cache из project безопасна? | Нет: legacy compatibility и v2 ownership различаются |
| Смена конфигурации делает старый request неправильным? | Завершение старого согласованного snapshot допустимо; смешение поколений — нет |
| Новый trait решит concurrency? | Нет: storage publication и #66 имеют отдельную реализацию/приёмку |

Блокирующих продуктовых вопросов для CF0 нет. Нагрузочные бюджеты сайта, внешние
Rust bindings и изменения supported provider registry определяются их owning
issues до появления соответствующих настроек; CF0 не назначает им выдуманные SLA.

## Delta 2026-11-10 — CF2 store consumption boundary

CF2 реализует runtime-потребление typed `Resolved<DbSettings>`: переданные override
и defaults доходят до реальных writer/read connections без повторного выбора
значений внутри Db. Управляющая CLI/daemon boundary разрешает policy один раз и
передаёт её дочерним jobs/watchers/ANN workers. Старые публичные методы остаются
compatibility adapters; production использует только explicit-settings путь.
Это частичная SC-3 для store, не завершение SC-3/SC-5 всего #78.

В CF2 не активируются TOML `store`/`read_pool`: v1 startup и формат `config`
сохраняются, production boundary пока получает owner defaults из resolver.
Typed callers могут передавать overrides уже в CF2. File activation входит в
CF3/CF5 с определёнными schema/startup/error semantics; pool consumer — #66.
Неработающие public keys и per-consumer fallback не добавляются.

Pure inspector сообщает **identity схемы заголовка**, а не готовый SQL reader или
snapshot документов. `Ready` означает подтверждённую identity этого наблюдаемого
артефакта; проверка таблиц/rows/query consistency принадлежит operational readers.
Inspector читает ограниченный SQLite header без Connection/immutable/VFS tricks,
повторно проверяет header/file metadata и отсутствие WAL/SHM/journal. Sidecars,
меняющееся/нечитаемое/невалидное состояние дают Unknown/NeedsRecovery без пригодной
identity. Missing допускается только без main DB и recovery sidecars. Наблюдение
не заменяет повторную проверку inputs перед apply (CF11).

## Delta 2026-11-11 — CF3: preview версий и проект миграции

- **CF3-1:** ограниченные caller-supplied sources имеют независимые версии v1/v2.
  V1 сохраняет замену hybrid целиком; v2 наследует отсутствующие вложенные поля.
  Bool reset использует те же owner templates; stage false отключает, stage table
  включает/обновляет. Неверные version/key/scope отклоняются до побочных эффектов.
  Provenance различает унаследованные, явно переданные и сброшенные поля.
- **CF3-2:** v2 допускает только global vector_cache.max_bytes; project vector_cache
  и v2 hybrid.cache запрещены. Прежний пример ADR-CFG-2 с project-v2 cache override
  заменяется примером reranker.window. Preview global2/project1 явно сообщает
  конфликты scope для legacy project cache.
- **CF3-3:** pure MigrationDraft получает список зарегистрированных проектов и
  loaded/observed-missing sources; необследованные проекты блокируют полный draft.
  Legacy budgets показаны отдельно; maximum — предложение, не согласие.
  Выбор global budget явный. Файлы/state/providers не затрагиваются; отказ от
  миграции сохраняет рабочее v1 поведение.
- **CF3-4:** source edits содержат исходную identity/hash и before/after text;
  предупреждают о canonical formatting, потере комментариев и materialized defaults.
  Proposed sources разрешаются повторно; non-resource effective settings должны
  совпадать. Различия budgets явные. Сохраняются v2 inheritance и bool resets;
  отсутствующие project files не создаются.
- **CF3-5:** preview/draft явно сообщает о незавершённом runtime adoption.
  Обычные loaders не активируют v2 до consent/strict shared-store ownership
  CF5/CF7. Новые CLI commands, public no-op settings, provider construction и
  implicit migration запрещены. Проверки: Rust1.88/default/cli-full, native CI,
  fresh task/final review.


CF3 budget survey clarification (2026-11-11): при global-v1 учитываются effective
legacy budgets всех обследованных проектов. При global-v2 уже выбранный global
cap действует для отсутствующих/v2/reset источников; explicit legacy project
cache override остаётся отдельным конфликтующим наблюдением. Неиспользуемый
legacy default не увеличивает suggested maximum. Ошибки preview сохраняют
категорию слоя (base/hybrid/effective), но не включают исходные значения; обычные
v1 runtime diagnostics не меняются.

## Delta 2026-11-12 — CF4a: effective source view

- **CF4a-1:** новый `config effective --format=json` имеет view_version1 и явно
  показывает source_resolution basis, без утверждения об observed live runtime.
  Bare `config` сохраняет прежнюю форму. Поля имеют typed value, unit, scope,
  реальный SourceId и feature capability; unknown origins не угадываются.
- **CF4a-2:** значения формирует owning component, а не центральная копия правил.
  Checked composition отклоняет missing/duplicate/undeclared поля. Provider
  capabilities и safe record projection принадлежат существующему registry;
  credentials/raw arbitrary provider params не сериализуются. Build availability
  не равна provisioning readiness; status/probe/build методов не вызывают.
- **CF4a-3:** новый CLI path не создаёт state/cache/data, не открывает БД/IPC,
  не вызывает providers и не активирует v2 runtime. Ошибки не раскрывают исходный
  TOML/credential values. Это scoped FR-6/8/12, не завершение всего CF4/#78.

## Delta 2026-11-12 — CF4b bounded pure state plans

- **CF4b-1:** `config::change::compile` принимает borrowed owner assessments и
  explicit `PlanLimits`; выдаёт `ChangePlan` v1 с canonical identity, observations,
  topological actions, required grants и blockers. Duplicate actions объединяют
  reasons/dependencies; conflicts/missing inputs/cycles отклоняются. Input
  permutations не меняют результат. Evict/DeleteStore/Rebuild различаются.
- **CF4b-2:** реальные SearchSpec/DbSpec оценивают свои validated policy значения;
  timeout влияет только на runtime policy, не на vectors/ANN/index. Fingerprints
  имеют разные versioned domains, длительности не теряют sub-ms точность.
- **CF4b-3:** metadata schema owner использует no-write inspector: Missing,
  Unknown, NeedsRecovery и распознанный schema header различаются. Future/unknown
  identity не переиспользуется. Ни header, ни unchanged observation не доказывают
  row/index/query readiness. Отсутствующий cache не создаётся; mismatch/WAL и
  recovery evidence не меняют DB, sidecars, conflict logs или directories.
- **CF4b-4:** CF3 MigrationDraft становится state assessment: incomplete survey
  не означает no-op; RewriteSource зависит от AdoptSharedStore. Source revisions
  относятся к supplied text, не observed file existence; strict-store adoption
  и source observation остаются явными blockers. Plan не содержит source text.
- **CF4b-5:** raw action/edge counts проверяются до построения graph; streaming
  JSON admission ограничивает bytes входа и результата независимо. Лимиты задаёт
  caller без default. Это bounds metadata, не RSS. Каждый SourceEdit повторно
  проверяется по существующему MAX_STATE_BYTES перед hashing. Plan ID не consent;
  все actions отмечены ExecutorUnavailable до появления реального apply.

CF4b — pure Rust API, не новый CLI. Не закрывает CF4/SC-4: registry observation,
активный daemon snapshot, CLI migration, оценки места/стоимости, полная карта
artifact fingerprints и исполнение остаются последующими owning slices.

## Delta 2026-11-12 — CF4c1: quiescent registry observation

- **CF4c1-1:** полный persisted registry наблюдается bounded API, включая
  неиндексированные и отсутствующие на диске roots. Missing, Ready(empty) и
  Unavailable различаются. Ready относится только к registry, не query readiness.
- **CF4c1-2:** observer не пишет DB/sidecars/directories/conflict log и не вызывает
  recovery/providers/daemon. Native SQLite connection владеет nonblocking exclusive
  lease и immutable read-only pager на одном descriptor; sidecars и изменившийся
  namespace запрещают Ready. Active/recovery DB остаётся Unavailable.
- **CF4c1-3:** caller явно задаёт count, retained metadata bytes, individual SQLite
  value cap и soft page-cache KiB. Нет partial Ready или скрытого fallback. Эти
  limits не являются жёстким RSS cap внутренних SQLite allocations.
- **CF4c1-4:** revision v1 включает exact cache context, schema и ordered id/root;
  future apply обязан повторно наблюдать state. CLI/source observation/apply ещё
  не поставляются. FR-10/12 закрываются только в этой границе.
- **CF4c1-5:** существующие same-process SQLite locks сохраняются. Raw open/close
  DB descriptor запрещён: POSIX close снимает locks процесса. Поэтому CF4c1 не
  вызывает прежний header-only inspector; preflight использует только metadata,
  а schema читается после lease. Это заменяет initial-plan header-first proposal.

CF4c1 review correction (2026-11-12): native `unix` допускается только с concrete
POSIX method table; automatic no-lock/dotfile/AFP/NFS alternatives отклоняются
до xLock. Symlink cache ancestors допустимы с directory resolution/recheck,
сам DB entry остаётся NOFOLLOW; alias retarget запрещает Ready. Oversized persisted
root проверяется отдельным value cap, который допускает schema и обычный root.

CF4c1 final-review delta (2026-11-12): Windows фактический borrowed native HANDLE
должен соответствовать canonical-directory/DB filename до SQL и Ready; metadata
preflight/NOFOLLOW flags одни недостаточны для reparse races. Gated native test
подменяет DB symlink после preflight и проверяет target recovery preservation.
«Nonblocking lease» означает отсутствие application retry/busy-timeout loop:
платформенные SQLite3.53.2 Windows pending-byte/AV retries остаются явно описанным
исключением; hard wall-clock deadline и нулевая задержка не обещаются.

## Delta 2026-11-12 — I133: schema inspection сохраняет locks

- `Db::inspect_readonly` не открывает/закрывает raw DB descriptors вне SQLite
  bookkeeping: существующие locks reader/registry observer сохраняются, внешний
  writer получает BUSY до их освобождения.
- Schema header остаётся fixed100-byte observation без SQL/schema loading,
  recovery/migration/sidecar/log writes. Native exclusive lease и directory
  resolution/recheck общие с CF4c1. Busy/unsupported stores дают Unknown.
- Это заменяет прежнюю формулировку CF2 «без Connection»: private SQLite handle
  разрешён ради lock correctness, SQL и query-readiness claims по-прежнему нет.
  Missing/orphaned recovery и версионирование identity остаются прежними.
