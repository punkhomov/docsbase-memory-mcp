# Progress — component-configuration / issue #78

Plan: [tasks.md](tasks.md). Requirements: [requirements.md](requirements.md).

## 2026-11-09 — CF0: архитектурный baseline

- Source baseline: `f51368e27500598a1c66372d0d9506aab0165be9`, main после #98/#76.
  Открытый соседний PR #99 относится к frontmatter; runtime этого этапа не меняется.
- Созданы requirements/design, inventory I01–I68 и exceptions X01–X12,
  dependency graph/invalidation matrix, phased adoption и FR coverage.
- Ruling: этот PR — только CF0/SC-1/SC-2; #78 остаётся открытым до runtime adoption,
  plan/apply/recovery и final audit. Следующие code PR — CF1, CF2, затем #66.
- Ruling: typed composition выбрана вместо type-erased registry/codegen;
  центральный wiring знает типы секций, semantic rules остаются у owners.
- Ruling: v1 whole-value hybrid merge и legacy vector-store mode сохраняются;
  v2 field merge/strict owner требуют explicit migration. Не смешивать два evictor
  режима одной vectors.db. Цена — временный legacy adapter, не новая тихая семантика.
- Ruling: old config/stages snapshot #76 не доказывает cross-storage snapshot.
  CF10 storage ADR обязателен до CF11; минимальный #66 не ждёт полной #78.
- Ruling: database read-only planning не использует migrating Db::open; providers,
  downloads, eviction и state writes исключены из dry-run. Unsupported online
  inspection остаётся Unknown/blocked, не равенством active config и TOML.
- Ruling: library/process-global env и worker pools входят в ownership inventory;
  hot reload не мутирует process env. Windows timeout no-op не считается enforcement.
- Ruling (CF0 review fix): neutral ArtifactId/ArtifactFacts/ComponentImpact DTO
  объявляются CF1, planner algorithms — CF4. CF3 даёт pure MigrationDraft,
  read-only migration CLI/state plan — CF4, consent/write — CF5; цикл задач устранён.
- Ruling (CF0 review fix): pure DB inspector отделён от operational read-only helper,
  который пишет conflict log при schema mismatch. CF2/CF4 требуют filesystem-snapshot
  regressions, включая WAL/SHM/missing state; не считать SQLITE_OPEN_READ_ONLY гарантией purity.
- Ruling (CF0 review fix): owner catalog содержит disabled/enabled-shortcut/table-base
  templates; bool true включает обе default stages, false очищает обе. Auto на новом
  policy path разрешается по captured entrypoint/TTY, legacy v1 startup сохраняется.
- Ruling (CF0 review fix): добавлен отдельный lexical widening I68; read-write/read-only
  busy timeouts закреплены как30s/5s. Ссылки полной publication исправлены на CF10/CF11,
  CLI enum separators экранированы в Markdown table.
- Validation/review: исправления внесены; scoped re-review и финальная validation pending.
  Runtime requirements/SC-3–SC-5 не выполнены этим этапом.

- Task CF0: complete (design delivery, 2026-11-09). SC-1/SC-2: source-backed inventory
  68families/12exception groups, dependency matrix, typed contract, migration/policy
  decisions и phased tasks. Local audit:46relative links/24source anchors, balanced
  fences/table columns, docs-only scope; docs hygiene/6Python checks/fmt/diff GREEN.
  Fresh task review:6Important/3Minor documentation gaps fixed отдельным docs agent;
  scoped re-review PASS, все9ADDRESSED, новых Critical/Important нет. Deferred minors:none.
  Native CI/final review pending PR. Runtime FR/SC-3–SC-5 здесь не реализованы;
  #78 остаётся открытым, продолжение — CF1/CF2 и отдельный #66.

## 2026-11-09 — CF1: typed resolution foundation

- Добавлены `schema`, `bootstrap`, `resolve`, storage/dense owner specs и hybrid
  owner catalog. `Resolution` строит defaults один раз, stages patches atomically,
  сохраняет scopes/units/origins и выдаёт immutable validated `Resolved<T>`.
