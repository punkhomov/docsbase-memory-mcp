# Поиск сайта: официальный Node/TypeScript IPC-клиент (#79)

Дата: 2026-11-13. База: 4c9ff9f. Владелец потока B.
Пользователь разрешил непрерывную реализацию, создание PR и squash merge после
независимых reviews и зелёных docs/linux/macos/windows. #79 остаётся открытой до
выполнения серверного этапа и реальной corpus acceptance. #81 — отдельный этап.

## Проблема, пользователи и границы

Node-сайт должен использовать долгоживущий docsbase через локальный IPC, без
subprocess на каждый поиск и без неявной индексации. Оператор управляет одним
зарегистрированным проектом; HTTP-пользователь передаёт только поисковые данные.
Первая поставка — lexical. Ключи, модели, расходы и Rust facade не требуются.

P1: как разработчик Node, хочу типизированный search/status для заранее выбранного
проекта. Given реальный daemon и индекс, When клиент ищет, Then возвращает настоящие
citations/snippets; status не выполняет search и административные операции.
P2: как оператор, хочу конечные очереди и отмену. Given заблокированный search,
When очередь заполнена/запрос отменён, Then результат — типизированная ошибка,
соединение не переиспользуется при незавершённом exchange, status имеет свой pool.
P3: как оператор, хочу пережить restart. Given закрытый daemon connection, When
следующий вызов подключается к новому daemon, Then заново выполняются Hello/bind;
уже отправленный запрос не повторяется автоматически.

## Первая поставка: client contract (FR-C*)

- FR-C1 MUST: Node >=24.22.3, ESM пакет `@docsbase/client`; TypeScript declarations
  и compiled JS, native net Unix sockets/Windows named pipes. Endpoint передаёт
  оператор; socket discovery не угадывает cache hash или произвольный TCP host.
- FR-C2 MUST: один immutable project ID, positive safe integer, на client instance.
  Hello protocol=1, exact caller-supplied expectedBuildId и обязательный
  expectedSchemaVersion. Expected и Hello.schema_version — positive safe integers,
  равенство schema проверяется до bind. Build ID равен `docsbase <CARGO_PKG_VERSION>`:
  exact string match проверяет версию Rust-пакета, не fingerprint исходного commit.
  Require search-context-v1 и panel-read-v1, literal-query-v1 до literal dispatch.
  Read-only BindReadOnly, process.pid; никаких
  RegisterSession/RegisterUnbound/StopDaemon/admin tools.
- FR-C3 MUST: capped NDJSON, arbitrary fragmentation/UTF-8 boundaries; frame limit
  8MiB — существующий wire invariant, не configurable runtime default. Malformed,
  incomplete, oversized, unsolicited/extra response closes connection. Outbound
  limit проверяется до write. chunk_id принимается только как raw JSON number
  token с unsigned integer syntax `^(0|[1-9][0-9]*)$`, в диапазоне 0..2^64-1;
  sign/fraction/exponent отвергаются. Приложение получает canonical decimal string
  без округления; unsafe JSON integers в иных полях отвергаются.
- FR-C4 MUST: search(query, options, call) и status(call), metadata hello getter,
  close(). Нет публичного raw call/tool passthrough. Search options следуют
  search_docs, omitted остаются omitted; не менять ranking/defaults/filtering.
  SearchResult — hits либо {hits,meta}; schema проверяется на runtime, additive
  поля разрешены. Daemon error code/message/instruction сохраняются отдельно от
  transport/protocol/deadline/cancelled/overloaded/closed errors.
- FR-C5 MUST: отдельные bounded search/control pools; один exchange/connection,
  bounded active и pending, отменяемая FIFO queue. Complete policy обязательна
  при create: searchConnections, controlConnections, queueLimit, requestTimeoutMs.
  Один владелец ClientSettings разрешает/валидирует её; transport не вставляет
  defaults. Queue limit per pool; zero = немедленный overload при заполненном pool.
- FR-C6 MUST: монотонный ingress timestamp захватывается первым действием
  search/status, до validation/serialization аргументов. Один absolute operation
  deadline включает preprocessing, admission, connect, Hello/bind, write/read.
  После preprocessing истёкшая операция отвергается до admission/connect/write.
  Search context remaining/elapsed/queue_ms берёт исходную operation, без reset;
  elapsed включает preprocessing, queue_ms измеряет только ожидание pool slot.
  AbortSignal до/во время ожидания; timeout/abort destroys interrupted connection,
  очередь/permits освобождаются ровно один раз.
- FR-C7 MUST: reconnect только в следующем независимом вызове. Никаких retries
  внутри уже начатой операции, autostart, CLI/direct fallback или network provider
  вызовов в connector. Close идемпотентен, отвергает очередь и активные операции,
  закрывает сокеты и не останавливает daemon. Permanent disconnect освобождает
  idle slots. Session/PID supported boundary: same OS user + same PID namespace.
- FR-C8 MUST: Rust-owned protocol fixture связывает JSON shapes/constants/error
  codes и citation schema с Node parser tests. TypeScript compile/build/package
  smoke; package не private, но npm publish/credentials не нужны для этого этапа.
- FR-C9 MUST: Node fake peer tests + real daemon E2E в native CI: search/snippets,
  filter/dedupe, project isolation, readiness diagnostics, disconnect/restart,
  compatibility failure, cancellation, bounded pool. Без real providers/downloads.

## Следующие этапы, не критерий закрытия первого PR

