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