- Legacy Config file entrypoints теперь adapters к typed resolution. v1 bool/table
  replacement, provider-aware defaults, feature-off parsing и global-before-project
  validation сохраняются. Plain supplied snapshots явно `ProvidedSnapshot`.
- Ruling: `hybrid/config.rs` добавлен к planned Modify list для owner locality;
  common loader не хранит hybrid templates/default callbacks. `fields()` агрегирует
  descriptors owners; neutral impact DTO не выполняет планирование/inspection.
- Ruling: storage/dense/read-pool patches typed-only; inactive TOML keys rejected.
  Production runtime adoption и конфигурируемые budgets остаются CF2/CF6/#66.
  Dense legacy vector fallback не объявляется strict-v2 global quota; zero disk
  budget/backoff сохраняет legacy semantics. Новые settings не имеют Default.
- Ruling: explicit injected bootstrap сохраняет dependency fallback policies;
  Tokio malformed/zero становится redacted Admission. Никаких env writes,
  credential reads, home lookup в deterministic Config defaults/overrides или
  запуска providers/DB/models в resolver. CF13 runtime bootstrap ещё впереди.
- TDD: unresolved CF1 public API compile RED (`config_resolution`, exit101) до
  production modules; existing v1 assertions — characterization, не behavioral RED.
  Затем behavioral RED `oversized_db_busy_timeout_is_admission_before_rusqlite_can_panic`
  показал ошибочное принятие 2147483648ms. Owner bound check исправляет будущую
  rusqlite panic; zero timeout допустим. Focused/full/lean validation и fresh review
  будут зафиксированы после завершения проверок. #78 остаётся открытым.

- Task CF1: complete (local implementation, 2026-11-09; validated source
  `194cbb1fcda07d9e311557fa33beeb2c6e24d32a`). Новый API compile RED и behavioral
  busy-timeout RED зафиксированы до соответствующего GREEN. Все восемь именованных
  CF1 regressions присутствуют; всего19 новых tests. Focused cli-full:31 existing
  config +19 новых GREEN; lean:25 existing config +19 новых GREEN.
- Validation: `cargo test --locked --features cli-full` — exit0,908 selected tests
  passed/13ignored/0failed; вложенный self-test subprocess не прибавлен второй раз.
  `metrics_match_baseline` GREEN, baseline/snapshots не менялись. Full и lean
  `cargo clippy --locked [--features cli-full] --all-targets -- -D warnings` GREEN;
  `cargo fmt --check`, `git diff --check`,6 Python observer tests GREEN. Rust1.88,
  jobs4; isolated config/temp roots, существующий CI latency slack3.
- Native evidence (source194cbb1, coordinator-verified Actions artifacts): required
  docs/linux/macos/windows4/4GREEN; Linux908 selected +2 static artifact tests,
  macOS904, Windows893,0failed/13ignored. Inventory — exact parent superset +19 на
  каждой ОС; прежние tests не удалены. Fresh task/final review и delivery состояния
  ledger commit остаются pending; coordinator закрепит их в PR evidence.
- Ruling: это завершение CF1 foundation, не всех runtime FR-3/SC-3 и не #78.
  Db/dense consumers, read pool, env adoption, migration/planner/consent/recovery
  остаются CF2/CF6/#66/CF13 и последующим задачам; inactive file keys не добавлены.
  Deferred minors на момент local handoff: none; fresh review ещё не выполнен.

- CF1 task review: один Important I1 — public HybridSpec snapshot import принимал
  unknown providers и malformed provider-specific stage settings. Regression RED
  на source f133876: full imported tests3failed/1passed, lean3failed/0passed.
- I1 fix: только strict `HybridSpec::validate` дополнительно использует pure owner
  provider registry resolution с explicit model/dimensions/normalize/window.
  Никаких defaults reread, credentials/env/provisioning/model/runtime construction.
  Ruling: legacy `Config::validate` и private ConfigSpec сохраняют прежнюю structural
  compatibility; plain `Config::for_project` acceptance закреплена отдельным тестом.
  Новый public `Resolved<HybridCfg>` проверяет те же provider constraints, что patches.
