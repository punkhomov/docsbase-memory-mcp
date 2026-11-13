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
CF2b fresh task review: Spec compliance PASS / Quality APPROVED; findings none.
Reviewer проверил actual source всех10 opens/12 resolution sites, source hashes,
64 full targets и lean counters без повторного прогона тестов. T55 M1 закрыт.
Final whole-branch review и native final-head required CI следуют перед merge.
CF2 final whole-branch review (2026-11-10): converged for scoped CF2,
no unmet scoped requirements, Critical/Important/Minor none. Actual policy flows,
all12 resolver sites/10 metadata opens, taskA/B evidence and product hashes checked
independently; no tests rerun by reviewers. File activation/read pool/whole-generation
publication remain explicitly future scope, so #78/#66 are not closed by this PR.
Final product equals validated74de58d; this commit adds factual ledger only.
Delivery gate: latest-head docs/linux/macos/windows must all succeed; native artifact
inventories and pinned squash SHA are recorded in the PR before/after merge.

CF3 implementation/local validation (2026-11-11, base0b2b5df): pure versioned
SourceLayer/preview и MigrationDraft без runtime activation. v1 replacement,
v2 nested inheritance/disable/reset, mixed scopes/provenance и explicit budget
survey реализованы; proposed TOML roundtrip проверяет non-resource equivalence.
No-write проверяется реальными source files + полный snapshot файлов/directories.
Ruling: global-v2 reset не возвращает legacy default в suggested budget;
explicit v1 project override остаётся отдельным conflict/budget observation.
RED поймал увеличение4096→1073741824 и утечку значения provider в ошибке preview;
исправления подтверждены GREEN. Обычный v1 loader/diagnostics не изменены.
Rust1.88: cli-full1230passed/0failed/18existing ignored (72targets включая doc),
lean config/config_resolution/config_migration/config_runtime/hybrid_config82/0/0.
Новые12 regressions выполняются в обоих builds. Full/lean all-target Clippy,
fmt/diff GREEN. Full suite включает настоящие CLI compatibility tests и frozen
search gate; baseline/recordings/dependencies/CI/daemon implementation не менялись.
No inference/provider requests; TMPDIR=/var/tmp, existing host slack3.
Fresh task/final review и final-head native CI остаются delivery gates.
CF3 fresh task review: FAIL/CHANGES, Important I1 — strict-v2 preview сохранял
project scope legacy budget key. Исправление: owner descriptor vector_cache.max_bytes
со Scope::Daemon/Unit::Bytes, provenance rebind после layering; v1 key/scope
сохраняются. Regression RED подтвердил отсутствие v2 identity до исправления.
Scope expansion: config/schema.rs (generic provenance projection) и vector_cache.rs
(только owner descriptor, без изменения store/runtime).
I1 fix validation: full config/migration/resolution68/0, lean61/0; новые13 migration
regressions. Full/lean all-target Clippy и fmt/diff GREEN. Полный локальный1230 run
предшествует изолированному I1 fix; native final-head suite остаётся обязательным.
Task CF3: complete (2026-11-11, implementation + fresh task review).
I1 re-review: ADDRESSED; new Critical/Important none. Все CF3-1…CF3-5 выполнены
в source-only scope; deferred minors нет. Fresh whole-branch final review и
required docs/Linux/macOS/Windows CI остаются delivery gates. #78 не закрывается.

CF3 delivery (2026-11-11): PR127 merged as543225be973f9faadcfc17f00b2cfba3be71a027.
Fresh final review converged, no findings. Native run43082250608 attempt1:
docs/linux/macos/windows GREEN; Linux1231 +2static, macOS1227, Windows1219,
0failures/18existing ignores. Каждый parent test сохранён,13 migration tests
выполнены на каждой ОС. Squash tree99fd7f92f74756c1618dd9aea0d3965fae317747
совпадает с reviewed/tested tree. Доказательства: PR127 comments и issue78.

CF4a implementation started (2026-11-12): working source effective view, отдельно
от будущего state planner. Dated requirements CF4a-1…3/plan. RED: отсутствующие
view API/provider safe projection. Initial GREEN:6real integration tests, включая
CLI/bare-config compatibility, no-write snapshots, unprovisioned models и error
redaction. Owner values/provenance checked; existing provider registry reused.
Проверка полного suite/lean/lint и свежие reviews ещё не завершены.
CF4a local verification: Rust1.88 full cli-full1239/0/18existing ignores,
73targets включая doc; lean config/view/migration/resolution/hybrid/cli-routing81/0
и owner-view units2/0. Full/lean all-target Clippy, fmt/diff GREEN. Единственный
lint fix: private CLI run принимает ссылку вместо ненужного владения enum; после
него actual CLI view integrations6/0 повторно GREEN. Full suite был до этого
behavior-neutral signature change; final-head native suite остаётся gate.
No new crates, baseline/threshold/snapshot changes, provider calls или model
provisioning. Views используют actual resolver и existing provider registry.
Task CF4a: complete (2026-11-12, local implementation + fresh task review).
Spec compliance PASS / Quality APPROVED, Critical/Important none. Minor M1 fixed:
plan использует точное wire имя provisioning_checked=false, как code/tests/docs.
Deferred minors нет. Final whole-branch review и native final-head CI следуют.

