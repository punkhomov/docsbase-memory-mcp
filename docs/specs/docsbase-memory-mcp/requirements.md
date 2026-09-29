# Feature Specification: docsbase-memory-mcp

**Status:** Draft (rev 2)
**Дата:** 2026-09-28
**Вход:** бриф из Discovery (см. также `docs/rust-docs-rag-research.md`)
**Constitution:** `docs/specs/constitution.md`

## 1. Problem

Документация проекта и вендоров лежит разрозненно: агент тратит контекст на grep/read,
не находит нужный раздел, галлюцинирует API. Существующие решения — Python/Node,
тяжёлые (Docker/Postgres), либо заточены под web-crawl. Нужна быстрая локальная память
по документации, разделяемая между coding-агентами, работающая офлайн.

## 2. Goals / Non-goals

**Goals**
- G1: постоянный **per-project** индекс `.md` с явной регистрацией проекта и
  авто-обновлением.
- G2: доступ через MCP, read-only, локально (stdio frontend + daemon).
- G3: лексический поиск с надёжным exact-identifier попаданием.
- G4: один daemon на пользователя, шарится всеми агентами.
- G5: офлайн после установки; ноль сетевых вызовов и внешних API.

**Non-goals (v1)**
- PDF/DOCX/HTML/веб-краул.
- Векторный/семантический поиск (фаза 2).
- **Shared-слой кросс-проектных доков — отложен.** Сейчас вендорские доки покрывает
  Context7, а общие конвенции правильнее держать в переиспользуемом скилле. Вернуться
  после фазы 2.
- Запись через MCP (файлы — источник истины).
- Мультипользовательность, auth, сетевой доступ.
- macOS/Windows (фаза 3).
- Индексация кода (для этого есть отдельный codebase-memory-mcp).

## 3. Users / Actors

- **Разработчик** — использует Codex/OpenCode с MCP, держит проекты и `.md` доки.
- **Coding-агент** — MCP-клиент; подключается через stdio frontend.
- **Daemon** — per-account процесс: индексация, watcher, sync jobs, чтение.
- **Файловая система** — источник истины; watcher следит за `.md`.
- **CLI-пользователь** — `docsbase` без агента (одноразовые команды).

## 4. Prioritized user stories

### P1 — Явная индексация проекта и поиск по нему (v1)
*As a developer, I want to register and index a project explicitly, then have the agent
search its docs, so answers come from specs instead of guesses.*

**Why this priority:** ядро продукта; без этого нет ничего.
**Independent test:** чистый проект → `index_project` → `search_docs` возвращает
релевантный чанк с citation; до `index_project` поиск по проекту не находит ничего.

**Acceptance scenarios**
1. **Given** проект не в реестре, **When** агент подключается, **Then** `status` /
   `list_projects` показывает «не индексирован»; молчаливой индексации не происходит.
2. **Given** проект не в реестре, **When** вызывается `index_project`, **Then** проект
   зарегистрирован, индекс создан, `search_docs` находит чанк с `path`, `heading_path`,
   `lines`.
3. **Given** `auto_index = true` в конфиге, **When** первое подключение проекта, **Then**
   индекс создаётся автоматически (opt-in, как `auto_index` в CBM).
4. **Given** запрос `assessment_plan_id`, **When** поиск выполняется, **Then** документ с
   этим идентификатором попадает в top-3 (точное совпадение токена).

### P2 — Один daemon на все агенты (v1)
*As a developer, I want one daemon shared by all agents, so the index is consistent and RAM
is not multiplied.*

**Why this priority:** заявленная модель использования; влияет на архитектуру.
**Independent test:** два агента стартуют → один PID daemon; закрытие первого не гасит
демон; закрытие последнего — гасит.

**Acceptance scenarios**
1. **Given** ни одного daemon, **When** стартует первая сессия, **Then** daemon поднят,
   записан pid/socket; `status` видит одну сессию.
2. **Given** две сессии, **When** первая закрывается, **Then** daemon жив и вторая
   продолжает работать.
3. **Given** последняя сессия закрыта, **When** истекает grace-период, **Then** daemon
   завершается, индекс на диске сохранён.

### P3 — Свежесть: правка видна агенту (v1)
*As a developer, I want edits picked up within seconds, so the agent sees the doc I just
wrote.*