- I1 fix GREEN: focused config/config_resolution/hybrid_config — full31/24/13,
  lean25/23/8; full/lean all-targets Clippy `-D warnings`, fmt/diff-check GREEN.
  Новые tests покрывают unknown embedding/rerank provider, fastembed normalization,
  dimensions/model/task mismatch, valid snapshots и прежнюю feature-off admission.
  Локальный полный suite908 относится к source194cbb1; после fix полный suite
  проверяется новым native CI на трёх ОС. Scoped re-review/final review и этот
  новый native proof pending; #78/runtime adoption scope не меняется.

- Final review Minor M1: addressed (2026-11-09). Design уточняет сохранённую v1
  feature policy: ungated provider records и OpenRouter scalar resolution доступны
  feature-off; fastembed stage resolution требует hybrid-local уже до construction.
  Ruling: это docs correction, Rust/тесты/поведение не меняются. Coordinator сообщил
  native4/4GREEN для source5c6818f; scoped final re-review и required checks нового
  docs HEAD ещё pending. Docs hygiene/diff-check GREEN; новые tests не нужны.

CF1 delivery reconciliation (2026-11-10): PR #102 merged as8fd4308; final source
c370fc5 had required docs/Linux/macOS/Windows4/4GREEN, 914/910/899 selected tests,
13ignored/0failed (Linux +2 static artifact). Task/final reviews completed before
merge; earlier pending handoff lines above are historical. CF1 runtime adoption
claims remain partial. Subsequent #103 fix PR #104 merged c1fe915; its final head
28b5c49 passed native4/4 with923/919/908, Linux+2artifact, parent inventory+9/no removed.

