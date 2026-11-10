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
