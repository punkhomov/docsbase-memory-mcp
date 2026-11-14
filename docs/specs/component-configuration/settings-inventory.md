# Инвентарь настроек и исключений

CF0, 2026-11-09. Проверено по `f51368e27500598a1c66372d0d9506aab0165be9`
(main после #98). Это факты исходников и целевая классификация, **не список уже
поддерживаемых TOML-ключей**. Имена future settings выбираются owning tasks.
Открытый PR #99 меняет frontmatter metadata refresh; не включён в этот baseline.

Scope: **D** daemon-wide, **M** machine/bootstrap, **P** project, **Pr** provider/model.
Actions: **R** runtime/resource update, **E** eviction/drain, **I** instance recreation,
**O** enumeration/metadata reconciliation, **C** rechunk, **L** lexical rebuild,
**V** vector identity/backfill, **A** ANN rebuild, **Q** query-vector cache identity.
Точные downstream edges и условия переиспользования: [state-dependencies](state-dependencies.md).
`>0` — целевое базовое ограничение; v1 validation и zero-as-disable семантика
не меняются до owning migration. Настройки одинакового owner объединены в семейства,
но каждый перечисленный параметр разрешается явно, не скрытым constructor default.

## Индекс, metadata и выдача

| ID | Текущее значение / источник | Owner; scope | Validation / последствия |
|---|---|---|---|
| I01 | `Config::default`: max_file_size=1048576 B | index; P | >0; текущий `file_size + FRAME_OVERHEAD <= MAX_FRAME_BYTES`; O/C/L/V/A только для изменённого состава/текста |
| I02 | max_docs_per_project=20000 | index; P | >0; O; уменьшение не удаляет документы молча, plan показывает skips/completeness |
| I03 | ignores=[]; built-ins `node_modules,target,vendor,dist,build,.git`; скрытые файлы skip | walk; P | Валидные confined patterns; O; cache/root/symlink confinement остаётся invariant |
| I04 | auto_index=false | daemon registration; P | bool; R/admission, включение не является согласием на новую платную migration |
| I05 | MAX_IGNORE_FILE_SIZE=1MiB | walk; M ceiling/P working budget | >0; проект не повышает доверенный предел чтения; R/O |
| I06 | MAX_CHUNK_CHARS=1500; CHUNK_OVERLAP=150 Unicode chars | chunk; P | cap>0, 0<=overlap<cap; atomic table/fence exception остаётся; C/L/V/A |
| I07 | MAX_TOKEN_CHARS=40 | tokenizer; P | >0; L; не V без изменения chunk text |
| I08 | WRITER_HEAP_BYTES=20000000; `Index::writer` выводит threads из heap/CPU (при этом heap — 1) | lexical writer; D/P lease | Порог библиотеки на thread проверяется; R/I, не rebuild существующего artifact |
| I09 | boosts text1/title2/heading1.5/identifiers2.5 | lexical query; P | finite, nonnegative, хотя бы один >0; R, quality-gate при изменении defaults |
| I10 | LONG_CHUNK_PENALTY=0.5; threshold=cap+overlap | lexical query; P | 0<penalty<=1; threshold derived; R, C только от отдельного chunking change |
| I11 | RRF_K=60; два равных вклада | fusion; P | finite positive K/weights, ненулевая сумма; R, не V/A |
| I12 | MAX_HITS=1000; default search limit10 (CLI и tool parser) | search; D ceiling/P default | 0<=request limit<=resolved ceiling, allocations bounded; R |
| I13 | SEARCH_UX_OVERSCAN=4 | retrieval; P | >=1, checked/saturating arithmetic; R; полноту за пределом окна не обещает (#67/#68) |
| I68 | Lexical initial fetch=max(limit×4, limit+16), subsequent widening×4 (`search_reader`) | lexical collector; P | initial factor>=1, additive>=0, widening>1; checked/saturating growth, unchanged settled/exhausted stop condition; R/performance, не L/V/A и не гарантия scoped recall |
| I14 | dedupe=none; n_per_doc=2; MAX_CHUNKS_PER_DOC=10 | search; P | enum; request cap1..ceiling; R; новые настройки не исправляют #68 автоматически |
| I15 | SNIPPET_MAX_CHARS=240; default snippet=false, explain=false | response; P | >0 для размера, bool toggles; R, response bytes всё ещё bounded |
| I16 | NEIGHBOR_CAP=100, before/after=1; list_docs limit50/cursor=None | response; D ceiling/P default | nonnegative bounded paging; R |
| I17 | MAX_WARNINGS=100; MAX_SYNC_JOBS=50 | index diagnostics/jobs; D | >0; R/E; retention не меняет исходные документы |
| I18 | SQL BATCH_KEYS=400 | store; D | >0 и bind-count ниже connection limit с учётом параметров на key; R |
| I19 | SQLite metadata write-open busy_timeout30s; read-only наследует rusqlite5000ms; WAL retries3 ×100ms | store; D/M direct | bounded attempts/durations; R/I для новых connections, не schema migration |
| I20 | vector DB busy_timeout5s | vector store; D/M direct | bounded duration; R/I |
| I21 | index removal retries20 ×100ms | lexical lifecycle; M | attempts>0, delay>=0; R; не обход reader leases |
| I22 | `cli/status.rs` PAGE=500 | CLI paging; M | >0 и <=server page ceiling; R |

Источники: [config](../../../src/config/mod.rs), [walk](../../../src/index/walk.rs),
[chunk](../../../src/index/chunk.rs), [job](../../../src/index/job.rs),
[tokenizer](../../../src/index/tokenizer.rs), [lexical](../../../src/index/tantivy_index.rs),
[tools](../../../src/daemon/tools.rs), [repo](../../../src/store/repo.rs),
[store](../../../src/store/mod.rs), [CLI](../../../src/cli/mod.rs).
Literal retrieval defaults в `parse_search_args` и CLI — один будущий owner,
не две независимо настраиваемые копии. I68 — внутреннее widening lexical collector
для long-chunk penalty, отдельное от post-filter UX overscan I13. Перенос параметров
сохраняет нынешний settled/exhausted критерий; их tuning не исправляет #67/#68.

## Dense, ANN и providers

| ID | Текущее значение / источник | Owner; scope | Validation / последствия |
|---|---|---|---|
| I23 | hybrid=false; table enabled=true; ranking=lexical | hybrid stages; P | bool/enum, RRF требует embedding; I/R |
| I24 | OpenRouter embed `nvidia/nemotron-3-embed-1b:free`, dimensions2048, normalize=true | provider stage; P/Pr | provider/model-supported dim; V/Q/A |
| I25 | OpenRouter rerank `nvidia/llama-nemotron-rerank-vl-1b-v2:free`; window50, min10/max100 | rerank; P/Pr | enum/model/window within qualified domain; I/R, не document V |
| I26 | providers.enabled=true, отдельные runtime_path/model choices | provider registry; M/Pr | feature/resource admission; I; paths не являются vector recipe |
| I27 | CacheLimits.max_entries=1000000, max_bytes=1GiB; `[hybrid.cache]` задаёт лишь bytes | vector store; legacy P policy на общем DB, target D | >0; R/E, не V; v2 ownership только explicit migration |
| I28 | DenseCacheLimits: max_vectors1000000, resident1536MiB, max_projects8 | dense cache; D + P threshold | >0, byte accounting всех живых builds/entries; R/E/drain |
| I29 | min_vectors4096; build_backoff30s | ANN admission; D/P policy | min>0, backoff>=0; R/E/admit, не смена содержимого graph |
| I30 | QUERY_CACHE_MAX_ENTRIES256, QUERY_CACHE_MAX_BYTES4MiB | query cache; D | >0; R/E; identity не меняется |
| I31 | max_ann_disk_bytes4GiB; ANN_KEEP_GENERATIONS2 | ANN storage; D/P retention | >=0 с explicit zero semantics; retention>=1, учитывать leases; E |
| I32 | M16; LAYERS16; ef_construction200 (M дублируется в ann_store metadata) | ANN construction; P | library M2..=256/layers1..=16 плюс qualified memory-estimator domain; A, не V |
| I33 | ef_search base64, maximum4096, multiplier4 | ANN search; P | >0, base<=max, checked beam>=fetch where supported; R, не A |
| I34 | MAX_ANN_FILE_BYTES2GiB | ANN storage reader; D | >0, bounded decode/load; R/admission, не считать существующий большой файл corrupt только из-за меньшего working budget |
| I35 | BACKFILL_PIN_TTL3600s; ANN_OUTCOME_TTL3600s | jobs/status; D | >0; R/lease renewal, не удалять live pins при shrink |
| I36 | local max_length512; batch_size64 | local provider; Pr | >0, model bounds; length/truncation→V/Q/A, batching→R/I |
| I37 | intra_threads=min(4, available CPU), CPU fallback1 | local provider; M/Pr | explicit resolved positive thread count; I, не V без доказанного semantic change |
| I38 | EMBED_BATCH_TEXTS128, EMBED_BATCH_BYTES512KiB | remote batching; Pr | >0; R; текущий oversized-single-text exception не менять молча |
| I39 | HTTP attempt timeout30s; fallback/ambient runtime ownership | OpenRouter transport; Pr/D | >0, attempt<=remaining deadline в #81; I/R; runtime lifetime отдельно от timeout |
| I40 | attempts4, initial250ms, ×2, max4s, Retry-After cap30s, jitter[0,50)ms | remote retry; Pr | attempts>=1, delays>=0, base<=max, no new attempt after deadline; R/I |

Источники: [hybrid config](../../../src/hybrid/config.rs),
[dense](../../../src/hybrid/dense.rs), [ANN](../../../src/hybrid/ann.rs),
[ANN storage](../../../src/hybrid/ann_store.rs), [vector cache](../../../src/vector_cache.rs),
[local](../../../src/hybrid/local.rs), [wire](../../../src/hybrid/wire.rs),
[OpenRouter](../../../src/hybrid/openrouter.rs), [jobs](../../../src/hybrid/jobs.rs),
[outcomes](../../../src/hybrid/meta.rs).

Важные связи: `jobs::ann_byte_cap` повторно берёт DenseCacheLimits::default;
`CacheCfg::limits` дополняет только bytes остальными defaults; `route_search`
добавляет default window при отсутствии reranker. Все три — будущие явные dependencies.
`ModelCacheKey` содержит max_length, а persisted model ID сейчас его не содержит:
пока длина фиксирована это не новый config defect, но её настройка требует recipe ID.
Устройство shared vector DB не позволяет объявить I27 готовой project quota.

## Watcher, lifecycle, IPC и bootstrap

| ID | Текущее значение / источник | Owner; scope | Validation / последствия |
|---|---|---|---|
| I41 | watcher debounce1500ms, max2000ms | watcher; P | 0<=debounce<=max; R; no source reindex только от debounce |
| I42 | event/batch queues4096; transient attempts3/backoff100ms; lease retry250ms | watcher; D/P worker | bounded capacities>0, bounded retries; R/I/drain, не drop pending batch молча |
| I43 | expand max_files10000, depth32 | watcher traversal; P | >0; R/O; явно сообщать incomplete traversal |
| I44 | watcher enable/disable — сейчас настройки нет, watcher стартует для indexed projects | daemon/watch; P | bool; stop/drain/restart worker; explicit sync сохраняется (#79) |
| I45 | start timeout10s, grace5000ms, stop timeout15s, no-listener timeout2s | lifecycle; D/M caller | bounded durations; R/I, не index invalidation |
| I46 | MAX_CONNECTIONS64, MAX_EVENTS1024, MAX_SESSIONS256 | daemon/session; D | bounded >0, active/pending ownership раздельно; R/drain |
| I47 | lifecycle poll50ms, janitor500ms, server drain5s, drain poll20ms | lifecycle; D | bounded durations; R; live blocking work учитывается до finish |
| I48 | CLI install wait600s, poll100ms | installer; M | bounded durations; R; не оправдание force-delete active lock |
| I49 | IPC IO_TIMEOUT2s; MCP quick30s/long600s | IPC/MCP caller; M/D | единицы/deadlines явные; Windows BlockingStream setters сейчас no-op, требуется capability/adapter test |
| I50 | config-write lock50 attempts ×20ms | config writers; M | attempts>0, wait>=0; R, lock identity/never-unlink invariant |
| I51 | model exclusive-lock50 attempts ×20ms | provisioning; M | attempts>0, wait>=0; R |
| I52 | MAX_STATE_BYTES1MiB для config/state/manifest | bootstrap/readers; M/D | разнести working budgets по типам; config read budget разрешается до чтения файла, через trusted bootstrap |
| I53 | MAX_LOG_BYTES8MiB, conflict/daemon log rotation | diagnostics; D/M | >0; R/E, не данные индекса |
| I54 | VCS META_READ_LIMIT4KiB, PACKED_REFS_LIMIT16MiB | registry/VCS; M/P working | >0, confined file reads; R/O |
| I55 | cache/config/data root env → OS dirs; daemon exe env override | bootstrap; M | canonical path/root admission; cache root change требует stopped daemon |
| I56 | ORT_DYLIB_PATH и provider runtime_path/data-dir discovery | runtime provisioning; M/Pr | порядок resolution сохраняется явно, provenance без secret; I |
| I57 | DOCSBASE_REPO → default `punkhomov/docsbase-memory-mcp` | updater; M | validated owner/repo; R, не index state |
| I58 | DOCSBASE_IPC_DEBUG / DOCSBASE_WATCH_DEBUG | diagnostics; D/P | bool explicit override, R; не fingerprint |

Источники: [watch](../../../src/watch/mod.rs), [lifecycle](../../../src/daemon/lifecycle.rs),
[server](../../../src/daemon/server.rs), [sessions](../../../src/daemon/session.rs),
[IPC client](../../../src/ipc/client.rs), [MCP](../../../src/mcp/tools.rs),
[paths](../../../src/config/paths.rs), [installer](../../../src/cli/install.rs),
[VCS](../../../src/vcs.rs), [limits](../../../src/limits.rs).

## Provisioning и неявные библиотечные настройки

| ID | Текущее значение / источник | Owner; scope | Validation / последствия |
|---|---|---|---|
| I59 | runtime archive256MiB/library512MiB/headroom256MiB; required=(archive+library)×3/2+headroom | provisioning; M | bounded bytes/overflow-safe ratio; R/admission, не V |
| I60 | model per-file2GiB/total3GiB/headroom256MiB; multiplier3/2 | provisioning; M | >0, file<=total, overflow-safe; R/admission |
| I61 | runtime download connect30s/response120s/body600s | provisioning; M/Pr | explicit positive durations; R/I |
| I62 | model download connect30s/response120s/body300s | provisioning; M/Pr | то же; R/I |
| I63 | Tokio1.53.1: workers=env/available CPU, max_blocking_threads512 (builder defaults) | executor; D | captured DefaultsContext + pinned dependency behavior; explicit settings до #79; не hardcode наблюдение512 как вечный API guarantee |
| I64 | HNSW/Rayon1.12+rayon-core1.13 global pool: env/CPU; tokenizers0.23.2 parallelism=true без override; FastEmbed7.1.1 API defaults | ANN/local; D/Pr | инвентаризировать именно defaults используемых pinned APIs, задать owned pools/explicit documented values до consumer migration |
| I65 | новый read pool сейчас отсутствует; design initial max_connections4 | store; D | >0; CF2 вводит перед #66, value не является измеренным capacity claim |
| I67 | local instance maps / model VERIFY_MEMO без отдельного size/TTL setting | local manager; D/Pr | закрытый registry ограничивает нынешние identities; перед hot reload расширенных settings нужны retained-handle accounting и bounded retention; R/E/I |
| I66 | search active/pending, provider in-flight, interactive/background priorities, deadline context сейчас не определены как settings | executor/providers; D/Pr | owning #66/#79/#81 задают и тестируют до ввода consumers; не притворяться, что MAX_CONNECTIONS уже решает это |

### Delta 2026-11-10 — #66 S3 owned payload policy

| ID | Новые defaults / источник | Owner; scope | Validation / последствия |
|---|---|---|---|
| I70 | search.max_request_payload_bytes64MiB | search::settings::SearchSpec; D | >0, <=aggregate; global-only TOML/defaults/provenance, byte admission; без vector/index invalidation |
| I71 | search.max_retained_payload_bytes256MiB | search::settings::SearchSpec; D | >0; shared RAII charge, checked sum; global restart по существующей policy; не RSS cap |

Это сознательно введённые runtime policies, не заимствование wire frame limit.
Они не вычисляются из read_pool capacity. S3.2 готовит owner/budget, реальные
SQL/pipeline consumers обязательны до whole-S3 delivery. Старый config JSON
остаётся legacy view; versioned full effective view принадлежит #78.

### Delta 2026-11-10 — S3.3 вычисляемый payload charge

| ID | Значение / источник | Почему fixed/computed |
|---|---|---|
| X15 | std::mem::size_of для owned payload/record headers; SQL octet_length direct columns; UTF-16→UTF-8 upper bound 3*ceil(bytes/2) | Размер Rust representation и Unicode code-unit bound — вычисляемые свойства формата, не memory policy. Лимиты I70/I71 настраиваемы; logical charge не равен всему RSS. BATCH_KEYS/MAX_HITS переиспользуют прежние bounded query domains |

Источники: [runtime](../../../src/hybrid/runtime.rs), [models](../../../src/hybrid/models.rs),
[local](../../../src/hybrid/local.rs), `daemon/lifecycle.rs::run_daemon`,
`hybrid/ann.rs::AnnIndex::build`. I63/I64 — видимый dependency-default debt,
а не обещание полного контроля внутренних эвристик каждой библиотеки.
Dependency env: `TOKIO_WORKER_THREADS`, `RAYON_NUM_THREADS` (затем deprecated
`RAYON_RS_NUM_CPUS`), `TOKENIZERS_PARALLELISM`; сейчас читаются зависимостями.
Будущий bootstrap захватывает их provenance/значения до старта workers; per-project
override для process-global tokenizer policy запрещён. Tantivy0.26.2 `Index::writer`
вычисляет num_threads из CPU и memory budget; переход на explicit writer options
сохраняет этот baseline и не приписывает приложению отсутствующий вызов API.
Это проверено в исходниках pinned crates, версии взяты из Cargo.lock.
 Переносятся
только используемые behavior knobs; недоступные внутренние стратегии фиксируются
версией dependency и документированным compatibility contract.

## Fixed / derived exceptions (FR-1)

| ID | Значения / источник | Почему фиксированы либо вычисляются |
|---|---|---|
| X01 | PROTOCOL_VERSION1; MAX_FRAME_BYTES8MiB; FRAME_OVERHEAD64KiB; TOOL_ALLOWLIST | Versioned v1 wire/security envelope. Рабочие budgets могут быть ниже cap; изменение hard cap требует protocol review. Текущий file-size guard не доказывает вместимость JSON-escaped текста; bounded oversize error сохраняется |
| X02 | DB SCHEMA_VERSION2, ANN_FORMAT_VERSION1, TOKENIZER_VERSION7; HNSW_RS_VERSION0.3.5 | Идентификаторы совместимости, не пользовательская настройка. Новый recipe/settings fingerprint дополняет их, не заменяет |
| X03 | f32=4 B, bit packing chunk/doc IDs, hash encodings, Unicode/Markdown grammar | Структура данных и математические/форматные инварианты. Config не меняет width типа или количество уровней heading в Markdown |
| X04 | ANN SEQUENTIAL_BUILD_MAX64; GRAPH_OVERHEAD_BYTES1024 | Первый — correctness workaround tiny parallel graphs; второй — conservative measured accounting model, не доверенная пользователю скидка. Для новых M/layers модель оценки пересматривается/проверяется, пока неподдерживаемая область отклоняется |
| X05 | HNSW max layers16/M bound256; Tantivy per-thread minimum heap | Ограничения pinned library; внутри них выбирается working policy. Не копировать только doc-comment: validation сверяется с реальным API версии |
| X06 | SUN_PATH_MAX104 macOS/108 Linux; OS permission modes/root confinement | OS ABI и границы доступа; настройка не может поднять OS cap или разрешить path escape |
| X07 | ORT_VERSION1.24.4, MACOS_MIN_MAJOR14, pinned registry revisions/checksums/licenses/prefix versions | Проверенные provenance/ABI/model semantics. Model choice настраивается, произвольная подмена registry metadata требует отдельного provisioning contract |
| X08 | OpenRouter allowed origin/path, HTTP-Referer/X-Title, provider IDs; GitHub update origins | Security/product identity, не эксплуатационный timeout. Добавление нового endpoint/provider проходит review allowlist, не обходит его произвольным URL в project config |
| X09 | `.docsbase.toml`, `.docsbaseignore`, DB/marker/manifest filenames, APP identifier, dump basename | Storage/discovery compatibility; задаются roots, а не произвольная перестановка внутренних имён без migration |
| X10 | Vec capacity hints по длине input, enum counts, status-unit labels; diagnostic syscall buffer growth | Детали реализации без самостоятельного поведения/полезного user control; не заменяют явные byte/work budgets. Если hint становится limit/retention — переводится в inventory |
| X11 | Тестовые fixtures/watchdogs/fault injection и CI latency slack | Test/development control plane, не product settings. Debug fault env остаётся явно помеченным unsupported test seam; не показывается как обычная настройка deployment |
| X12 | ratio/beam/peak bytes/thread defaults | Формулы от resolved values/OS facts, отображаются в effective output. Коэффициенты рабочей policy (I33/I37/I59/I60) настраиваемы; bytes-per-f32/checked arithmetic — нет |

Необоснованные исключения по принципу «редко меняют» не допускаются. Это каталог
проверенных семейств на указанном commit, не static proof отсутствия любого
неучтённого литерала: перед adoption owner повторяет source scan, классифицирует
новые knobs/dependency defaults и обновляет строки/исключения в том же PR.

## Delta 2026-11-10 — CF2 storage slice

I19: owner `store::settings::DbSpec` теперь передаёт complete typed policy в новые
Db constructors: write30s/read5s, WAL3 attempts ×100ms; все четыре значения могут
быть заменены typed patches. Производственные callsites мигрируются в CF2b;
TOML activation остаётся CF3/CF5, новые inactive keys не принимаются. Старые Db
constructors — compatibility path к одному resolver, не вторая копия defaults.

| ID | Значения / источник | Почему fixed |
|---|---|---|
| X13 | SQLite main header100B, magic, user_version offset60/schema_cookie40, page-size encoding1=65536, legal512..65536 powers-of-two, payload fractions64/32/32; `sqlite-schema-v1` identity prefix | SQLite file-format invariants и versioned observation identity. Это не work/memory budget: inspector читает ровно header, не всю DB. WAL/SHM/journal filenames — storage compatibility X09. Их наличие не даёт trust main-only identity |

## Delta 2026-11-10 — #66 S1 reader pool

`read_pool.max_connections` (owner ReadPoolSpec, daemon scope, count >0, default4)
теперь ограничивает startup prepared readonly connections. Typed overrides и
provenance проходят CLI/lifecycle/Shared; file keys по-прежнему отвергаются.
Pool acquisition не имеет hidden constant: caller передаёт абсолютный deadline.
Read handles используют I19 explicit read_busy_timeout; dispatcher adoption и
request deadline policy остаются следующими task #66.

## Delta 2026-11-10 — S2a wake protocol invariant

X14: a singleflight participant has at most one pending wake notification.
The channel capacity1 coalesces completion/cancellation wakeups; published state
is authoritative and is checked after waking. This is message-protocol cardinality,
not a configurable request/work/memory budget. Active subscriptions/flights retire
on drop/completion; no persistent failed-flight cache or hidden deadline is added.

## Delta 2026-11-10 — #66 S3.6a request deadline owner

`search.request_timeout_ms` adopts the deadline-context portion of I66 under
`search::settings::SearchSpec` (daemon scope, Milliseconds). Default30000 matches
the existing MCP quick-call budget for search. Positive u64, global-only config
and typed provenance; construction checks platform Instant overflow. The initial
value has exactly one owner. No per-stage/reconnect reset, index/vector
invalidation or new provider timeout. Active/pending/provider scheduling in I66
remains future work. Global changes require restart under the existing policy;
S3.6a primitive alone does not activate transport cancellation.


## Delta 2026-11-11 — #87 explicit request query semantics

X01 additionally covers `QueryMode::{Syntax,Literal}` wire spellings and the
Syntax compatibility default, plus the fixed `literal-query-v1` capability.
These identify versioned per-request interpretation, not cache/time/work policy.
`index::QueryMode` owns parsing/default initialization; consumers receive the
typed value. Omitted/null selects the owner default at boundaries; old index/
CLI APIs explicitly preserve Syntax as a compatibility contract. No deployment
setting, hidden component fallback, persistent state or invalidation is added.


## Delta 2026-11-11 — #68 request refill policy

| ID | Значения / источник | Owner; scope | Validation / последствия |
|---|---|---|---|
| I72 | search.dedupe_max_candidates=1000 | search::settings::SearchSpec; D | 1..=MAX_HITS; effective=max(base,configured); count/provenance/global-only; R, restart; no L/V/Q/A invalidation |

I13 factor4 используется также для bounded refill growth; новая скрытая
константа не добавляется. I12 MAX_HITS остаётся прежним compatibility ceiling.
Candidate budget — не exhaustive recall/RSS/time claim; payload admission и
original deadline продолжают действовать. RRF+cap один раз получает dense до
effective ceiling, без повторного vector scan при lexical widening.

## Delta 2026-11-13 — #79 S1a: daemon owner и bounded launch capture

`daemon::settings::DaemonSpec` определяет один каталог defaults, complete
`DaemonSettings`, global-only `DaemonPatch`, units/provenance, structural/count/
duration validation и pure runtime assessment. `ConfigSpec` только компонует
owner. `[daemon]` структурно запрещён в project files; v1/v2 source preview и
source migration сохраняют глобальные значения/provenance без runtime activation.
Все принятые изменения требуют restart; live executor unsupported, persisted
artifacts не инвалидируются. Compatibility constants ссылаются на owner catalog.

| Inventory | S1a adoption / consumer / units | Что ещё не завершено |
|---|---|---|
| I44 | `daemon.profile`: complete tagged Shared/Website, Name, D; whole-profile CLI patch, provenance default/global/CLI | В S1a только resolution/view. Website startup явно отклоняется до resources; S1b добавляет fixed-project/lexical/watcher-off/no-idle invariants и только затем снимает отказ. Отдельного watcher bool нет |
| I45 | `daemon.idle_grace_ms=5000`, Milliseconds, D; lifecycle получает complete policy. Omitted CLI grace сохраняет file/default provenance; explicit Shared0 допустим | START10s/STOP15s/no-listener2s остаются прежними deferred caller/bootstrap policy |
| I46 | `daemon.max_connections=64`, `max_events=1024`, `max_sessions=256`, Count, D; реальные accept-loop Semaphore/event channel/SessionRegistry используют overrides | Положительные counts проверены против actual Tokio MAX_PERMITS/channel bound и checked session allocation; worker admission отдельно |
| I47 | `daemon.poll_interval_ms=50`, `janitor_interval_ms=500`, `reply_drain_ms=5000`, `drain_poll_ms=20`, Milliseconds, D; lifecycle/server timers используют complete policy | Coalesced nonblocking janitor и physical Activity completion относятся к S1c; S1a меняет policy forwarding, не orchestration |
| I63 | `daemon.max_blocking_threads=512`, Count, D; lifecycle явно задаёт pinned Tokio1.54.1 Builder capacity. Existing BootstrapContext даёт captured Tokio async-worker count: checked blocking+async sum до lease/resources, ровно этот count передаётся worker_threads без повторного env read | Async-worker env/CPU default/source переиспользованы из bootstrap resolver; новые file keys/whitelist/defaults не вводятся. Остальной bootstrap adoption deferred; Shared hybrid reserve не обещается |
| I66 subset | `daemon.search_active=4`, `search_pending=32`, `control_active=2`, `control_pending=16`, Count; `control_timeout_ms=2000`, Milliseconds, D; owner validation/provenance/views/complete forwarding | Физические consumers/zero-pending refusal/FIFO/cancellation подключаются в S1c. Эти accepted fields не могут ship до этого; provider/background orchestration — #81 |
| I52 subset / #152 | Существующий trusted `limits::MAX_STATE_BYTES=1MiB` теперь фактически ограничивает global/project reads, one launch capture и restart hashing через общий `config::capture::read_bounded`; actual consumption/retention <= checked budget+1 | Per-type configurable bootstrap budgets остаются deferred; cap не читается из ограничиваемого файла. Observer сохраняет собственные no-follow/regular/race/CF5 semantics |

Lexical executor sum проверяется checked arithmetic:
`search-active + control-active + fixed sync1 + nominal helper1 <= blocking capacity`.
Две единицы — closed-inventory
implementation invariants, не дополнительные settings и не разрешение untracked
work. Сам physical reserve и watcher/provider zero-classes требуют S1b/c и
повторного source scan перед единственным S1 merge. Connection fds, payload bytes
и ReadPool — независимые budgets; real-corpus capacity/RSS/latency не измерены.
**MODIFIED 2026-11-13:** прежняя platform audit предпосылка S1c заменена
уточнённой application ownership границей в delta «S1c application worker scope»
ниже; platform adapter работа независима и не блокирует эту поставку.

Launch parse и baseline diagnostic digest используют одни captured bytes, без
повторного startup file read. Missing/Loaded/Unavailable различаются; текущий
Unavailable всегда даёт notice. Это runtime/diagnostic state, не CF5 precondition,
generation identity или publication edge. Malformed-global legacy warning/default
adapter сохранён; strict launch отклоняет ошибки до cache/admission/listener writes.

Остальные I41..I58 subsets остаются deferred: watcher debounce/queues/traversal,
installer waits, IPC/MCP caller timeouts, config/model lock retries, per-type
bootstrap/log/VCS budgets, root/runtime discovery, updater/debug controls.
S1a не означает full CF12 adoption или завершение #79. Source scan:
`rg -n 'const |DEFAULT_|Semaphore::new|mpsc::channel|spawn_blocking|block_in_place|tokio::fs|thread::spawn|thread::Builder' src/daemon src/platform src/hybrid src/watch src/web src/mcp`.

S1a review round1: new daemon-owner errors и любой error после распознанного
Website fail closed до legacy fallback/resources. Старые malformed Shared sources
сохраняют warning/default/diagnostic adapter. Applicability/provenance marker не
входит в effective runtime identity/impact; omitted и explicit Shared5000 дают
одну effective policy. Reply-drain sleep ограничен remaining original deadline,
даже если drain-poll больше timeout. Это уточнение существующих consumer contracts,
без новых settings, artifact edges или S1c physical-drain adoption.


## Delta 2026-11-13 — #79 S1b fixed lexical activation

I44 is now consumed by canonical existing-indexed-project admission, complete CLI
profile overlays, immutable watcher ownership, reader/session/tool scope and the
lifecycle idle-deadline decision. The S1a temporary startup refusal is removed
only with these gates. No independent watcher toggle or idle default is added.
I47 retains Shared grace including explicit0; Website cannot arm an idle timer.
Profile and all daemon operational policy remain restart-immutable; no index,
vector, ANN, generation or CF5 publication edge is introduced by profile change.

The additive `active-website-profile-v1` Hello marker reports the active Website
profile, not a capability to enable it. It is a wire identity, not a configurable
operational limit. Shared Hello and existing required SDK capabilities remain.
Pre-admission SQLite reads share the real readonly/schema/WAL implementation
with diagnostics disabled only for the narrow crate-internal admission entry;
ordinary readonly schema-conflict diagnostics retain their existing behavior.

Website composes lexical stages and disabled ANN policy without provider
construction, vector-store opens, ANN refresh/backfill or watcher producers.
Existing DenseCache/SearchOperation/generation/singleflight and sync-job dedup/
ProjectLease mechanisms remain. I46/I48..I51 physical active/pending/control
admission and coalesced janitor remain S1c; accepted unwired settings cannot
ship before that task and integrated final review. **MODIFIED 2026-11-13:**
platform adapter ownership is independent, outside S1c (see scope delta below).


## Delta 2026-11-13 — MODIFIED: S1c application worker scope

The I63/I66 sum and closed inventory describe application-owned Tokio blocking
work only. They do not bound dependency-owned/native process resources or prove
IPC retirement/control availability. Prior text making platform adapter work an
S1c prerequisite is superseded; that independent work is parked outside S1. The
functional worker change keeps explicit B, validated S/C active/pending counts,
original search deadlines, same-state cancellation and closure-held ownership.
No new configurable defaults or artifact invalidation edges are introduced.


### Delta 2026-11-13 — S1c actual consumers

I66 search/control active+pending counts now construct the two fair Semaphore
lanes in WorkerAdmission from complete DaemonSettings; zero pending keeps
immediate admission when active is free. control_timeout_ms sets the absolute
control admission deadline; search uses its existing SearchOperation deadline
and same-state cancellation. Bind/register and metadata/status/panel share C.
I47 janitor ticks install at most one tracked admission/completion future, polled
beside accept/Stop/signals; timer branches never await the blocking work.

I63 keeps the explicit Tokio B and checked application inventory sum. The sole
production daemon spawn_blocking call is request::spawn: permit and Activity
move into execution through finish/unwind. Website sync uses its fixed singleton
and parent Activity; watcher/backfill/ANN/provider classes stay disabled there.
Legacy Shared background sync can use the helper without an application lane
permit; its background/provider orchestration remains outside this reserve proof.
Connection/session slots, ReadPool handles and payload bytes stay independent.
All adopted policy changes require restart; no index/vector/ANN invalidation.

### Delta 2026-11-13 — S2/S3 Website consumers and application boundary

S2 adds no daemon runtime default: existing control/ReadPool/project read admission
observes two fresh bounded metadata snapshots around one read-only index check.
The existing coordination file is opened without creation. A's actual full-survey
outcome/recipe and durable Sync attribution govern readiness; no generation,
writer/publication protocol, provider or watcher mechanism is introduced.

The Node example's `config.ts` owns a complete explicit HTTP policy, resolved once:

| Policy group | Consumer / units | Effect |
| --- | --- | --- |
| host/port/maxConnections/maxRequestsPerSocket | Node listener/socket admission; address/count | application restart; no artifact invalidation |
| search/control active+pending | independent FIFO HTTP lanes; count, pending0 valid | queue before body retention; slot held through actual completion |
| header byte/count, body/query/result bytes, maxHits | parser/upload/JSON/response/search boundary | rejection before unbounded retention; maxHits owns HTTP default/cap |
| request/header/check-interval/idle/keep-alive times | Node/request-operation timers; milliseconds | finite original deadline, no new stage budget |
| complete SDK client options | existing SDK owner / immutable project and native endpoint | SDK validates once without I/O; no consumer fallback |

Node timer ranges, header-pair count+one overflow detection and keep-alive buffer0
are API/implementation invariants, not additional hidden operational defaults.
`CallOptions.budget` is optional caller-owned monotonic start/deadline data; it can
only narrow the existing SDK operation. Connection/session/worker/reader/byte caps
stay independent. No new external runtime dependency is added. Full CF12/CF13,
remote async #81 and process-wide/native-resource guarantees remain separate.


## MODIFIED 2026-11-14 — #81 A1 remote execution owner

RemoteSpec (`hybrid/remote_settings.rs`) owns the global-only `[remote]` typed
policy: interactive_active4/pending32, attempt_timeout_ms30000, retry_attempts4,
retry_initial_ms250/max4000/multiplier2, max_retry_after_ms30000/jitter_max_ms50,
embed_batch_texts128/bytes524288 and max_response_bytes33554432. Count/byte/ms
units, defaults, provenance, validation, capability projection and runtime-only
assessment are owned there; ConfigSpec composes without copying its rules.

I38–I40 use those values in the new real async provider/transport/retry/batch/body
path, including original/hard deadline minimum and independent live-interest
wait. DaemonSpec adds helper_active1; helper pending is derived interactive_active
and physically enforced by WorkerAdmission before custom DNS spawn. CPU/DNS
closures retain the existing Activity and physical permit through finish/unwind.
Reqwest owns no Runtime. Explicit root-retired Handle bridge preserves sync
compatibility and never crosses an await. All timeout/retry/budget changes require
restart and do not alter persisted vector/query/model/ANN identities.

Recognized remote owner errors are fatal before effects in the actual legacy
Shared launch; unrelated legacy fallback remains. Project remote override is
structurally rejected. Effective/source/v1 roundtrip and connect read-modify-write
preserve values/origins. A1 is an internal functional task: new library provider
consumers run, server phase dispatch lands with A2 in the same first PR; existing
background/direct sync-over-async remains explicitly legacy until PR2. No settings
are published in a standalone scaffold/docs PR. Full CF13/provisioning/v2 remains
deferred. Locked Tokio1.54.1, reqwest0.12.28 and hyper-util0.1.21 were inspected;
no process-wide native resource or capacity claim follows from the Tokio bound.