CF2 start (2026-11-10, basec1fe915): source Db::open/open_readonly inventory captured
before code. Production metadata opens: Shared initial/register/index/ANN worker,
execute_sync_job, watcher consumer, CLI index/status/read_project/uninstall peek.
Source tests/fixtures keep compatibility APIs until owning migration. CLI search
and lifecycle carry policy despite not directly opening DB themselves.
Ruling: no TOML activation in this bounded store adoption; explicit typed overrides
reach runtime, production bootstrap resolves owner defaults once. File/version/
startup policy activation remains CF3/CF5; no ignored public keys added. Pure
inspector reports bounded schema identity only, not a query snapshot. CF2a/CF2b
and dated requirements/design deltas make these boundaries reviewable before code.
CF2a RED evidence: новые explicit constructors отсутствуют на basec1fe915 (E0599).
После первых API overrides17/29ms и defaults30000/5000ms подтверждены реальным
PRAGMA. Inspector сначала не давал schema identity и ошибочно считал orphan WAL
Missing: обе deterministic regressions RED, затем GREEN. WAL policy RED на
provisional API с legacy3×100ms: waits [100,100] вместо [] при attempts1 и [100]
вместо [43] после controlled unlock. Теперь точные attempts/delay берутся из owner.
CF2a partial verify: store unit13/0 (включая7 новых), store_settings10/0; config,
config_resolution и legacy store focused GREEN. Snapshot cases включают committed
WAL после killed child + отсутствующий SHM; никакого восстановления sidecar или
main-only identity. Full/lean validation и свежий task review ещё pending.
CF2a full validation caught ADR-9 boundary violation: no-follow platform flags
находились в store/inspect.rs. Existing platform_boundary regression RED показала
OpenOptionsExt/unix/windows imports вне seam. Ruling: вынести функцию открытия
в platform::fs с Unix/Windows implementations, не ослаблять scanner/allowlist.
Это расширяет CF2a files до platform/{mod,unix,windows}.rs; inspector сохраняет
read-only/no-follow semantics и не содержит OS API.
Task CF2a: complete (2026-11-10, basec1fe915; local implementation gate).
Final Rust1.88 cli-full suite exit0:940passed/0failed/13ignored,64targets
(parent923 +17 новых, child helper summaries не удвоены). Lean unfiltered
integration:58/0 (config25/config_resolution23/store_settings10); separate store
unit filter13/0. Full/lean all-target Clippy `-D warnings`, full opt-in
perf_budget/artifact Clippy, fmt/diff-check GREEN. Platform boundary2/0 после
вынесения no-follow open в seam; scanner не менялся. No new crates/unsafe,
baseline/timing/config wire changes. Native proof — после всей CF2 в PR.
Ruling: CF2a завершает API/inspection, не production adoption. До CF2b старые
callers ещё идут через documented compatibility policy resolver. Fresh CF2a task
review pending; CF2b wiring/adapter audit и T55 M1 cleanup следуют после него.
CF2a task review FAIL/CHANGES (I1): Unix readonly no-follow open мог зависнуть
на FIFO до проверки regular-file type, включая replacement race. Scoped
implementer добавил O_NONBLOCK в platform seam и две subprocess regressions
(initial FIFO / DB→FIFO перед повторным наблюдением) с kill/wait/join cleanup.
RED: обе завершились только deadlock watchdog; GREEN inspector7/0, symlink guard
сохранён, FIFO inode/mode/length/mtime не меняются, fact Unknown/no identity.
Scoped re-review: I1 ADDRESSED, новых Critical/Important нет.
Task CF2a: fix validation complete (2026-11-10,99f098c + I1): full943/0/13,
64targets; full focused integrations83/0 + units16/0, lean integrations76/0 +
units16/0. Full/lean all-target Clippy + full opt-in lint, fmt/diff GREEN.
Все10 команд завершились exit0; child stdout не удваивает counters. Native CI
остаётся gate после CF2b. Test watchdog10s — deadlock containment X11, не SLA;
нет fixed sleeps или production budget changes. No new crates/unsafe/scanner changes.
CF2a consolidated task review: Spec compliance PASS / Quality APPROVED after I1,
no remaining findings. Source99f098c + reviewed fix09c7193; CF2b begins next.
CF2b Ruling: ANN refresh metadata work only reads repo generation/chunk hashes;
его child connection становится prepared readonly с read policy (default5s),
вместо прежнего writer-open30s/WAL negotiation. Override тест проверяет actual
PRAGMA и отказ записи через существующий private warm seam. Shared удерживает
immutable policy вне глобального DB mutex; это не реализация pool/snapshot #66.
Task CF2b: complete (2026-11-10, base09c7193; local implementation gate).
CLI composition разрешает policy один раз; direct index/search/list/status/sync
fallback/uninstall/serve передают её явно. Lifecycle проверяет imported policy до
admission/state и передаёт её Shared. Shared хранит immutable policy вне Db mutex;
registration/index/sync/watchers получают writer policy, ANN metadata — prepared
readonly с read policy. Actual overrides writer17ms/reader29ms и запрет записи
через ANN reader GREEN. Invalid policy не создаёт admission/state или watcher.
Source audit:10 explicit metadata opens,0 production calls legacy Db/Shared/job/
CLI wrappers; standalone resolution только composition/compatibility boundaries.
T55 Minor M1 resolved by scoped implementer при fixture migration: Db объявлен
раньше cache/root TempDirs, named initializer сохранён; full/lean watcher10/10.
Final verify Rust1.88: cli-full947passed/0failed/13ignored,64targets; lean
integration43/0 (5targets), server9/0, lifecycle1/0; full/lean all-target Clippy,
full opt-in perf/artifact lint, fmt/diff GREEN. Source freezes соблюдены; TMPDIR
/var/tmp и прежний shared-host slack3, без budget/snapshot/dependency changes.
Ruling: первый targeted server run ошибочно использовал /tmp с cloud .git marker
и получил fixture root0-docs; он не засчитан. Корректный /var/tmp run server9/0 и
финальный полный suite подтверждают реализацию. Fresh CF2b task review pending;
final whole-branch review и native required CI остаются delivery gates.
