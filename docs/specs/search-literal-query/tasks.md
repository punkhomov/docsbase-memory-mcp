# План machine execution: #87 + #111

Goal: explicit literal mode без default/query-quality regression; specs в этом
каталоге, constitution/AGENTS обязательны. Один task за раз, TDD до production,
fresh task reviewer; finding fixes делегируются, scoped re-review. No new crates/
unsafe/baseline edits; official cli-full + relevant lean; final4checks/most-capable
whole review. Grant пользователя разрешает root normal squash после gates.

## LQ1 — typed owner, literal core и daemon propagation

Files: add src/index/query.rs, tests/literal_query.rs; modify index/mod.rs,
index/tantivy_index.rs, daemon/tools.rs. При необходимости private mode parser
unit tests в query.rs; product fixture в literal_query.rs через existing public
index/store/tools seams. Requirements FR1/2/3/6, SC1/3.

TDD: actual7Syntax errors уже подтверждены историческим report; новые public
mode APIs сначала missing-API compile RED (feature missing, не ложное требование
изменить старый Syntax). GREEN проверяет реальные hits обеих index APIs и actual
tool propagation, default/explicitSyntax equivalence/typed errors, literal
operator/quote/colon/sign cases/no-term. Отдельно покрыть raw provider query.

Verify full cli-full; focused literal/index/query/schema/hybrid+quality;
relevant lean, full/lean all-target+optinClippy/fmt/diff. Frozen113 + old reports
byte-identical. Fresh review, source freeze, delegate findings, local commit.

## LQ2 — negotiated clients, CLI/MCP/schema/docs (#111)

Depends LQ1. Files: ipc/protocol.rs, ipc/client.rs, ipc/async_client.rs,
daemon/server.rs; CLI mod.rs/search.rs and tests; mcp/tools.rs; relevant
ipc protocol/server/client/CLI/MCP tests. README/docs/MCP/CONFIG as appropriate,
component-configuration/settings-inventory.md X01 dated semantic extension.

TDD native fake scoped peer with search-context-v1 but no literal cap; requested
literal must produce Protocol BEFORE search dispatch, with no downgrade/direct
fallback on an indexed local project. Default Syntax remains compatible.
Cover sync/async tool helpers, CLI direct and daemon, MCP actual request mode,
invalid mode before provider work. Keep legacy-cap fixtures and original scope.
Fix tags description only, retaining runtime AND behavior. Public Options stable.
Full cli-full + focused full/lean clients/wire/CLI/MCP and existing tags tests;
all-target/optinClippy/fmt/diff/docs, frozen inputs; fresh review/local commit.

## LQ3a — honest external query-mode measurement

Depends LQ2. Files: tests/support/external.rs, external/execution.rs,
tests/support/bench.rs, tests/search_bench_external.rs; external-track.md.
Add optional DOCSBASE_EXTERNAL_QUERY_MODE, owner Syntax default, explicit report
provenance. Existing error-denominator test explicitly Syntax; new Literal
case keeps denominator and records no parser error. No replay/provider rewrite.
Report writer protections unchanged. Focused full/lean benchmark/external/
quality and full suite/lints/frozen; fresh review and CLEAN local source commit
BEFORE actual paired measurements, preserving engine/source/harness identity.

## LQ3b — actual paired report, docs and final acceptance

Depends LQ3a. Data outside repo scratch/issue-87/data/scifact has raw3hashes equal
oldreport, 5183docs/1109queries/same300positive IDs. Record pinned mirror, actual
fetch timestamp/license and hashes; canonical domain needed no policy change.
Run both Syntax/Literal offline BM25 release on same clean committed runtime,
full features, configs Bm25 only, cost0; new outputs protected from overwrite.
Retain all300cases and7originalqueries. Assert Syntax7parseerrors at sameIDs,
Literal0 without excluding queries; preserve historical reports/frozen suite.

Add new JSON/MD reports under docs/specs/search-benchmark/reports and final
README/MCP/architecture/query examples. Record data/mode/profile/runtime/harness
pins, results and honest limits without claiming retrieval misses fixed.
Task review (reports/docs, no functional rerun for prose), commit metadata.
Whole-feature most-capable review maps every FR/SC/task. Push final reviewed
head and require docs/linux/macos/windows all0 with inventories/identity;
normal ready/squashmerge only after convergence. Fixes87/111 only on completion.
If new confirmed bug, duplicate-check/issue/reprioritize; no silent gate relaxation.

## Delta 2026-11-11 — LQ3a file scope refinement

После чтения actual execution seam tests/support/bench.rs не меняется:
external/execution.rs уже непосредственно вызывает общий product tool. Новый
mode передаётся туда из ExternalOptions и включается в provenance. Это
исключает неиспользуемый Runtime wrapper и сохраняет frozen harness byte-identical.
Остальные LQ3a/LQ3b acceptance/TDD/review/clean-commit требования прежние.