CF4a delivery (2026-11-12): PR131 merged as
f70a6f80e0d1a3f6cae654d6e9a8e5a34eb47276. Fresh final review converged; native
run43221452372 attempt1 all four required checks green. Linux1239+2static,
macOS1235, Windows1227; zero failures,18existing ignores/platform. Parent
inventory retained,8new tests executed/platform. Reviewed source10f4170 and
merge7c23a94 share squash tree509b9839774dcf86e47e571da49398edc0a34890.

CF4b implementation started (2026-11-12): pure bounded graph + actual Search/Db
and MigrationDraft adapters. RED missing APIs; separate behavioral RED showed
public SourceEdit could bypass source byte contract; now checked before hashing.
Initial GREEN10 real/graph integration tests. No runtime/CLI behavior changed;
all proposed actions explicitly lack executor. Full validation/reviews pending.

CF4b local verification: Rust1.88 cli-full1249passed/0failed/18existing ignores,
74targets including doc; lean plan/migration/resolution/store56passed/0failed.
Full/lean all-target Clippy and fmt/diff checks GREEN. Aggregate excludes nested
child-process lock-holder summary (its parent unit test is counted once).
Task/final independent reviews and final-head native CI remain delivery gates.
CF4b task review: initial CHANGES — Important path equality/serialization mismatch,
Minor exact input-budget boundaries and independent malformed-draft tests. Fixes:
component-based source encoding on every resource reference, raw path byte cap,
permutation/dependency/Windows-prefix coverage, isolated version/cap checks.
Behavioral RED→GREEN for alias identity and canonical-size bypass. Re-review:
all three findings ADDRESSED; no new Critical/Important. Deferred minors none.

Task CF4b: complete (2026-11-12, local implementation + fresh task review).
Ruling: initial Important and two Minor findings addressed; re-review no new
Critical/Important, no deferred minors. Final local full cli-full1252/0/18existing
ignores,74targets; lean59/0; full/lean all-target Clippy and fmt/diff GREEN.
13plan integration tests on Linux; additional Windows prefix test requires native
Windows. Final whole-branch review and native final-head checks remain gates.

CF4b delivery (2026-11-12): PR132 merged as d2d903780466eafe9f14ae1b78b7891213d1cd24.
Fresh final review converged, no findings. Native run43227848982 attempt1: all
four required checks green; Linux1252+2static/macOS1248/Windows1241, zero failures,
18existing ignores. Parent inventory retained;13new tests/platform plus Windows
prefix case. Reviewed sourcef314efc and tested mergef064fb4 share squash tree
 d6ffddcf3e68d38b2871cb300332a05c6669ea94.