**Why this priority:** файлы — источник истины; иначе агент читает устаревшее.
**Independent test:** изменить `.md` → в течение 2 с новый текст находится, старый чанк
удалён.

**Acceptance scenarios**
1. **Given** проиндексированный файл, **When** он изменён на диске, **Then** в течение 2 с
   поиск возвращает новую версию.
2. **Given** файл удалён, **When** watcher обработал удаление, **Then** его чанки больше не
   находятся, `list_docs` его не показывает.

### P4 — Hybrid retrieval (фаза 2, за флагом)
*As a developer, I want semantic search when lexical misses paraphrases.*

**Independent test:** с включённым флагом semantic-запрос без общих слов с текстом находит
нужный чанк; с выключенным — не находит.
**Acceptance scenarios**
1. **Given** флаг `hybrid = false`, **When** выполняется любой запрос, **Then** поведение
   идентично v1.
2. **Given** флаг `hybrid = true`, **When** semantic-запрос без общих слов с текстом,
   **Then** нужный чанк найден за счёт vector + RRF, латентность в рамках NFR-1.

### P5 — Auto-setup конфигов агентов (фаза 3)
*As a developer, I want `docsbase install` to wire MCP into Codex/OpenCode automatically.*

**Independent test:** на чистой машине `install` добавляет корректные MCP-записи; повторный
запуск идемпотентен.
**Acceptance scenarios**
1. **Given** чистая машина, **When** `docsbase install`, **Then** MCP-записи Codex/OpenCode
   созданы и валидны.
2. **Given** конфиг с пользовательскими записями, **When** install/uninstall, **Then**
   чужие записи не изменены и не удалены.

### P6 — Shared-слой (отложено, вернуться после фазы 2)
*As a developer, I want cross-project docs available everywhere, so they are not duplicated.*

**Independent test:** shared root → поиск из двух проектов находит документ в обоих.
**Acceptance scenarios**
1. **Given** настроенный shared root, **When** поиск из проекта A, **Then** результат
   помечен как shared.
2. **Given** тот же shared root, **When** поиск из проекта B, **Then** контент доступен без
   переиндексации под B.

## 5. Functional requirements

### A. Жизненный цикл и daemon

- **FR-1** (must) `docsbase install` / `uninstall` регистрируют и удаляют owned-артефакты
  (бинарь, конфиги, сокет-путь); uninstall не трогает чужие файлы, а перед удалением
  индексов показывает список и удаляет только после подтверждения.
- **FR-2** (must) Первая daemon-backed сессия поднимает daemon; каждая сессия регистрирует
  свою работу; последняя гасит daemon по истечении grace-периода.
- **FR-3** (must) Daemon владеет watcher'ами, индексацией и sync jobs; закрытие сессии
  отменяет только её работу, общая продолжается.
- **FR-4** (must) Admission: единый canonical cache root; при несовпадении
  version/build/schema/root процесс отказывает **до** выполнения работы и пишет запись в
  conflict-лог.
- **FR-5** (must) `install`/`update` останавливает daemon, ждёт выхода всех процессов,
  затем подменяет бинарь; активная работа не прерывается частично.
- **FR-6** (must) `docsbase daemon stop` — явная остановка; статус возвращается в CLI.
- **FR-7** (must) stdio frontend на каждого агента: MCP JSON-RPC — только stdout; логи — в
  stderr; frontend не зависит от stderr daemon.
- **FR-8** (must) **Cleanup при смерти тонкого клиента.** Если stdio frontend завершился
  (в т.ч. SIGKILL, broken pipe, закрытие stdin), daemon в течение ≤ 2 с обнаруживает это,
  закрывает сессию и освобождает все её ресурсы: socket/fd, per-session locks, snapshot
  readers, отменяет jobs, принадлежащие только этой сессии. Регистрация сессии удаляется;
  «мёртвые» сессии не блокируют самозавершение daemon.
- **FR-9** (must) Admission-барьер crash-safe: после аварийного завершения любого процесса
  блокировка восстанавливается без ручного вмешательства.

### B. Реестр проектов и индексация

- **FR-10** (must) Daemon ведёт **реестр проектов**; подключение незарегистрированного
  проекта не индексирует его молча. `list_projects`/`status` показывают статус
  (not_indexed / indexing / indexed / error).