- FR-S1: server profile задаёт один проект, watcher off, daemon без idle exit,
  bounded execution и явно resolved owner settings (CF12). Не менять auto-index
  legacy CLI/MCP; Server profile не делает поиск основанием для индексации.
- FR-S2: explicit CLI sync/readiness, completeness/skips, generation/restart/no-index/
  failed-sync semantics. Пока supported отказ при index mutation: не обещать
  непрерывное чтение старого поколения до CF11. Failed/partial sync не ready.
- FR-S3: runnable Node HTTP example с fixed project, validation, bounded request
  bodies, abort propagation и error mapping; никакого browser-controlled tools/root.
- FR-S4: real ~20k Markdown corpus acceptance с заранее зафиксированными workload,
  budgets и provenance; cold/warm, updates/recovery, RSS/disk/p50/p95/p99/errors.
  Synthetic не заменяет real corpus; сейчас корпус/бюджеты ещё не предоставлены.

## Нефункциональные требования и проверка

NFR-C1: поддержка Linux/macOS/Windows native IPC; required checks прежние.
NFR-C2: ноль runtime npm dependencies, ноль новых Rust crates. Dev tooling pinned,
14-day age policy не обходится. Не менять frozen retrieval inputs/baselines.
NFR-C3: bounded retained parser bytes <=wire limit и bounded queue; pending input
args сериализуются/проверяются до admission, объём каждого <=frame limit. Client
владеет только своим API/transport состоянием; существующий SearchOperation,
generation checks, backend singleflight не дублируются.
NFR-C4: hidden provider requests =0 в lexical E2E; query/error logs не добавляются.

SC-C1: compile + Node suite + protocol fixture test успешны; packing даёт JS/types.
SC-C2: deterministic gate tests доказывают fragment framing, non-replay, queue cap,
FIFO cancellation, one request/connection и independent control admission. Clock
tests включают preprocessing в elapsed/remaining и запрещают I/O при исчерпании
deadline до admission. Handshake tests отвергают missing/invalid/mismatched schema
до bind, даже когда build string совпадает.
SC-C3: реальные native Node→IPC→daemon checks успешны на всех трёх OS.
SC-S1: #79 закрывается только после FR-S1..S4, не после client PR.

## Challenge / assumptions / non-goals

1. expectedBuildId и expectedSchemaVersion обязательны; версия npm не источник
   Rust build/schema. Build string — версия Rust-пакета, не source fingerprint;
   разные commits одной версии могут совпасть, поэтому schema сравнивается явно.
2. RegisterSession запускает config/providers/backfill: используем BindReadOnly.
3. EOF/timeout может случиться после платной работы: никакого replay.
4. JS number не сохраняет u64: chunk_id string, без silent rounding.
5. Read-only binding ещё не гарантирует lexical-config: server этап обязан
   валидировать profile; клиент не обещает override hybrid через rerank=false.
6. Same-user local socket не заменяет HTTP auth; filters не ACL. Cross-container
   PID namespaces и remote TCP не поддерживаются, пока не решены явно.
7. Существующий status registry-wide: клиент отдаёт только bound project, но не
   обещает изоляции от доверенного локального процесса с доступом к socket.
8. Нет hardcoded defaults на transport paths: только resolved ClientSettings;
   protocol constants и технические timer/int representability bounds исключения.

Открытые вопросы первого PR отсутствуют; реальные corpus/budgets нужны только
для FR-S4, не блокируют client/lifecycle implementation.

## Delta 2026-11-13 — MODIFIED: ingress deadline и schema handshake

FR-C6/SC-C2 уточнены: validation/serialization оплачиваются из исходного deadline,
а expiry после preprocessing запрещает admission и I/O. FR-C2/SC-C2 требуют
caller-supplied expectedSchemaVersion и точное safe-positive-integer равенство
Hello.schema_version до bind. Build string описывает package-version equality;
гарантия source-commit identity не заявляется.
FR-C3 уточнён: chunk_id имеет unsigned integer token syntax, u64 range и canonical
decimal string output; C1 проверяет обе границы и запрещённые формы записи.

## Delta 2026-11-13 — целевой корпус пока недоступен

MODIFIED FR-S4 / SC-S1: пользователь сообщил, что сейчас не может предоставить
реальный корпус/нагрузку/целевую среду, и разрешил пропустить этот прогон. FR-S4
откладывается и не блокирует реализацию/поставку client/server этапов. Сохранять
пометку «не проверено на реальном ~20k корпусе»; synthetic E2E не заменяют FR-S4,
capacity/production latency claims запрещены. После FR-S1..S3 допускается закрыть
implementation scope #79 с явным указанием отложенного измерения. Первый client
PR всё ещё не закрывает #79: FR-S1..S3 остаются обязательными.

## Delta 2026-11-13 — C3 native test preparation

MODIFIED SC-C1/SC-C3: every entrypoint executing the full Rust suite with `web`
or `cli-full`, or selecting `node_client` with either feature, must explicitly
build `packages/client` after workspace dependency installation and before test
execution. The ignored `dist/index.js` public export is not produced by install.
This includes the existing nightly release suite as well as required native CI
and documented agent/local commands. Preserve full-suite execution and release
profiles; no test skip, cfg weakening, npm publishing or new job/check is needed.
CLI-only builds, compile-only feature checks, lean suites and selected targets
that do not execute the Node E2E have no client-build prerequisite.