CF4c1 implementation started (2026-11-12): actual bounded quiescent registry
observer. RED absent API; causal RED removing native lease allows competing write.
URI double-slash RED→GREEN; external child stdout BrokenPipe fixed in harness.
Additional causal RED: raw header preflight releases existing rollback-reader OS
locks; observer now uses metadata-only preflight and native SQLite descriptor.
Regression passes with external writer blocked before/after unavailable observation.
Targeted integration13/0 and leased units7/0 (Linux); final validation/reviews and
native namespace checks remain pending. Existing raw header inspector defect (#133) is
separate from this new API and must be tracked rather than hidden by this change.

CF4c1 local full verification: Rust1.88 cli-full1272passed/0failed/18existing
ignores across75targets including doc (nested child summaries excluded). Clippy
found identical match arms in metadata preflight; merged equivalent arms, no
behavior change. Lean/final lint verification follows; required native CI will
run the complete suite on the final reviewed tree. No baseline/threshold changes.
CF4c1 remaining local checks: lean integration36/0, leased units7/0;
full/lean all-target Clippy, fmt and diff checks green. Fresh reviews pending.

CF4c1 task review: Important macOS auto-locking backend (unix name alone does not
prove POSIX) and Minor oversized persisted-value coverage. Fix: concrete methods
checked before xLock; actual native unix-none/dotfile RED→GREEN, no file writes.
Additional URI/path audit found SQLite NOFOLLOW rejects symlink ancestors; real
alias RED→GREEN via directory-only resolution with final retarget recheck.
Oversized stored root test uses4096value cap admitting the actual schema.
Re-review and final validation are pending; no delivery claim yet.
CF4c1 re-review: both original findings ADDRESSED; no new Critical/Important.
Lean targeted fixes:15integration+9units pass, all-target Clippy pass. Full suite
then caught a test-file layout mismatch with platform_boundary scanner: the new
Unix symlink fixture now lives under an explicit cfg(test) module; scanner itself
unchanged and both platform_boundary tests pass. Final full rerun pending.

Task CF4c1: complete (2026-11-12, implementation + fresh task review).
Ruling: Important concrete-backend guarantee and Minor stored-value coverage
addressed; re-review confirms both, no new Critical/Important or deferred minors.
Final local cli-full1276passed/0failed/18existing ignores,75targets; full all-target
Clippy/fmt GREEN. Lean targeted15integration+9units and all-target Clippy GREEN
before the behavior-neutral test-module relocation; native final-head suite is
still required. No new crates or baseline/threshold changes. Final whole-branch
review and docs/Linux/macOS/Windows delivery gates follow; #78 remains open.

CF4c1 final review on dd93b71: partial. Important findings: Windows NOFOLLOW
preflight race, overstrong no-wait statement, actual macOS fixture failure.
Fixes: native HANDLE normalized-path verification via platform facade before SQL/
Ready; gated Windows DB-symlink/recovery regression; explicit native retry
exception (no global setting mutation); canonical test directory on macOS.
Run43243065134: docs/Linux green, macOS failed only alternate-method fixture;
Windows still executing when correction prepared. Final re-review and new native
run remain required. #133 work is isolated in a dependent worktree.

I133 started (2026-11-12, depends CF4c1): два controlled RED подтверждают, что
старый header inspector снимает rollback-reader и registry-observer OS locks:
external writer сначала BUSY, после inspection неожиданно пишет. Logs
/tmp/issue133-red.log, /tmp/issue133-registry-red.log. Исправление переиспользует
native lease и fixed100-byte xRead без SQL. Первые focused integration42/0 и
store units32/0 GREEN, lean all-target Clippy GREEN. Existing test names сохранены;
platform scanner/thresholds/baselines не менялись. Full/fresh reviews ещё впереди.
I133 local validation: full cli-full1279passed/0failed/18existing ignores,
76targets; full/lean all-target Clippy/fmt GREEN. После переноса поверх CF4c1
4063486 (Windows namespace/final review fixes) повторно прошли42focused
integration+32store unit tests и full Clippy/fmt. Повторное исполнение всех
unrelated targets не требуется из-за platform-gated parent correction; native
final-head CI remains required. Fresh task/final review follows.

Task I133: complete (2026-11-12, implementation + fresh task review).
Ruling: Spec compliance PASS, Quality APPROVED, no findings/deferred minors.
Full1279/0/18existing ignores; rebased focused42integration+32units/full Clippy/fmt
GREEN, lean checks GREEN. Final review and native final-head CI remain delivery
gates; parent CF4c1 must land first. Closes #133 only after verified PR delivery.

Delivery decision (2026-11-12): CF4c1 and I133 ship atomically in PR134, as the
user permits strongly related issues in one PR. This supersedes the separate-PR
ordering above: both independently reviewed tasks remain separate commits, but
schema/registry observers land together after combined final review and native
final-head gates. I133 final review converged at5f5f398, no findings/minors.


## 2026-11-12 — CF4c2 actual source migration planning

Task CF4c2: complete (bounded observed source survey + read-only `config migrate
--to 2 --format=json`; actual complete registry, missing/empty distinction, all
input preconditions, explicit budget choice, safe numeric/version/revision output).
Ruling: fresh task review found Windows non-symlink reparse-file admission;
platform-owned attribute guard added at each source metadata boundary. Fix
re-review: ADDRESSED; no new Critical/Important findings. No deferred minors.
Validation: MSRV1.88 cli-full full suite1294 passed,18 unchanged ignored; focused
12 integration +3 deterministic race tests; platform boundary2; all-targets
Clippy/fmt passed. Lean migration/plan/view/observation and3 units passed. Windows
attribute regression and native delivery checks remain PR gates. No baseline,
threshold, dependency, workflow or provider-request changes.
Limits: observations are independent reads, not an atomic multi-file snapshot;
future apply must revalidate all inputs. No raw source export, source writes,
runtime activation, strict-store adoption or executor is implemented here.
Issue78 and the remaining CF4/CF5–CF14 contracts stay open.


## 2026-11-13 — CF6a query-cache policy adoption

Task CF6a: complete (construction-only implementation + fresh task review).
DenseSpec owns query defaults and positive validation; QueryCache holds its
immutable policy with shared entries, eviction order, counters and query flights.
Explicit daemon/lifecycle/direct CLI constructors consume the projection; old
signatures remain adapters. No new source keys, live update/shrink, migration
writes, strict-store activation or broader dense/ANN/backfill adoption.
Ruling: existing DenseSpec is reused rather than introducing a duplicate query
settings catalog. This typed construction slice may precede CF5 without enabling
configurable apply. Accounting remains retained vector payload, excluding external
Arc holders, keys/allocator overhead and in-flight work. Query identity and default
256 entries / 4 MiB behavior are preserved.
Validation: updated origin/main9d12a72, exact stable Rust 1.100.0 and upstream locked
dependencies; pinned frontend built. API/propagation compile RED before GREEN;
9 new integration tests + 1 CLI factory test check actual eviction, shared caps,
counting-embedder reuse/bypass and invalid-before-filesystem admission. Full
cli-full1321 passed, 0 failed, 18 existing ignored; frozen benchmark and all9
existing singleflight regressions passed. Lean42 focused tests passed; full/lean
all-target Clippy, fmt and diff checks passed. Initial Clippy findings corrected
local import ordering and a test literal lifetime without behavioral changes.
Fresh task review: Spec compliance PASS; Quality APPROVED; no findings or deferred
minors. Final whole-branch review and native docs/linux/macos/windows delivery
gates remain required. No dependency, workflow, baseline or threshold changes;
no live inference/model downloads. #78 stays open for the remaining tasks.


CF6a delivery (2026-11-13): PR136 merged as b0c820dfc62fc33e2ba2da2a25b96b01ee51b1c4.
Fresh final review converged without findings. Native run43386029380 attempt1:
docs/Linux/macOS/Windows green; inventory retained every parent test and ran all10
new tests/platform. Linux1321+2static/macOS1317/Windows1305 passed,18 existing
ignores/platform. Reviewed source/tested merge/squash share tree
9994e24f5203271a2ddc9cdbe8b46a7398027dab. #78 stays open.

## 2026-11-13 — CF6b dense construction and ANN backfill policy

Task CF6b: complete (construction-only implementation + fresh task/fix review).
DenseCacheLimits defaults project the existing DenseSpec, whose complete immutable
policy passes through CLI/lifecycle/Shared; production synchronous/background sync
uses that cache's actual limits. Compatibility APIs remain adapters. Legacy Config
vector-store budget keeps its explicit precedence over the dense-path fallback.
Ruling: shared static builder/backfill eligibility covers chunk-row count, min/max
vectors, project capacity, encoded model width and graph+exact peak bytes. It is
not a reservation: existing resident/live-worker accounting is unchanged. Small
corpora do not take temporary ANN pins; eligible duplicate rows retain pins using
row count rather than distinct hashes. The debug global cap hook is removed in
favor of per-call policy. Existing leases/TTL are not revoked by eligibility.
TDD: missing APIs compile RED; actual small backfill held4 leases instead of0;
maximum-cap overflow erroneously admitted; inner width/overhead boundary panicked.
All now GREEN. Fresh task review found that inner overflow (Important) and a
masked count-gate fixture (Minor); checked owner estimator plus isolated fixture
fixed both. Re-review: both ADDRESSED, no new Critical/Important, no deferred minors.
Validation: exact stable Rust1.100.0, upstream locked dependencies and pinned Web
assets. Final cli-full1332passed/0failed/18existing ignores,81targets; lean65 focused
passes; full/lean all-target Clippy/fmt/diff GREEN. Frozen benchmark and worker
reservation/invalidation/load/build/cancellation regressions pass. 11 net new tests
exercise actual SQLite metadata/vector leases/cache fallback/eviction/concurrent
policies and CLI/daemon propagation. No baseline/threshold/dependency/workflow
changes or live inference/downloads. Final whole-branch review and native delivery
gates follow. Live shrink, new source keys/apply, strict-store/TTL adoption and
remaining #78 tasks are not completed by CF6b.


CF6b delivery (2026-11-13): PR137 влит как f027b382a31e1e5c3766454b842f12d55dca0bd7.
Первый native run43392113171 выявил Web readiness race #138; W7 устранил
воспроизведённый интервал до регистрации signal handler. Оба task review и свежее
combined final review завершились без замечаний. Final native run43394892652,
attempt1: docs/Linux/macOS/Windows green. Аудит: Linux1333+2static/macOS1329/
Windows1316 passed,18прежних ignores/platform; все parent tests сохранены,
12новых Unix/11Windows выполнены. Reviewed source/tested merge/squash имеют tree
b290007c129489fbbff0e0a5dfd1dca6a987cfc2. #138 закрыт; #78 остаётся открытым.
#71 только offline; #109/#113 остаются отдельными задачами.

## 2026-11-13 — CF6c: сроки жизни ANN при создании

Task CF6c: complete (реализация, RED/GREEN и свежее task review).
AnnLifetimes проверяет положительные целые signed seconds для pins и положительный
Duration для монотонных outcomes; закрытые поля сохраняют инварианты raw-limit
constructors. DenseSpec владеет обоими часовыми defaults, origins и daemon scope.
Полная policy уже проходит через CLI/lifecycle/Shared/sync.
Ruling: первый pin и продление берут TTL у реального cache; error cleanup сохраняет
leases другого проекта той же модели и другой модели того же проекта. Прежние
saturation/ownership/identities/accounting сохраняются; нет live update/revocation.
Status использует фактический outcome TTL и generic stale reason; state names и
legacy default helper прежние. Нет второго catalog/runtime defaults/нового lock.
TDD: absent API compile RED; initial lease3600 вместо600 и status ready вместоstale.
После исправления первого pin отдельный renewal RED всё ещё давал3600 вместо600.
Теперь GREEN. Настоящий SQL probe укорачивает owned lease во время fake embedding
без sleep/clock override; настоящий metadata/status сравнивает custom и default cache.
Validation: Rust1.100.0, upstream lock/frontend прежние. Full cli-full1339passed/
0failed/18прежних ignores,82targets; финальные усиленные6lifetime+9cache и direct
CLI propagation проходят; lean50focused pass. Full/lean all-target Clippy/fmt/diff
GREEN. Шесть новых тестов: domain/transactional origins/scope, initial/renewed/default
lease, error ownership и actual status. Без dependency/workflow/baseline/threshold,
source keys/apply, inference/download changes. Остальные CF6–CF14/#78 открыты;
#71 только offline, #109/#113 не затронуты.

Fresh task review: Spec compliance PASS; Quality APPROVED; замечаний и deferred
minors нет. Reviewer сверил точный worktree с immutable diff. Fresh final review
и required native CI/inventory audit остаются условиями доставки.


CF6c delivery (2026-11-13): PR139 влит как 4c9ff9f4ce421c6290eb81f578d051d71c2d74ad.
Fresh final review converged без замечаний. Native run43400126108 attempt1:
docs/Linux/macOS/Windows green; Linux1339+2static/macOS1335/Windows1322 passed,
18 прежних ignores/platform. Все parent tests сохранены и все6 lifetime tests
выполнены. Reviewed source/tested merge/squash имеют tree
bb81f3bf5542e791dd68dc837a6655054390ee8b. #78 остаётся открытым.

## 2026-11-13 — CF7a: владелец общей базы векторов

Task CF7a: complete (implementation, verification и fresh task/fix review).
VectorStoreSpec владеет entry/byte/timeout policy и origins; legacy defaults
проецируют тот же каталог. VectorStore удерживает immutable policy, canonical root,
shared atomic recency и native exclusive lease. Его отдельные connections/клоны
удерживают lifetime; legacy constructors используют shared leases без смены v1 caps.
Ruling: lifetime exclusion advisory для cooperating current callers, не persisted
v2 activation/ACL. Lockfile не удаляется. Raw SQL/older binaries/external namespace
replacement вне гарантии; после последнего release legacy снова доступен. Нельзя
включать v2/migration до CF5/CF7 consumer rollout и recovery/quiescence. Strict
put+evict и unpin+evict атомарны; get/page/write берут recency после immediate
reservation. Leased rows могут превышать caps; payload budget не равен disk/RSS cap.

TDD: baseline28 passed; API compile RED. Независимые consumer RED: idle handle
вытеснял recent вместо old; failed put оставлял2 rows вместо1; failed unpin удалял
2 leases вместо rollback. Native descriptor duplicate RED показал задержку release
после close — acquired guard теперь явно unlock-ит lock перед закрытием; failed
probes не unlock-ят чужое владение. Все случаи GREEN. Полный suite выявил ADR-9
boundary violation: private nofollow/nonblocking open перенесён в platform seam.
Точечные Clippy fixes: актуальный Atomic::try_update для Rust1.100, doc markup и
static literal lifetime тестового embedder; без подавления lints.

Validation: exact stable Rust1.100.0, upstream Cargo.lock и pinned frontend.
Full cli-full1358 passed/0 failed/18 прежних ignores;83 top-level summaries,
вложенный filtered subprocess summary не считается отдельным тестом. Lean82 focused
passed; full/lean all-target Clippy, fmt/diff GREEN. Новые18 Unix/17 Windows
integration tests и2 native unit tests проверяют real SQLite/OS/subprocess/threads,
общие caps/LRU/reopen/leases/rollback, zero-timeout BUSY, no-I/O admission/scope/origins,
отсутствие скрытого eviction при acquire и legacy zero-cap. Нет зависимости,
workflow/baseline/threshold, file source keys/apply или inference/download changes.
Fresh task/final review и required native CI/inventory audit — следующие gates.
Полные CF7/#78, #71 beyond offline, #109/#113 этим slice не закрываются.

Fresh task review: Spec compliance PASS; Quality APPROVED; Critical/Important нет.
Minor: BUSY сам по себе не доказывает timeout=0. Исправлено direct PRAGMA проверкой
двух actual owned connections, включая clone, для0/17/5000ms. Re-review: ADDRESSED,
новых Critical/Important нет; deferred minors нет. После test-only добавления
final7 vector unit +18 integration pass; оба all-target Clippy/fmt/diff GREEN.
Production source совпадает с full1358 suite; native inventory теперь ожидает
19 Unix/18 Windows новых tests + первоначальный native unit, всего20/19 additions.
Fresh final review и native delivery следуют отдельно; #78/CF7 остаются открытыми.

CF7a delivery (2026-11-13): PR140 влит как254af73d087bb44e75ed5f71794669689805e716.
Fresh final review converged без замечаний. Native run43417372537 attempt1:
docs/Linux/macOS/Windows green; Linux1359+2static/macOS1355/Windows1341 passed,
18 прежних ignores/platform, все parent tests сохранены,20новых Unix/19Windows
выполнены. Reviewed source/tested merge/squash имеют tree
10c8c4da1b6eed442f0eefe194c93a22d4b5deff. CF7/#78 остаются открытыми.


## 2026-11-13 — CF7b: source для dense/jobs/search

Task CF7b: complete (implementation и local verification; fresh reviews следуют).
VectorCacheSource хранит explicit legacy limits либо clone immutable owner. Root
проверяется без создания другого каталога; strict connections не fallback-ят.
Dense cold loads/worker unpin и full/scoped/streaming search используют source;
sharing_query_cache и worker snapshot удерживают owner. Strict aliases разделяют
canonical cold key и metadata identity, legacy raw-root semantics сохранены.
Ruling: project budget setter теперь Result; Owned отказывает даже равному budget.
Оба daemon caller передают отказ; reload делает это до invalidate/publication.
Experimental library signature меняется явно, stable CLI/MCP/file/IPC прежние.
Source-taking jobs используют прежний core, TTL/ownership и short SQL phases.
Production startup/status/sync factories, persisted adoption и consent/recovery
остаются следующими CF5/CF7 tasks; blockers migration не сняты, #78 открыт.

TDD: absent API compile RED; real search streaming дал mode conflict, wrong root
успевал вызвать query provider. Causal mutation RED: worker оставлял3leases вместо1;
late setter менял config до отказа; raw cold key запускал2loaders вместо1.
Все GREEN. Новые9integration +3unit regressions проверяют настоящие SQLite,
metadata, search results и native lifetime: content reuse двух projects, shared
limits/legacy0, cold identity, sharing lifetime, scoped/streaming equality,
provider BEGIN IMMEDIATE probe при backfill с pins, worker-only owner retention,
сохранение чужих leases и прежних config/stages/resident rows после отказа reload.

Validation: stable Rust1.100.0, upstream lockfile и pinned Web. Full cli-full1370
passed/0failed/18прежних ignores,84top-level summaries;2nested filtered subprocess
summaries исключены. На том же production source final489unit+9integration pass
(добавлен1alias cold test), затем final alias GREEN после causal RED. Lean61focused
integration +15vector-filtered unit passes. Full/lean all-target Clippy/fmt/diff
GREEN. Нет новых crates/workflows/baseline/threshold/file keys или provider calls/
downloads. Fresh task/final reviews и native inventory audit остаются gates delivery.

Fresh task review: Important — Db-taking get/warm читали generation до root admission.
Проверено causal RED на настоящей чужой БД без project: Internal вместо Admission.
Исправление проверяет root до SQL в обоих adapters; новый test сохраняет также
legacy Internal semantics. GREEN10consumer tests; re-review ADDRESSED, новых
Critical/Important нет. Новых tests теперь13 (10integration+3unit). Final refreshed
full/lean/Clippy и fresh final review следуют; deferred minors нет.

Post-review validation: final full cli-full1372passed/0failed/18прежних ignores,
84top-level summaries,2nested filtered subprocess summaries исключены. Все13новых
regressions выполнены на final source. Final lean10consumer + прежние52integration/
15vector-filtered unit и1alias-unit pass (78distinct focused tests); full/lean
all-target Clippy/fmt/diff GREEN. Fresh final source review и native delivery pending.

CF7b delivery expansion (2026-11-13): run43424811833 attempt1 — docs/macOS/Windows
pass, Linux488/1 fail в прежнем watcher assertion helper. Новый #144/C12 исправляет
test observer, без production changes/retry/skip; source-backed causal RED/GREEN и
отдельный свежий task review PASS/APPROVED. Новый native run обязателен для всей ветки.
Отдельный #143 выявил main debug-only lease probe в release tests. В новых CF7b tests
probe заменён test-only SQL count, production API не расширен. Старые callers/#143
ведёт второй агент; полное закрытие #143 этим изменением не заявляется.

Final CF7b/C12 source review: converged, Spec PASS / Quality APPROVED; Critical,
Important и Minor нет. Final refresh:490unit+10consumer debug; lean11watch+
10consumer; release1worker+10consumer+1watch regression; full/lean all-target
Clippy/fmt/diff GREEN. Полный1372pass относится к тому же production source до
C12; после него менялись только test observer/probes и docs. Все14новых tests
должны выполниться в новом native run; первый run не засчитываем как final green.
Task CF7b implementation/review complete, native delivery остаётся pending.

## 2026-11-13 — CF7c1: embedding context в index jobs

Task CF7c1: implementation in progress. Предварительная база — reviewed6d6e3bc
CF7b/C12, чей native delivery продолжается отдельно. Перед PR новый task будет
перенесён на актуальный origin/main. IndexEmbedding заимствует immutable source и
explicit optional embedder; новые prepared full/incremental adapters сохраняют
общий core и прежние configured compatibility APIs.
TDD API RED — отсутствующие symbols;6real integration cases GREEN. Causal accidental
legacy-open RED даёт Admission mode conflict; late stale gate RED меняет persisted
metadata до отказа. Возвращены настоящие sources. Unit gate контролирует incremental
и recreated full recovery; rejected state/provider/markers неизменны. Provider probe
открывает BEGIN IMMEDIATE в registry и vectors.db во время actual callback.
Production daemon/watcher/sync composition и persisted migration не заявлены; #78 открыт.

CF7c1 implementation/local verification: complete. Полный cli-full1380passed/
0failed/18прежних ignores,85top-level summaries (2nested filtered subprocess
summaries исключены). После full run добавлен только1root-ordering regression:
causal removed-admission RED с unusable foreign registry, затем7consumer full/lean
GREEN. Итого8новых tests:7integration+1unit. Lean50focused tests (gate+49integration,
включая final extra case); final full/lean all-target Clippy/fmt/diff GREEN.
New full/incremental callbacks не меняют legacy provider timing; core R2/W103
semantics сохранены. Fresh task/final reviews и native delivery pending.

CF7b/C12 delivery: PR142 squash bbf650dad6923c2536fcd617cd887973e948c93e.
Required docs/Linux/macOS/Windows GREEN в run43427786037 attempt1. Скачанный
native inventory относительно PR140 сохраняет каждого parent test,18ignores/OS;
все14new tests executed. Linux1373+2static, macOS1369, Windows1355pass/0fail.
Reviewed6d6e3bc/tested mergea4bd885/actual squash tree совпали:
515907dde54c0412c457b58b7228629c50f59d63. #144 закрыт, #78 открыт.
CF7c1 перенесён на этот main без изменения code tree. Его final7integration
release GREEN; task review идёт, final/native delivery остаются pending.

Fresh CF7c1 task review: Spec PASS / Quality APPROVED, findings/minors none.
Final whole-branch review и native delivery остаются gates.

Task CF7c1: complete (implementation/local verification/fresh task review).
Independent final source review и required native inventory остаются delivery gates;
полное завершение CF7/#78 не заявляется. No deferred minors.

CF7c1 delivery: PR145 squash3b347320d9399edebbb3d597df74fd720d2f7f14,
required4checks run43430656050 attempt1 GREEN. Downloaded parent inventory PR142
сохранён полностью,18ignores/OS;8new tests executed. Linux1381+2static, macOS1377,
Windows1363pass/0fail. Reviewed7c2c9b7/testedc71e052/squash tree совпали:
7bd2b66cbb1acca2401e104154194a64787278aa. #78 остаётся open.

## 2026-11-13 — CF7c2: prepared sync/index/status composition

Task CF7c2: implementation in progress на актуальном main3b34732. Новые prepared
API несут Config/stages/source через index и полный backfill/child Db; callback
cores сохраняют legacy state/error timing. Actual daemon/CLI callers используют
prepared paths, stage admission до enqueue без shared DB guard. Captured legacy
policy не читается из более новой dense map. Auto-index проходит authority; raw
config не публикует budget. Source-aware coverage не строит provider.
TDD: missing API/method RED; real owner-backed core7integration GREEN. Actual
status old coverage дал Null вместо1/1 (causal RED), fixed source propagation GREEN.
3new project-state regressions среди15unit GREEN; valid disabled local provider
fixture не делает inference/downloads. Initial malformed fixtures были исправлены,
не засчитываются causal RED. Omitted stage preparation mutation RED возвращает job
вместо Admission, source restored. Full/lean/local lint/review/native gates pending.
Legacy mode остаётся production startup; watcher CF7c3, CF5 adoption и #78 open.

CF7c2 local verification: complete cli-full1391passed/0failed/18existing ignores,
lean1198passed/0failed/11existing ignores;86top-level summaries в каждом прогоне,
по2nested filtered subprocess summaries исключены. Все10new regressions выполнены;
full/lean all-target Clippy/fmt/diff GREEN. Provider-admission regression изолирован
в child process от ORT_DYLIB_PATH/OPENROUTER_API_KEY и повторно прошёл full/lean.
Fresh task/final reviews и required native inventory остаются delivery gates.

Fresh CF7c2 task review: Spec PASS / Quality APPROVED;1Minor исправлен: failed
provider test теперь требует ровно2persisted retry rows перед проверкой hashes.
Final7integration full/lean GREEN, test-only assertion delta после full suites.
Independent final source review и required native delivery/inventory pending.

Task CF7c2: complete (implementation/local verification/fresh task review).
Ruling: Minor по vacuous marker assertion исправлен, re-review ADDRESSED;
новых Critical/Important нет. No deferred minors. Final/native delivery остаются
gates; полное завершение CF7/#78 не заявляется.

CF7c2 final review converged at d5601d4, findings/deferred minors none. Первый
native run43435698037 attempt1 GREEN: Linux1391+2static, macOS1387, Windows1373
pass/0fail/18existing ignores; каждый parent145 test сохранён,10new tests/OS
выполнены. Во время delivery merged upstream PR141 (Node IPC); branch обновлён
обычным merge до0517793. Diff против нового main0c6d707 byte-identical прежнему
reviewed patch; independent integration re-review converged, no findings.
Refreshed native run43438407813/inventory обязателен; первый run не final acceptance.

## 2026-11-13 — CF7c3: prepared watcher runtime

Task CF7c3: implementation in progress. Source base — reviewed CF7c2, затем
обновление с main0c6d707/PR141 без конфликтов. WatchRuntime несёт config/stages/
source через общий producer/consumer/retry core. Actual Shared ensure готовит
stages без DB/state guards и не создаёт watcher из устаревшей revision; reload
меняет целый snapshot. Configured compatibility APIs сохранены.
RED missing APIs и actual daemon observer без prepared context; GREEN7real
watcher regressions+2daemon regressions. Causal mutations: legacy consumer теряет
vector coverage; config-only revision identity пропускает obsolete stages; missing
ensure revision check создаёт watcher со старой policy. Sources restored;18watch
и17project-state tests GREEN. Local full/lean/lints, fresh reviews/native pending.
Strict startup/file-v2/persisted adoption/apply и полный #78 остаются open.

CF7c3 initial local suites: full1402pass/18existing ignores, lean1208pass/11existing
ignores,0failures; full/lean all-target Clippy/fmt/diff GREEN. Fresh task review
выявил Important: Config-equal reopen публиковал new stages в Shared, сохраняя old
watcher runtime. Новый actual Shared regression RED, затем watcher обновляется
при каждой успешной runtime publication; dense invalidation остаётся semantic.
Final18watch+18project-state GREEN,10new tests. Полные suites/lints повторяются
после production fix; re-review/final/native pending. Первый локальный run не
final verification изменённого source.

Task CF7c3: complete (implementation/local verification/fresh task review).
Ruling: Config-equal publication Important ADDRESSED, re-review новых Critical/
Important не нашёл. Independent final bounded-source review converged; unmet
requirements/findings/deferred minors none. Final cli-full1403passed/0failed/
18existing ignores, lean1209passed/0failed/11existing ignores;87top-level summaries,
по2nested filtered child summaries исключены. Все10new tests executed. Final
full/lean all-target Clippy/fmt/diff GREEN. Required native inventory/delivery и
обновление на actual CF7c2 squash остаются gates; CF5/CF7/#78 ещё не завершены.
