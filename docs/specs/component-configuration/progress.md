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