- **FR-11** (must) Явная команда `index_project` (MCP) и `docsbase index [path]` (CLI)
  регистрируют проект и запускают индексацию; повторный вызов — инкрементальный.
- **FR-12** (must) Конфиг `auto_index` (default **false**): при `true` новый проект
  автоматически регистрируется и индексируется при первом подключении.
- **FR-13** (must) Ключ проекта — canonical root (git root, если проект под git);
  несколько roots дают отдельные записи реестра.
- **FR-14** (must) Индексируются все `.md` под проектом, кроме дефолтных игноров
  (`node_modules`, `target`, `vendor`, `dist`, `build`, `.git`) и паттернов
  `.docsbaseignore`.
- **FR-15** (must) Watcher отслеживает create/modify/delete и запускает переиндексацию с
  debounce ≤ 2 с; всплески (git checkout, массовая правка) коалесцируются в один батч-джоб.
- **FR-16** (must) Инкремент по `mtime` + content hash: byte-identical файлы пропускаются,
  изменённые переиндексируются, чанки удалённых/переименованных файлов удаляются.
- **FR-17** (must) `sync_start` + `sync_status`: явная полная синхронизация с persistent
  job (состояние переживает рестарт daemon); на проект — не более одного активного job,
  повторный `sync_start` возвращает текущий.
- **FR-18** (must) Frontmatter парсится и сохраняется как метаданные (title, tags);
  ошибка frontmatter не валит индексацию файла.
- **FR-19** (should) Настраиваемые лимиты: `max_file_size` (default 1 MiB),
  `max_docs_per_project` (default 20 000); превышение — предупреждение в `status`.

### C. Поиск и чтение

- **FR-20** (must) `search_docs(query, limit?)` возвращает top-k чанков с citation: `path`,
  `heading_path`, `lines`, `score`. Параметр `scope` зарезервирован под shared-слой и в v1
  принимает только значение `project`.
- **FR-21** (must) Exact-identifier поиск: токенизатор делит camelCase, snake_case,
  `X-Request-ID`, `__bt_tt_getProp`; точное совпадение ранжируется выше.
- **FR-22** (must) Chunking по heading-дереву: чанк несёт breadcrumb `file > H1 > H2 > H3`;
  code fences не разрываются; таблицы по возможности целые.
- **FR-23** (must) `get_doc(path)` — полный документ (для случая «прочитать файл целиком»);
  `path` резолвится только внутри зарегистрированных roots, иначе ошибка.
- **FR-24** (must) `read_neighbors(chunk_id)` — соседние чанки для progressive disclosure.
- **FR-25** (must) `list_docs(limit?, cursor?)` — файлы, состояние индексации, размер;
  пагинация обязательна.
- **FR-26** (must) `status` — проекты реестра, документы/чанки, версия схемы, состояние
  watcher, активные сессии.
- **FR-27** (may, фаза 2) Hybrid: vectors (`fastembed` + `usearch`) + RRF + reranker за
  фича-флагом; выключенный флаг сохраняет поведение v1.

### D. Конфиг и CLI

- **FR-28** (must) Глобальный конфиг `~/.config/docsbase-memory-mcp/config.toml` (ignores,
  лимиты, `auto_index`, feature flags) и per-project `.docsbase.toml`.
- **FR-29** (must) Precedence: CLI flags > project config > global config > defaults.
- **FR-30** (should) CLI `index|search|list|sync|status|config|daemon stop|install|uninstall`
  работает без запуска daemon (per-command lease + per-project locks), как CLI-режим CBM;
  команды чтения идут по read-only snapshot, мутирующие берут lease на время команды.
- **FR-31** (may, фаза 3) `docsbase install` автодобавляет MCP-записи Codex/OpenCode
  идемпотентно; uninstall удаляет только owned-записи.

### E. Границы и безопасность

- **FR-32** (must) Все файловые операции — только внутри зарегистрированных roots; symlink,
  ведущий наружу, отклоняется. Паттерны `.docsbaseignore` с `..` или абсолютным путём не
  могут вывести за root; то же правило действует для аргументов MCP tools.
- **FR-33** (must) Socket только localhost, права `0600`; никакого TCP-порта наружу.
- **FR-34** (must) MCP read-only: инструментов записи нет; любой write-запрос отклоняется.

