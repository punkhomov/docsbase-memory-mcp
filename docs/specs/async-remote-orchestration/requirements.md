# Асинхронная оркестрация remote stages — #81

Дата: 2026-11-14. Путь project-design: **Architectural**. Статус: локальный
blueprint, одобренный после fresh challenge; A1 implementation checkpoint,
полное выполнение требований и fresh task review ещё впереди.
Источник: [issue #81](https://github.com/punkhomov/docsbase-memory-mcp/issues/81),
текущий main `61effaf34dc570b0a3ea9c5c5afec4748587bf6f`, после #158/#159.
Дизайн: [design.md](design.md), исполнение: [tasks.md](tasks.md),
доказательства/решения: [progress.md](progress.md).

## Проблема

`ReqwestTransport` уже использует async reqwest, но `Transport`, `Embedder`,
`Reranker` и их consumers синхронны. `call_tool_in` держит search worker permit
весь `route_search`, включая network/backoff. `post_with_retry` спит через
`std::thread::sleep`. Новая библиотека HTTP эту границу не исправит.

У текущего main уже есть один `SearchOperation`, lost-wake-safe async ожидание
`Cancellation`, `WorkerAdmission`, physical `Activity`, `ReadPool`, short read
phases, bounded `OwnedPayload`, config/stages Arc revision и query/dense flights.
Их ownership сохраняется и расширяется. Source issue81 исследовал старый commit;
его line numbers и Rust1.88 не являются implementation baseline.

## Цели, акторы и граница поставки

Акторы: пользователь поиска через native IPC/MCP/CLI, embedding application,
оператор daemon и background index/watch jobs. HTTP website adapter остаётся
внешним consumer SDK; Website profile остаётся lexical-only, watcher-off.

Цель: remote I/O и retry waits освобождают application blocking workers;
локальный FastEmbed/SQLite/lexical/fusion остаются bounded blocking. Все операции
имеют конечные ресурсы и явные cancel/deadline границы, без потери прежних
retrieval и recovery contracts. Не обещаем ускорение до измерений.

Максимум **два функциональных PR**. PR1 даёт работающий async interactive
search в Shared daemon и его IPC/MCP consumers; PR2 завершает background jobs,
direct CLI, lifecycle и metrics и закрывает #81. Документы/traits/settings не
публикуются отдельным PR. После PR1 legacy background/direct ограничения
остаются явно перечислены; #81 ещё не закрыт.

## Приоритетные истории и Given / When / Then

| ID | Приоритет / история | Приёмочный сценарий |
|---|---|---|
| US1 | P1: независимый поиск во время remote wait | Remote fake остановлен в HTTP/backoff / другой lexical query и status пришли / первый не держит search CPU permit или read lease, независимые запросы завершаются при наличии собственных ресурсов |
| US2 | P1: один исходный request budget | Запрос потратил время в очереди/prepare / embedding, retries и rerank выполняются / ни одна стадия или reply не получает новый deadline, после expiry новый HTTP attempt не начинается |
| US3 | P1: coalescing с независимой отменой | Два одинаковых misses одной config/model identity / первый waiter отменён / второй получает единственный результат; failure не становится успешным cache hit |
| US4 | P1: bounded background | Несколько sync/watch/backfill jobs / provider медленный / admission и batch payload ограничены, CPU/control работают в своих lanes, pending job можно отменить до mutation |
| US5 | P2: recoverable indexing | Remote embedding failed/cancelled после markers / процесс перезапущен и explicit sync повторён / незавершённый outcome не считается Ready, прежние retry markers вызывают re-plan, validated vectors переиспользуются |
| US6 | P2: наблюдаемое завершение | Awaiter dropped при local inference/DNS / shutdown requested / реальная closure сохраняет permit/Activity до finish/unwind; статистика не объявляет её остановленной |

## Функциональные требования

- **FR-81-01 (must, PR1).** Добавить настоящий async remote provider path с
  работающим search consumer. Переиспользовать reqwest Client, Rig Wire codecs
  ADR-RIG1, provider registry и существующие stage objects. Ни application
  blocking worker, ни thread-local `blocking::enter` не переживают remote await.
- **FR-81-02 (must, PR1).** Сохранить один исходный `SearchOperation` от ingress
  до reply: queue, prep, query embedding, dense/fusion, rerank, final build,
  transport и reconnect входят в прежний deadline. Ordinary attempt timeout равен
  `min(remote.attempt_timeout_ms, operation.remaining())`. MODIFIED 2026-11-14
  fix1/I2: shared flight фиксирует attempt timer по attempt policy/hard cap,
  personal live-interest максимум применяет outer resettable wait над **тем же**
  HTTP future; новый peer не наследует timeout первого waiter. Cancellation/expiry
  прерывают async wait; граничная проверка непосредственно перед каждой попыткой
  исключает новый attempt при истёкшем бюджете.
- **FR-81-03 (must, PR1).** Retry statuses и error classification сохраняются:
  429/500/502/503/524/529 и transport errors; прежние немедленные failures.
  Default4 attempts, base250ms, ×2, cap4s, Retry-After seconds для429 с cap30s,
  jitter `[0,50)ms`. Backoff/Retry-After/jitter ожидаются async, cancellable,
  в исходном deadline; последний attempt не получает лишний sleep.
- **FR-81-04 (must, PR1).** Shared DB/config/map mutex, metadata transaction,
  `ReadLease`, config preparation guard и project **read** lease не переживают
  remote await. Короткие блокирующие фазы захватывают owned generation/config
  data. Повторный read проверяет тот же generation и captured config revision;
  mutation/reload между фазами даёт прежний explicit retry error. Готовый
  `OwnedPayload` может завершить старый согласованный ответ после reload.
  Uninterrupted old-generation serving не обещается.
- **FR-81-05 (must, PR1/PR2).** Provider active и pending limits раздельны для
  interactive/background; background не занимает interactive remote capacity.
  CPU workers переиспользуют `WorkerAdmission`; добавленные background/helper
  lanes не создают второй executor. Отмена/expiry до admission не запускает
  provider/closure. Physical permit переносится в closure и сохраняется после
  abort/drop awaiter. Новые bodies/retained plans ограничены до allocation.
- **FR-81-06 (must, PR1).** Async identical query misses используют существующий
  query-cache flight registry с model identity + query hash + retained runtime
  revision. Async waiter не блокирует worker. Cancellation одного waiter/leader
  не отменяет live peers; нет active members — shared async work прекращается.
  Failure/unwind/abandon снимают flight, поздний запрос может повторить.
  Только целиком validated success может попасть в completed LRU; stale runtime
  result не публикуется как result/config нового snapshot.
- **FR-81-07 (must, PR1/PR2).** FastEmbed, local model mutex, SQL, vector I/O,
  lexical/dense/fusion и payload construction остаются bounded blocking;
  нет async mutex вокруг всего поиска. Model handles/physical charges живут до
  фактического завершения. Cancellation checkpoints между доступными CPU phases;
  ONNX inference/getaddrinfo принудительно не прерываются.
- **FR-81-08 (must, PR1).** Все новые operational settings и затронутые retry/
  attempt/batch defaults принадлежат ungated component owner до construction.
  Global-only limits не принимают project override. Complete typed settings
  доходят до настоящего consumer, effective values/units/provenance/capability
  наблюдаемы, invalid policy отклоняется до listener/state/provider effects.
  Runtime-policy change требует restart, vector/model identity не меняет.
  Это узкий CF13/I38–I40/I66 adoption, не v2 executor/migration.
- **FR-81-09 (must, PR2).** Full/incremental index, open-time backfill, explicit
  Sync, watcher retries/recovery и direct CLI входят в общий async remote
  boundary. Нет production background sync-over-async ожидания. Batch cache
  lookup → remote → validated write разделены: cache connection/transaction не
  удерживаются на network await. Сохраняются chunk hash/order/dedup, pin TTL,
  whole-batch validation и existing crash markers/outcome/Sync attribution.
- **FR-81-10 (must, PR1/PR2).** Async dispatch сохраняет IPC/scoped search,
  session cancellation, CLI no-replay/fallback и MCP contracts. Один явный
  standalone runtime boundary на direct command, reuse внутри command; runtime
  на request/attempt не создаётся. Вложенный current-thread runtime даёт явную
  admission error, не panic. Connect/provisioning probes остаются single-shot
  compatibility boundary, без новых provider calls.
- **FR-81-11 (must, PR1/PR2).** Физическая инвентаризация включает hidden DNS.
  Pinned reqwest default GAI `spawn_blocking` заменён bounded resolver через
  существующие worker/Activity owners. Drop HTTP future не освобождает ещё
  выполняющийся DNS. Shutdown закрывает admission, отменяет queued/async work
  и дожидается tracked physical closures, включая detached flight leaders и
  job finish/error publication. Имеющиеся signal escalation semantics сохраняются.
- **FR-81-12 (must, PR2; core counters PR1).** Измерять отдельно admission queue,
  CPU phase execution, HTTP wait, backoff и end-to-end, provider attempt count,
  active/pending interactive/background/CPU/helper work и cancelled operations.
  Additive versioned diagnostic projection в search explain/status; без keys,
  queries, documents, raw responses или unbounded identity labels. Cumulative
  durations не объявляются partition wall-clock latency shared flights.
- **FR-81-13 (must, обе поставки).** Preserve ranking, query/document roles,
  dimensions, finite f32 validation, normalization, model/vector identities,
  candidate windows/refill/filter/snippet ordering, no-silent-fallback, endpoint
  allowlist/secret redaction. Не менять golden/baseline/ignores/budgets и не
  делать live provider/model-download calls в acceptance.

## Нефункциональные требования (ISO 25010)

- **NFR-81-01 — reliability/resource use:** active/pending и retained bytes
  проверяются счётчиками и gates. Async wait само по себе не доказывает bounded
  executor, RSS, fd или process-wide capacity. Отдельные existing ANN/native/
  watcher producer resources инвентаризированы и не выдаются за Tokio workers.
- **NFR-81-02 — maintainability/portability:** Rust1.100.0, Node24.22.3,
  pnpm12.16.1; Linux/macOS/Windows cli-full с обоими providers/Web, lean offline,
  remote-only/local-only/combined source configurations. Новых crates/runtime
  npm dependencies не требуется. Production unwrap/expect/panic запрещены.
- **NFR-81-03 — security:** provider access остаётся явно включённым,
  credentials не входят в settings fingerprint/view, logs и assertions.
  Retained input/response caps проверяются по actual bytes, не Content-Length.
- **NFR-81-04 — evidence:** TDD actual RED → GREEN, fresh task/final review,
  full project gates/native required CI. Синтетический controlled gate доказывает
  ownership/order, не throughput, production latency или provider billing cancel.

## Модель данных и интерфейсы

Новые on-disk tables/formats не нужны. Сохраняются `VectorCacheSource` и
`VectorStore` owner, `IndexOutcome` attempt и generation, Sync record association.
In-memory добавляются complete `RemoteSettings`, retained query flight revision,
owned resumable search/index state и bounded counters. Имена будущих Rust APIs
в [design.md](design.md) отмечены как proposed; это не claim об уже имеющемся API.

## Измеримые критерии и traceability

| ID | Наблюдаемая проверка | FR / задачи |
|---|---|---|
| SC-81-01 | Gated real async search удержан в fake HTTP и retry; CPU lane свободен, другой lexical/status проходит; ноль SQL/read/config leases во время gate | 01,04,05,07,13 / A1,A2,A3 |
| SC-81-02 | Queue/HTTP/backoff/Retry-After/jitter/multistage/reply cancellation/expiry; attempt count после deadline неизменен, последний sleep отсутствует | 02,03,05,10 / A1,A2,A3 |
| SC-81-03 | Identical misses один call; разные model/revision не сливаются; failure/unwind retry; leader/waiter cancel/drop и reload/stale publication | 06 / A2,A3 |
| SC-81-04 | Owner override/default reaches actual HTTP timer/retry/batch/admission; effective/scope/feature-off/restart impact и invalid-before-effects | 08 / A1,A3,B3 |
| SC-81-05 | Actual DNS/local fake closure после HTTP/request drop остаётся charged; queue bounds/zero-pending/FIFO/shutdown drain без преждевременного release | 05,07,11 / A1,A3,B2 |
| SC-81-06 | Full/incremental/watch/Sync/backfill/direct CLI проходят async consumer tests; mutation markers/failure/restart/no-op/partial and Sync attribution сохранены | 09,10,13 / B1,B2,B3 |
| SC-81-07 | Metrics distinguish controlled queue/CPU/network/backoff/end-to-end/counts, не содержат secrets/input; official suites/golden/offline/native CI green | 12,13,NFR / A3,B3 |

## Предположения и challenge решений

| Проверяемый вопрос | Решение/default |
|---|---|
| Достаточно async transport trait? | Нет: PR1 обязан включать настоящий Shared search consumer и физическую проверку CPU permit |
| Query embedding зависит от index generation? | Нет: persistent model recipe и completed pure-cache identity сохранены; active flight дополнительно fence-ится retained runtime revision |
| Deadline первого waiter — deadline всех? | Нет: каждый waiter сохраняет свой budget; shared flight имеет ограниченный hard lifetime и live-interest budget, детали D5 |
| Можно отменить DNS/ONNX abort-ом? | Нет: charge в actual closure; HTTP future cancel не означает прекращение принятой provider работы/оплаты |
| Writer lease над network нарушает search contract? | Read/config/SQL leases запрещены. Index job может держать owned exclusive project writer lease с прежним explicit same-project refusal; это не old-generation serving |
| PR1 уже доказывает control reserve всего Shared? | Нет: legacy background до PR2 остаётся вне такого утверждения; accepted issue81 закрывается только совокупностью PR1+PR2 |

## MODIFIED 2026-11-14 — обязательные уточнения fresh challenge fix1

Эта delta уточняет existing FR/SC, не меняет два функциональных PR и не объявляет
runtime acceptance. Дизайн D2/D3a/D5/D6/D7a/D7b — обязательные interfaces:

- FR-81-01/07/10/11, SC-03/05/06 (I1): operational transport/client/stage не
  владеют fallback Runtime. Command Runtime создаётся/уничтожается вне block_on
  в одном sync/block_in_place owner после drain; current-thread nesting отказана
  до construction. Async last-Arc/stale/error cleanup не вызывает Runtime Drop.
- FR-81-02/06, SC-02/03 (I2/M1): fixed shared attempt clock независим от personal
  deadlines; dynamic interest repolls тот же pinned attempt/admission/backoff.
  A early + B late + attempts1 обязан дать B success с ровно одним provider call.
  Register-before-check/permanent state и selection/join/publication/retiring
  races проверяются causal gates; terminal flight не оживляется late join.
- FR-81-08, SC-04 (I3): recognized RemoteSpec type/value/derived count failure
  fatal в actual legacy Shared launch до effects, даже с unrelated bad legacy
  field. Unrelated old Shared fallback сохраняется. Owner section/provenance
  сохраняется в v1 roundtrip и connect provider write; project key запрещён.
- FR-81-05/07/09/11, SC-05/06 (C1): отдельны logical job L и physical phase J;
  заранее выданный bounded terminal ticket даёт cleanup T=1 после shutdown новых
  lanes. JobOwner Drop только переносит unique IndexState; finalizer publishes
  terminal Sync/outcome с captured attribution и destroys IndexHandle/Db/payload
  до writer lease, затем CPU permit и Activity. Native destructor не блокирует
  Tokio, не освобождает lease/bytes/drain преждевременно, в том числе при abort/
  delivery failure/unwind и J=L=1. Capacity formula `S+C+J+H+T<=B`.
- FR-81-05/07/08/09, NFR-01/03, SC-05/06 (I4): usable existing PayloadPermit
  grow-before-copy guard считает paths/maps/hashes/chunks/current-file и peak
  encoded/transient clones до allocation. Low-budget instrumented test доказывает
  denial **до следующего capture**, charge/live retained bound и refusal до
  project status/attempt/markers/index changes; final error alone недостаточен.

## Вне scope и открытые вопросы (сохранённая граница)

Вне scope: #47/#48/#117/#128, generation/ask/new providers/harness/SDK migration,
#155 native adapter/probes/research, #80 facade, #71 paid quality recordings,
full CF5/CF10/CF11/CF12/CF13, provisioning/download redesign, new ranking/baseline,
uninterrupted old generations, Website hybrid activation, capacity/SLA claims,
environment publishing/reapply и npm releases. Блокирующих пользовательских
ограничений не обнаружено; решения в design ждут fresh root challenge до кода.
