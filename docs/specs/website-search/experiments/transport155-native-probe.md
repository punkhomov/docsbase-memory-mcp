# #155: временный Windows native probe

Дата: 2026-11-13. Статус: **статический черновик; Windows compilation и native
execution не проверены**. Это Spike по project-design. Результат будущего запуска —
наблюдение поведения одного Windows build, а не сохраняемый продуктовый код.
Production `src/**` не меняется. #155 остаётся открытой до отдельного решения
для реально принадлежащего daemon transport и его E2E-проверки.

## Основание, границы и этапы

Текущая база: `92c3b61678d4204f5687718abf86ec312181a0d3` (main после #157). Ветка:
`experiment/windows-transport-retirement-155`. Утверждённый bounded plan:
`/workspace/scratch/agent-b/79/transport155-pending-probe-plan.md`, SHA-256
`96c90d99a3db9ef7ead8d2c87ee921e25187a82d122ca3caddbecd7a5f125623`.
Историческая база утверждённого плана: `1da8f6ba9bcb5e4cbf9b6836f12c9f25409d7b78`;
перенос draft на current main не меняет native protocol.
Fresh plan review: `transport155-probe-plan-review.md`, PASS без оставшихся
findings. Investigation и официальные snapshots находятся рядом, в
`transport155-investigation.md` и `transport155-docs/`; hash/provenance входят
в пакет статического review `/workspace/scratch/agent-b/155/`.

Порядок: static draft → fresh whole-diff review → разрешение controller →
один exact-ref Windows dispatch → чтение исходных результатов → новое решение.
В этом этапе нельзя push, dispatch, открывать PR или merge. Нельзя переносить
заменяющий `ci.yml` в main. Сохранение probe-кода требует отдельного запроса.

Исходная reachable проблема: dirty Drop в interprocess может передать handle
в dependency-owned linger thread, выполняющий синхронный `FlushFileBuffers`.
Обычный local-socket flush/shutdown является no-op; отдельный raw explicit flush
не является обычным daemon reply path. Этот probe не вызывает interprocess,
Tokio, Mio, daemon или StopDaemon. Он не проверяет pending writes, private linger
pool, owner accounting, admission/capacity, quarantine или конечное завершение
production shutdown. Не добавляются crates, lockfile, product features,
модели, внешние peers или runtime network calls.

## Проверяемое утверждение

Успешный `GetThreadIOPendingFlag` с отдельным выходным TRUE — sampled observation
pending I/O на указанном thread. Свежий worker вызывает ровно один
`FlushFileBuffers` и не выполняет другого I/O. При соблюдении fixture invariants
TRUE можно отнести к этому flush. Это не atomic guard pending-at-abort, operation
token, completion event или гарантия, что flush вообще будет видим через flag.
FALSE не доказывает completion; ошибочный query не доказывает TRUE/FALSE.

`CancelSynchronousIo` только запрашивает cancellation. Его TRUE не освобождает
owner и не доказывает возврат flush. Даже FALSE/NOT_FOUND требует отдельного
наблюдения flush. Для `DisconnectNamedPipe` нет проверенной гарантии о результате
параллельного flush; disconnect может выбросить непрочитанные данные.

## Ресурсы и владение

`tests/windows_transport_probe.rs` имеет Windows-only test и явно требует x64.
Ровно четыре case запускаются по одному в свежих дочерних копиях этого test exe;
первая ошибка останавливает suite, без rerun. Одновременно живёт не более одного
child. В нём controller, максимум один raw flush worker, один client actor и
один abort actor. Fixture: один byte/wait pipe instance, два endpoint handle,
14 байт `{"probe":155}\n`, advisory output quota 4096. Server open flags
`PIPE_ACCESS_DUPLEX | FILE_FLAG_OVERLAPPED | FILE_FLAG_FIRST_PIPE_INSTANCE`
(`0x40080003`), pipe mode byte/read-byte/wait plus `PIPE_REJECT_REMOTE_CLIENTS`
(`0x8`). Client синхронный, с read/write access. Нет IOCP association или
повторного connect/write/flush/abort submission. Короткие успешные client reads
могут продолжаться до полного frame, максимум 14 native reads; ошибка не retry.

Controller сначала подаёт один overlapped connect, открывает client, наблюдает
terminal connect, подаёт один overlapped write и наблюдает terminal ровно 14 байт.
`OVERLAPPED`, events и static frame сохраняются до completion, в том числе при
ошибке. Worker создаётся только после успешного setup. Client до отдельного
read gate ничего не читает и endpoint не закрывает. Имя включает child PID/case;
один instance и isolated disposable runner — предпосылка отсутствия внешнего peer.
Remote clients запрещены. Это не проверка защищённости production pipe ACL.

`CreateThread` даёт новый реальный retained full-access handle, включая
`THREAD_QUERY_INFORMATION`, `THREAD_TERMINATE` и `SYNCHRONIZE`. Не используются
pseudo-handles, remembered thread IDs или pool reuse. Raw worker body: atomic
publication/Win32 events → entry wait → **один** `FlushFileBuffers` → немедленный
raw BOOL/error → exit-event park. Он не вызывает CRT, logging, file/pipe
setup/read/write или Rust thread teardown до завершения query/abort window.
Контекст не освобождается до actual thread signaling. Остальные actors также
используют retained native thread handles; client один владеет своим endpoint и
закрывает его только после завершения собственных reads.

Все child contexts, fixture, mapping и cleanup-event ownership намеренно
сохраняются до process teardown. Это ограниченная process-owned память, а не
production quarantine. Такая схема исключает Drop живого `OVERLAPPED`/handle
на error path. Сам process teardown запускается только после cleanup gate при
нормальном протоколе и не объявляется гарантированно конечным. Supervisor имеет
свою mapping view и owned Child process handle. Он не зависит от child stdout,
чтобы получить уже опубликованные native результаты.

### Shared publication

Supervisor до spawn создаёт named page-file mapping в `Local` namespace и named
manual-reset cleanup event, initially unsignaled. Mapping collision — failure
без retry. `Records` содержит 64 fixed slots, два сообщения по 256 atomic bytes,
счётчики/flags; размер проверяется `<= 65536`. Mapping page alignment и Windows
x64 lock-free `AtomicU32`/`AtomicU64` — явные предпосылки реализации.

Каждый slot имеет одного writer, CAS `0 → u32::MAX` claim, atomic payload,
diagnostic `child_cleanup_sample` и final nonzero publication sequence. `u32::MAX` не является
опубликованным record. Короткий child-only SeqCst atomic publication lock
сериализует заполнение и sequence publication; под ним нет native call.
Это interlocked publication посредством Rust x64 atomics, без handwritten ABI
или предположения об экспортируемых SDK intrinsics. Повторная запись slot
выставляет `duplicate_publication` и проваливает case. Если writer завис внутри
publication, supervisor не берёт этот lock: он сохраняет доступные slots и
считает отсутствующие evidence failure. Эта замена interlocked intrinsic на
Rust atomics должна быть явно оценена fresh draft reviewer.

Slots после publication неизменны. Supervisor читает только опубликованные
atomic поля; sequence описывает порядок harness publication, не kernel entry
или момент cancellation. Из-за concurrent scans порядок JSONL lines может
отличаться от sequence; сортировать нужно по полю sequence. Snapshot не является
одним atomic kernel-state снимком. `CandidateReady` — барьер завершения child
candidate protocol; на watchdog неполный snapshot никогда не означает pass.

## ADDED 2026-11-13 — native feedback fix1

Run43499988847 на reviewed7fa2754 успешно скомпилирован на Windows Server2025
build26100 AMD64. NormalDrain сохранил valid CandidateFrozen: положительный
pending sample, все14 байт, successful flush, post-terminal FALSE/1168 и worker
signaling. После successful NormalDisconnect cleanup read вернул FALSE/233,
0bytes; client close/signaling прошли. Прежний cleanup predicate ошибочно отверг
этот documented-disconnect outcome; run остался RED без forced kill. Остальные
три case не запускались. Raw artifacts сохранены без изменений.

В существующей normal post-drain cleanup ветке (NormalDrain и cleanup drain
CancelBeforeEntry), только после full frame, successful flush/worker retirement
и successful NormalDisconnect дополнительно допускается FALSE/
ERROR_PIPE_NOT_CONNECTED233 с0bytes. Raw ClientEndRead233 не нормализуется.
Official DisconnectNamedPipe remarks обещают ошибку доступа forced-off client,
но не конкретный код;233 установлен actual run и locked binding. Candidate
predicates, pending cancel/disconnect acceptance, phase boundaries и watchdogs
не меняются. Этот новый commit ещё не compile/native GREEN; нужен fresh scoped
review и один новый exact-head Windows run. #155 не закрывается.

## Точные четыре case

Общее начало: worker публикует `AtEntryGate`, сигналит event и ждёт. Controller
наблюдает gate и делает один control query: API success + выход FALSE обязательны.
После `EntryReleased` query/yield loop относится к тому же thread/flush:
максимум 100000 queries и 5 секунд, проверка deadline и `FlushReturned` перед
query, immediate last error при query failure, `SwitchToThread` после FALSE.
Нет sleeps, повторного flush/abort, увеличения frame или pending inference по
времени, thread counts, RSS или latency. Exhaustion либо completion без TRUE —
`NoPositivePendingObservation`, failure/inconclusive, не skip. TRUE сохраняет
query interval/BOOLs и reader gate/read-start state. Это sampled observation;
completion может выиграть до abort.

| Case | Candidate phase | После `CandidateFrozen` |
|---|---|---|
| `NormalDrain` | TRUE при нулевых reads → открыть read gate → сравнить все 14 байт и получить raw successful flush → один post-terminal cancel, FALSE/1168 обязателен → завершить abort activity, release worker exit, наблюдать actual worker signaling. | Обычный disconnect после full bytes и successful flush; client end-read/close, actor/process signaling. |
| `CancelBeforeEntry` | На entry gate cancel один раз, FALSE/1168 обязателен → затем намеренно разрешить native entry → TRUE при непрочитанном/open client. Результат `PreEntryCancelDidNotPreventLaterPending`; flush ещё не имеет terminal publication. | Только cleanup drain полного frame, successful flush, worker signaling, обычный disconnect/end-read/close. Этот flush result относится к cleanup и не делает pre-entry cancellation успешной. |
| `CancelAfterPositivePending` | TRUE при непрочитанном/open client → abort actor один раз вызывает cancel → независимо получить raw abort и raw flush, дождаться abort actor, затем worker exit/signaling. Для candidate code 3 нужен FALSE/995 от flush. NOT_FOUND от cancel не заменяет flush completion. Normal success или другая flush error сохраняется как terminal raw status, но проваливает cancellation candidate. | Client закрывает только свой endpoint. Late flush остаётся cleanup evidence. Нельзя вызывать disconnect как rescue. |
| `DisconnectAfterPositivePending` | TRUE при непрочитанном/open client → abort actor один раз вызывает disconnect → независимо получить raw disconnect и любой terminal flush → abort/worker signaling. TRUE disconnect требуется для candidate code 4; FALSE сохраняется и даёт failure. Ни один flush status здесь не доказывает normal delivery. | Только затем client делает один access/end-read, сохраняет exact status/byte и закрывает свой endpoint. Второго disconnect нет. |

Candidate codes 1/2/3/4 соответствуют cases в таблице; 100 — explicit candidate
failure. Native unexpected errors не подавляются: raw status остаётся в slots,
а более строгие acceptance checks дают failure. В частности, другой terminal
flush error после cancel — наблюдение failed I/O, а не successful cancellation.
NormalDrain — обязательный controlled completion-won и post-terminal NOT_FOUND
control. Не вводится искусственный порядок между client `ReadComplete` и worker
`FlushReturned`, которые могут публиковаться в любом порядке. Нормальный final
disconnect отложен за snapshot; actual worker signaling проверяется до него.

## Candidate boundary, failure и watchdogs

MODIFIED 2026-11-13 (fix1, review I1/I2): supervisor один раз копирует slots,
flags, опубликованные error texts и parent early-exit observation в собственный
fixed `Snapshot`. Validation, reason и сериализованный `CandidateFrozen.snapshot`
читают только этот immutable owned value, без повторных чтений mapping. Это
снимок evidence, а не atomic native-state observation. Только supervisor создаёт
immutable JSONL `CandidateFrozen`, включая valid/reason и именно этот snapshot.
`flush`, `sync_data` и stdout flush завершаются
**до** `CleanupBegin`, shared cleanup flag и `SetEvent(cleanup)`. Child никогда
сам не открывает cleanup. NormalDrain reads — часть candidate delivery action;
любые cleanup drain/read/close/kill следуют за freeze. Даже hung query/abort или
child controller не мешает parent заморозить failure по отсутствующим records.

После capture phase определяется supervisor по точному membership
`(slot, publication_sequence)` в frozen snapshot. Slot, отсутствующий в нём,
публикуется как `phase=Cleanup`, `phase_basis=AbsentFromFrozenSnapshot`, даже если
writer успел прочитать shared cleanup=0 до freeze, а commit сделал после начала
cleanup. Included slot получает Candidate/FrozenSnapshot. До capture supervisor
может печатать уже захваченные records как Candidate/BeforeCandidateFreeze;
неизменность published slots гарантирует, что они будут включены в frozen
snapshot. Child `child_cleanup_sample` сохраняется отдельно и не определяет phase.
Late label консервативен: он включает commit между capture и release cleanup,
не утверждает, что cleanup вызвал native return, и не является atomic kernel
ordering. Запоздалый commit не может стать Candidate после cleanup. Поздние
records не пересчитывают `CandidateFrozen.valid`. Candidate pass дополнительно
требует
ожидаемых control/setup/pending records и signaling. Final case pass требует
неизменного valid candidate, successful child exit, observed worker/client
signaling, `CleanupComplete`, отсутствия duplicate publication и forced kill.

Фиксированные failure-containment budgets, не performance criteria:

| Граница | Лимит |
|---|---:|
| Setup connect/write | 5 s total |
| Worker entry gate wait | 5 s, внутри whole-case limit |
| TRUE observation | 5 s и максимум 100000 queries |
| Candidate action + terminal/actor observation | 5 s |
| Whole case, включая launch attempt | 20 s |
| Parent failure cleanup | максимум 5 s |
| После одного `TerminateProcess` | максимум 5 s wait |
| Suite supervisor | 125 s; последние waits обрезаются этим deadline |
| Независимый PowerShell supervisor-process watchdog | 180 s + максимум 5 s reap |
| Независимый native-run step timeout | 5 min |
| Независимый hosted job timeout | 60 min, включая подготовку/build |

Raw child event waits могут быть INFINITE: это только удерживаемые child actors
и parked controller; supervisor никогда не делает INFINITE wait. Потенциально
зависшие native calls, spawn, файловая запись самого evidence и process teardown
также требуют независимого внешнего watchdog/job/VM boundary; одни Instant и
WaitForSingleObject не являются wall-clock/kernel-cleanup доказательством.

На timeout сначала freeze failure, затем разрешить client cleanup. Нельзя
закрывать handle из другого actor против живого read/flush/abort. После 5 s
parent вызывает `TerminateProcess` один раз **только на owned child**, сохраняет
raw BOOL/error, ждёт process handle максимум 5 s. Termination асинхронна;
pending I/O может препятствовать exit. Любая forced termination — failure.
`ChildNotReaped` — failure/inconclusive, suite останавливается, новый child не
создаётся. Ни TerminateThread, ни finite-kernel-cleanup claim не допускаются.
Missing `CandidateFrozen`, missing artifacts, timeout/job cancellation либо
unobserved signaling — незавершённый/failed run, никогда не pass.

Workflow допускает только `workflow_dispatch` на exact experiment branch и
сверяет required `expected_revision` с `github.sha`. Один Windows-only job;
обычные docs/linux/macos/windows jobs на этой временной ветке заменены. Exact
Rust 1.100.0, Node из `.node-version` (24.22.3), Corepack/pnpm, frozen lockfile и
repository cooldown/trust policy сохраняются. До Cargo строятся Web assets и
Node client; Cargo использует `--locked --features cli-full --test
windows_transport_probe --no-run`. Cache сохраняет registry, но не target,
чтобы прежние evidence не попали в новый run. Test exe запускается напрямую
один раз, без второго cargo execution или broad suite.

Нужен **GitHub-hosted** Windows runner, не self-hosted. Workflow проверяет
`runner.environment`, записывает OS/build/image/ref/run/exe hash и имеет
independent job timeout. Контракт disposable VM controller должен подтвердить по [GitHub-hosted runners](https://docs.github.com/en/actions/concepts/runners/github-hosted-runners):
ожидаемая граница — fresh VM на каждый обычный Windows job. Удаление VM после
job и работа service timeout — внешняя предпосылка, не свойство этого Rust кода;
эта GitHub contract page не fetch-verified в static-draft этапе и фактический
runner lifecycle локально не проверен. Если controller не может установить этот контракт
для выбранного runner, запуск запрещён. `always()` artifact upload пытается
сохранить весь `target/probe155/`; после hard host disposal доставка artifact
не гарантируется и её отсутствие означает incomplete evidence.

PowerShell watchdog отдельно сохраняет/flushes launch, owned supervisor PID,
expiration, один Kill request и finite reap. Он не объявляет descendant child
убитым после Kill supervisor; окончательная containment — VM disposal.

## Result schema v1

Основной файл: `target/probe155/cases/<Case>.jsonl`; те же supervisor JSON lines
выводятся в stdout и попадают в `supervisor.stdout.log` (вместе с libtest output; это не strict JSONL). Каждый line имеет
`schema=1`, `case`, `operation_id=1`, `supervisor_pid`, `kind`, `data`.

Kinds: `CaseLaunch`, `ChildSpawned`, `NativeRecord`, `CandidateFrozen`,
`CleanupBegin`, `SupervisorWaitFailure`, `CleanupWaitFailure`, `TerminateRequested`,
`TerminateReturned`, `ReapWaitFailure`, `ChildNotReaped`, `CaseVerdict`.
`CandidateFrozen.data` содержит `valid`, `reason`, `snapshot`; `CaseVerdict`
содержит final pass, child_signaled, exit_code, forced, candidate_unchanged,
final snapshot. Snapshot содержит slots (index, publication_sequence,
child_cleanup_sample,
args), duplicate_publication, captured read flags, опубликованные error texts и
child_exited_before_candidate. MODIFIED 2026-11-13 (fix1, I1/I2): эти поля копируются
один раз; именно их использует candidate validator.
`NativeRecord.data`: publication_sequence, event, supervisor phase (`Candidate`/`Cleanup`),
phase_basis (`BeforeCandidateFreeze`/`FrozenSnapshot`/`AbsentFromFrozenSnapshot`),
независимый diagnostic child_cleanup_sample, восемь unsigned u64 `args`, optional
error_message. BOOL сохраняется как
32-bit bit pattern внутри u64; zero означает FALSE, nonzero TRUE. Last error
равен 0 при BOOL success и валиден только при failure. Pending output BOOL
валиден только при query success. Wait args сохраняют raw WAIT_* и immediate
last error только при WAIT_FAILED. Неиспользуемые args равны 0.

| Event/slot | `args` слева направо, остальные 0 |
|---|---|
| ChildStarted (0) | child PID, case ID |
| ConnectSubmitted (1), WriteSubmitted (4) | raw BOOL, error, immediate byte count (для connect 0) |
| ClientOpened (2) | successful handle creation flag, error |
| ConnectCompleted (3), WriteCompleted (5) | GetOverlappedResult BOOL, error, terminal bytes |
| SetupComplete (6) | open flags, pipe mode, requested output quota, expected bytes |
| WorkerCreated (7) | worker thread ID, retained handle numeric identity (diagnostic only) |
| ClientCreated (8), AbortThreadCreated (12) | actor thread ID |
| AtEntryGate (9), EntryReleased (16), ReadGateReleased (20), ReadStarted (21), WorkerExitReleased (24), CleanupObserved (28) | event marker; no native-pending inference |
| EntryWait (10), AbortSignaled (15), WorkerExitWait (25), WorkerSignaled (26), ClientSignaled (32), ConnectWait (37), WriteWait (38), ClientGateWait (39), WorkerGateWait (55), CleanupAbortWait (56), CleanupGateWait (57) | raw wait result, error; ClientGateWait distinguishes cleanup index 0/read index 1 |
| ControlQuery (11) | API BOOL, output BOOL, error |
| AbortBegin (13) | 0 cancel / 1 disconnect |
| AbortReturned (14) | raw BOOL, error, 0 cancel / 1 disconnect |
| QueryBegin (17) | query cap, observation budget ms |
| QuerySummary (18) | count, FALSE count, last API BOOL, output BOOL, error, begin ns, end ns, exhaustion reason (0 native result, 1 deadline/completion, 2 cap) |
| PendingObserved (19) | count, begin ns, end ns, API BOOL, output BOOL, read_released, read_started |
| ReadComplete (22) | total bytes, exact comparison BOOL, bytes 0..7 packed little-endian u64, bytes 8..13 packed little-endian and padded with two zero bytes |
| FlushReturned (23) | raw BOOL, error |
| CandidateReady (27) | candidate code 1/2/3/4/100 |
| NormalDisconnect (29), ClientClose (31) | raw BOOL, error |
| ClientEndRead (30) | raw BOOL, error, byte count, byte value |
| CleanupComplete (33) | 1 |
| ChildError (34), CleanupError (54) | raw OS error if available, byte length; text до 255 bytes в fixed shared error storage |
| WorkerFault (35) | stage, error |
| ClientFault (36) | stage; stage 1 wait status/error; stage 2 oversized count; stage 3 cleanup wait result/error; stage 4 read error/chunk index |
| ReadChunk0..13 (40..53) | raw ReadFile BOOL, error, chunk bytes, accumulated bytes |

`QuerySummary` reason 1 без выполненного final query имеет нулевые BOOL/error,
count агрегированных FALSE и только end ns; они не изображают native failure.
Query intervals — controller-local monotonic elapsed ns от observation start;
никакого сравнения cross-thread clocks или performance threshold нет.
Actor faults/OS errors и cleanup error texts сохраняются; final verdict их не
переписывает. Child harness stdout/stderr идёт отдельно в `<Case>-child.log`.
Outer `external-watchdog.jsonl` имеет schema/kind/utc/data; это orchestration,
не native pending/cancellation evidence. `host.json`, exact tool versions,
build stdout/stderr, exe path/SHA-256 сохраняются даже при неудачном build.

## Bindings, feature gates и официальные контракты

Используются только готовые `windows-sys 0.61.2` bindings. Временное расширение
его существующей dependency feature list согласовано controller в static-draft
этапе: исходный scope «только System_IO» был недостаточен для supervisor-readable
mapped evidence. Product Cargo features, версии и Cargo.lock не меняются.

| Добавленный windows-sys feature | Реально нужные API/types |
|---|---|
| `Win32_System_IO` | CancelSynchronousIo, OVERLAPPED, GetOverlappedResult; также gate для overlapped pipe/file bindings |
| `Win32_Security` | SECURITY_ATTRIBUTES gating CreateThread, CreateEventW, CreateNamedPipeW, CreateFileW, CreateFileMappingW; передаётся null/default security |
| `Win32_System_Memory` | CreateFileMappingW, OpenFileMappingW, MapViewOfFile, MEMORY_MAPPED_VIEW_ADDRESS, UnmapViewOfFile и mapping constants |

Существующие Threading/Foundation/FileSystem/Pipes покрывают query, events,
thread/process waits, TerminateProcess, flush, setup reads/writes/disconnect.
Каждый unsafe block содержит SAFETY с сохранёнными handles/context/buffers;
OwnedHandle используется для ownership, explicit client CloseHandle переводит
ownership через into_raw_handle и сохраняет raw status. Новые ABI declarations,
SDK-version assumptions и дополнительные crates отсутствуют.

Проверенные official MicrosoftDocs SDK API snapshots (source ref `docs` mutable;
SHA-256 идентифицирует прочитанные bytes, а не утверждает native outcome):

| API / SDK source path после `https://raw.githubusercontent.com/MicrosoftDocs/sdk-api/docs/sdk-api-src/content/` | SHA-256 |
|---|---|
| `processthreadsapi/nf-processthreadsapi-getthreadiopendingflag.md` | `351020f1f080ea861bda12f7d93ee2c30dfef3bf2ddd9dd37c8e9d7caf855bbf` |
| `processthreadsapi/nf-processthreadsapi-createthread.md` | `89f4e0ca386929e4168896f4fcd97ee9cc99e36c2d082a9501e0ca09bd81605a` |
| `synchapi/nf-synchapi-waitforsingleobject.md` | `29474b87ad9e518fbb7e7bc1d2169646f8f3b00c7e6d332f64309c16a8b038dd` |
| `processthreadsapi/nf-processthreadsapi-terminateprocess.md` | `eb08e9fcf4166a9e0b2907e707a0bc25ea75b831ea0ce4f1992a85b542f720a0` |
| `ioapiset/nf-ioapiset-cancelsynchronousio.md` | `90af1ea3db75b30caa0aadb3a4be0654907ea8728c6bafdbac64a8ec93ba743c` |
| `fileapi/nf-fileapi-flushfilebuffers.md` | `6722ff0b4d1f58ef57f9894d06fd0fdf77d019f6eedaa101771f8c6a28f4e395` |
| `namedpipeapi/nf-namedpipeapi-disconnectnamedpipe.md` | `0c653684ec5fda9ce821608c6cf702ec84fd1702a2198be0b7babd8ba8e262ea` |

Cancellation considerations snapshot hash:
`cbcdf0433ea93b23e67a7437428a6470d6b0c91167dfa1da363cb0ffca60393b`.
Он прямо предупреждает об отсутствии гарантии driver cancellation и об опасности
thread reuse. Cached bindings/hash manifest и fetch provenance включены в draft
review package. Mapping/event/setup API signatures сверяются с locked bindings;
их поведение/права/совместимость остаются предметом Windows compile/native review,
а не выдуманным SDK доказательством.

## Команды и допустимый результат

Локально разрешены только direct pinned rustfmt и static checks; Cargo/tests/
builds не запускались: единственный local Cargo slot занят независимой задачей.
Фактическая статическая проверка и её output — в draft-report/static-verification
review package. Rustfmt доказывает parsing/formatting, не typecheck/Windows link.
Production RED→GREEN/TDD claim здесь неприменим: это characterization draft,
native result ещё не существует. Нельзя записывать task complete/native pass.

После fresh static review controller может push exact draft ref и один раз
вызвать `ci.yml` с `expected_revision=<reviewed40hex>`. Workflow выполняет:

```text
corepack enable
corepack install
pnpm install --frozen-lockfile
pnpm run audit
pnpm --dir web build
pnpm --dir packages/client build
cargo test --locked --features cli-full --test windows_transport_probe --no-run --message-format=json
<exact compiler-artifact executable> --exact windows_transport_probe --nocapture --test-threads=1
```

Для интерпретации нужны raw artifacts exact SHA/build, все четыре immutable
candidate snapshots, controls/bytes/native statuses, actual worker/process
signaling, отсутствие forced kill и полные final verdicts. Частичное прохождение
не подтверждает остальные cases. Если candidate не прошёл, остановиться и
разобрать evidence; не увеличивать budgets/frame и не rerun ради TRUE.

Даже четыре успешных case разрешают только: «на записанном Windows build и этом
fixture получены такие sampled pending/native completion результаты». Они не
дают универсального времени cancellation, atomic pending-at-abort, гарантии
production ownership/retirement, graceful StopDaemon delivery, разрешения пропуска
flush или accepted bounded quarantine. Неизвестны compilation/link result,
видимость именно этого NPFS flush через flag, cancel/disconnect driver outcomes,
реальные resource-retirement/host-disposal outcomes и любой production E2E effect.

Task 155-static-draft: Ruling: ready for draft review only — native execution,
compile acceptance и завершение #155 не заявляются. Controller должен получить
fresh review перед любым remote execution и пересмотреть решение по raw results.