## 6. Non-functional requirements (ISO 25010)

- **NFR-1 Performance.** `search_docs` p95 ≤ 200 мс при ≤ 50 000 чанков на CPU; первичный
  индекс 1 000 md ≤ 30 с; инкремент одного файла ≤ 300 мс + debounce.
- **NFR-2 Resource.** Daemon idle RSS ≤ 150 МБ с одним открытым проектом; индексация
  10 000 документов ≤ 800 МБ. Бинарь ≤ 40 МБ stripped.
- **NFR-3 Reliability.** Один writer на индекс; WAL + crash-safe lock recovery; после
  `kill -9` daemon поднимается, индекс консистентен или помечен на пересборку.
- **NFR-4 Latency.** Изменение файла становится искомым ≤ 2 с (после debounce).
- **NFR-5 Security/Privacy.** Ноль сетевых запросов после установки; ноль телеметрии; логи
  не содержат содержимого документов — только пути и счётчики.
- **NFR-6 Portability.** Один статический бинарь для Linux/WSL2; зависимости — только
  libc. Запуск на x86_64.
- **NFR-7 Maintainability.** Версия схемы в БД; несовместимое изменение — миграция или
  документированная пересборка; каждый `FR-*` покрыт тестом.
- **NFR-8 Observability.** Daemon log + conflict log + `status`; события жизненного цикла
  (daemon start/stop, session join/leave, index start/finish) пишутся структурно.
- **NFR-9 Session hygiene.** 1 000 циклов подключение/смерть клиента не дают роста fd и
  потоков сверх steady state; ресурсы сессии освобождаются ≤ 2 с после смерти клиента
  (проверяется интеграционным тестом).

## 7. Data model

| Сущность | Поля |
|---|---|
| `projects` (реестр) | `id`, `canonical_root`, `name`, `status`, `schema_version`, `created_at`, `last_indexed_at` |
| `docs` | `id`, `project_id`, `rel_path`, `abs_path`, `title`, `frontmatter_json`, `size`, `mtime`, `content_hash`, `indexed_at` |
| `chunks` | `id`, `doc_id`, `seq`, `heading_path`, `kind` (prose/code/table), `lang`, `line_start`, `line_end`, `text` |
| `sync_jobs` | `id`, `project_id`, `state`, `started_at`, `finished_at`, `stats_json` |
| daemon state (файл) | `pid`, `socket` (строка-endpoint), `build_id`, `schema_version`, `cache_root` |

- Хранилище: `~/.cache/docsbase-memory-mcp/` (SQLite WAL + tantivy-индекс per project).
- Реестр — единственный источник списка индексируемых проектов.
- Индексы производные: допускается пересборка из `.md`.

## 8. Interfaces

**MCP tools** (read-only, кроме `index_project` который только регистрирует/индексирует):
`search_docs`, `get_doc`, `read_neighbors`, `list_docs`, `list_projects`, `index_project`,
`sync_start`, `sync_status`, `status`.

| Tool | Вход | Выход |
|---|---|---|
| `search_docs` | `query`, `limit?` (`scope` зарезервирован) | чанки + citations |
| `get_doc` | `path` | полный markdown + метаданные |
| `read_neighbors` | `chunk_id`, `before?`, `after?` | соседние чанки |
| `list_docs` | `limit?`, `cursor?` | файлы и состояние |
| `list_projects` | — | реестр + статусы |
| `index_project` | `path?` (default — cwd) | `project_id`, `job_id` |
| `sync_start` | `project_id?` | `job_id` |
| `sync_status` | `job_id` | состояние + статистика |
| `status` | — | проекты, сессии, watcher, версии |

**CLI:** `docsbase mcp` (stdio frontend), `docsbase serve` (daemon foreground для отладки),
`docsbase index [path]|search|list|sync|status|config|daemon stop|install|uninstall`.

**Конфиг (TOML):** ignores, лимиты, `auto_index` (default false), feature flags
(например `hybrid = false`).

**`.docsbaseignore`:** gitignore-подобный синтаксис, дополняет дефолтные игноры.

**Socket-протокол:** версионированный JSON-RPC поверх Unix socket; `protocol_version` в
handshake; несовпадение → понятная ошибка.

## 9. Measurable success criteria

