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