- **SC-1** Exact identifier: на бенчмарк-корпусе запрос `assessment_plan_id` даёт нужный
  документ в top-3 в ≥ 95% прогонов.
- **SC-2** Error message: запрос
  `Body thickness and Sheet Metal component rule thickness are different` → релевантный
  чанк в top-1.
- **SC-3** Semantic (RU/EN): `How should authentication tokens be refreshed?` → релевантный
  чанк в top-3.
- **SC-4** Code/API: `defineStore setup store syntax` → релевантный чанк в top-3.
- **SC-5** Concurrency soak: 3 агента + watcher 1 час — один daemon, ноль повреждений, ноль
  неразрешённых конфликтов admission при одинаковой версии; рост fd/потоков отсутствует.
- **SC-6** Свежесть: правка → искомость ≤ 2 с (p95).
- **SC-7** Производительность: индекс 1 000 md ≤ 30 с; поиск p95 ≤ 200 мс.
- **SC-8** Офлайн: после установки ни одного сетевого запроса (проверка в network
  namespace).
- **SC-9** Экономия: агент отвечает на «что спека говорит про X» за ≤ 2 tool-call'а, без
  grep/read по репозиторию.
- **SC-10** Явная индексация: незарегистрированный проект не находится поиском, пока не
  вызван `index_project` (или `auto_index = true`).

## 10. Assumptions & challenges

| # | Вызов / допущение | Разрешение |
|---|---|---|
| C1 | Полный CBM-протокол — самый дорогой пункт v1 | Принят; в `design.md` отдельная декомпозиция и risk-регистр; при перерасходе срока режется первым |
| C2 | «Все .md» может затянуть generated/vendor README | Дефолтные игноры + `.docsbaseignore` + лимиты FR-19 |
| C3 | Общие доки между проектами | v1: вендорские — Context7, конвенции — скиллы; shared-слой отложен (P6) |
| C4 | Несовпадение версий даёт плохой UX | Отказ до работы + conflict-лог + понятное сообщение; пересборка индекса допустима |
| C5 | Watcher пишет, пока агент читает | Один writer; читатели — snapshot; WAL |
| C6 | Падение клиента оставляет «мёртвую» сессию и утечки | FR-8 + NFR-9: cleanup ≤ 2 с, проверка отсутствия утечек |
| C7 | Старый тезис «один общий индекс» противоречил per-project | Решено: per-project; research-дока помечена как устаревшая в этой части |
| C8 | `auto_index = false` по умолчанию: агент не увидит доки, пока проект не проиндексируют | `status` и поиск по незарегистрированному проекту дают явную инструкцию вызвать `index_project` |
| A1 | Один локальный пользователь, без auth | Принято; socket 0600, только localhost |
| A2 | Linux/WSL2 достаточно для v1 | Принято; OS-сервисы не используем, lifecycle свой |
| A3 | Только `.md` | Принято; расширение форматов — отдельная спека |
| A4 | Кодировка UTF-8 | Принято; иное — ошибка индексации файла, остальные продолжаются |
| A5 | Context7 доступен для вендорских доков | Принято как внешнее допущение; offline-требование проекта его не касается |

## 11. Out of scope

PDF/DOCX/HTML/веб; векторный поиск в v1; **shared-слой в v1**; write-API; GUI/TUI;
мультиюзер/auth; macOS и Windows; индексация кода; Docker/внешние БД; облачные
embedding/LLM.

## 12. Open questions

| # | Вопрос | Дефолт, если не решено |
|---|---|---|
| OQ-1 | Имя бинаря/пакета | `docsbase` |
| OQ-2 | Стемминг RU/EN | Нет стемминга в v1; lowercase + unicode-токены |
| OQ-3 | Когда возвращаться к shared-слою | После фазы 2; решить по факту боли от дублирования |
| OQ-4 | Лимиты | `max_file_size` 1 MiB; `max_docs_per_project` 20 000 |
| OQ-5 | Формат citation | `path` + heading breadcrumb + line range |
| OQ-6 | Перечитывание конфига | Global — на старте daemon; project — при открытии проекта |
| OQ-7 | Grace-период перед остановом daemon | 5 с |
| OQ-8 | Диагностика без телеметрии | Локальные структурные логи, выключены по умолчанию |
| OQ-9 | Поддержка ARM64 | Не в v1; проверить спрос после Linux x86_64 |
