# Progress: explicit literal query mode (#87 + #111)

2026-11-11 base main528369e7ef272257be4e5eeaa216758a0d87d27b. Prior#66/#114/#115
merged by PR112; full final/source gates/reviews closed. Main pushCI42959401173
also SUCCESS on identical tested tree. Worktree /workspace/docsbase-issue87,
branchfeat/literal-query-87. Autonomous machine grant applies;12/80excluded.

Task LQ0: design ready before production code. Requirements/design/tasks record
owner mode/default, literal AST semantics, public API compatibility, old-daemon
capability refusal, original provider inputs and honest external report pair.
Challenge: preserve Syntax default; no lenient/silent fallback/whole-phrase
semantics; old daemon ignores new args, so cap required before literal dispatch.

Task LQ0: Ruling: request mode default/spellings/cap identity are fixed v1
semantics (inventory X01), not deployment tuning. One owner initializes absent/
null mode; consumers receive typed mode. No hidden config/cache/time defaults.

Task LQ0: data feasibility verified. Canonical BEIR host CONNECT403, local raw
inputs absent. Pinned public GitHub mirror retrieved all3 files; BLAKE3 exactly
matches historical report,5183docs/1109queries/300samepositive test IDs. Source/
license/timestamp/hash proof outside repo scratch/issue-87/dataset-verification.json.
No env policy/config publication change or paid key needed. Corpus stays local.

Task LQ1: pending TDD. No production source modified yet; initial frozen source/
Cargo/fixtures/report snapshots will be captured before tests.

Task LQ1: in progress. Before core production, actual missing-API RED
(`cargo test --locked --features cli-full --test literal_query`) failed with
E0432/E0599 for QueryMode/search_with_mode. After core implementation, actual
runtime RED passed7/failed2: tool literal still reached Syntax parser; invalid
mode was ignored and called reranker. Then shared owner validation and typed
SearchArgs→lexical propagation were implemented. GREEN same9test target passed.
Evidence: scratch/issue-87/lq1/{red-api,red-tools,green-literal}.{log,json}.
Ruling: ordinary raw word repetition is deduplicated in first order, matching
existing grammar; ninth retrieval/score-equivalence test added before core code.
One local formatting preflight failed because import preceded module rustdoc;
archived/corrected before runtime RED, not counted as a behavioral failure.
No persistent index/tokenizer/provider-input change. Further checks/review pending.

Task LQ1: complete (base528369e; task commit contains this line). Actual final
`cargo test --locked --features cli-full --no-fail-fast`:1161passed/0failed/
13ignored,67targets. Focused full index/hybrid/golden/benchmark and lean literal/
index/hybrid passed; full+lean all-target and opt-in artifact/perf Clippy, fmt,
diff-check all0 on identical170Rust source hashes/Cargo. Frozen113 and historical
2reports/Cargo unchanged. Fresh clean-context reviewer: specPASS/qualityAPPROVED,
no findings; no deferred minors. Review/evidence scratch/issue-87/lq1/.
Ruling: native platforms, client negotiation, CLI/MCP/schema/docs and measured
reports are LQ2/LQ3/final acceptance, not claimed complete by this core task.
Existing original-scope/cancellation/provider-input/cache semantics retained;
full prior suites passed without tests/fixtures removal or relaxed gates.

Task LQ2: in progress. Actual native-client RED:2passed/4failed, old scoped peer
received literal requests and helpers accepted search after a failed second
Hello. Actual CLI RED rejected unknown --query-mode; actual daemon RED advertised
only search-context-v1. Added owner validation/literal-query-v1 admission to
sync/async helpers, clear capabilities before new Hello, additive daemon Hello,
CLI mode-aware wrappers preserving public Options and original workflow.
GREEN seven focused public/native targets:72passed/0failed (cli_read13,
cli_routing6,ipc_protocol9,ipc_server18,literal_clients7,mcp_docs10,mcp_frontend9).
No exclusions or sleeps added.
Old scoped fake peers remain old; incapable-peer refusal includes actual CLI
with a valid local index and MCP, with no search/replay/downgrade observed.
Actual CLI and MCP literal hit a real daemon; Syntax still errors on punctuation.
Existing real block-list AND filter passes; tags catalogue description corrected.
Full/lean/lints and fresh task review remain pending. Evidence scratch/issue-87/lq2.

Task LQ2: Ruling: independent review Important I1 found MCP schema contradicted
null→Syntax. Finding fix delegated (root did not implement); actual discovery
RED confirmed old constraints, then schema type/enum gained null and real MCP
omitted/null/Syntax equivalence/invalid-value tests. Scoped original reviewer:
I1 ADDRESSED, no new Critical/Important, final specPASS/qualityAPPROVED.

Task LQ2: complete (base7e85d7e; task commit contains this line). Final actual
full cli-full1172passed/0failed/13ignored,68targets; full/lean public boundaries
73passed each. All11GREEN checks0 on identical171Rust source/Cargo hashes,
including full/lean all-target+opt-in Clippy/fmt/diff. Root and reviewer matched
source identity. Frozen113,2historical reports,Cargo and original RED/GREEN
records unchanged; core9/native7/previousMCP9 preserved. No deferred minors.
Evidence scratch/issue-87/lq2/null-fix/{report,verification,root-audit,re-review}.
Native CI and measured external reports remain final/LQ3 obligations.

Task LQ3a: in progress. Dated design/task delta records actual existing external
execution seam; bench.rs remains byte-identical, no unused wrapper added.
Actual API RED E0560 proved missing ExternalOptions.query_mode before production.
After owner field/provenance, runtime RED8passed/1failed/1ignored proved merely
labelling Literal still left its parser error. Separate child-process env RED
showed invalid auto silently generated a report. Then explicit mode was passed
to warm-up/every query and env parsing added through owner FromStr. GREEN external
10passed/0failed/1manual ignored; default/explicitSyntax/Literal provenance,
raw-input hashes/full denominator, original query bytes, actual hits, no-provider
cost and separate-process valid/invalid env paths covered. Existing Syntax error
measurement remains explicit and valid; report no-overwrite/replay guards intact.
Ruling: initial runner inherited duplicate red-api key and ran old core9test target
successfully; archived as preflight, never counted as RED. Unique external-key
actual failing logs supply TDD evidence. No production before its actual RED.
Full/relevant lean/quality/lints and fresh review pending. No actual SciFact pair
has been run yet: clean reviewed source commit is mandatory first.

Task LQ3a: complete (base92fe57a; task commit contains this line). Final actual
cli-full1174passed/0failed/13ignored,68targets. External10/0/1; focused and lean
external/benchmark/golden pass; full/lean all-target+opt-in Clippy/fmt/diff pass.
All10GREEN records match171Rust source/Cargo hashes. Frozen113,2historical reports,
bench.rs unchanged; production/Cargo byte-identical to reviewed LQ2. Fresh reviewer
specPASS/qualityAPPROVED/no findings or deferred minors. Evidence scratch/issue-87/lq3a.
Ruling: this clean reviewed source/harness commit is the measurement pin. Real
SciFact300case Syntax/Literal report pair starts only after committing this task;
query counts/error IDs/input/profile/source hashes will be checked independently.

Task LQ3b: measured pair ready for review. Actual commands ran separately on clean
source/harness645485b739b042e468d006dd7288c0bb75a47e8a, release/cli-full/Rust1.88/
Linux x86_64, networknone, Bm25/cap0/spent0/requests0. Same5183docs/1109rawqueries/
300positive IDs, raw3BLAKE3/derivedhash/split/adapter1/tokenizer7 as old report.
Independent runtime/harness BLAKE3 plus SHA256 source snapshots/gitclean/HEAD
checked before/after both. Syntax summary exactly matches historical211hits/
89misses/7errors at1232/1280/1320/279/42/744/993; Literal217hits/83misses/0errors.
Ruling:1280 still misses; Literal also gains1041/1259 and loses514/551 among
previously valid Syntax cases. All300cases and18 changed metrics (282same) remain
visible. Explicit literal semantics do not promise per-query dominance; no query/
qrels/ranking/budget/baseline tuning or case exclusions to conceal losses.
Both generated JSONs copied byte-identical to new report paths; historical files
untouched. New MD records dataset source/license/fetch, exact pins/profile,
all7error outcomes, all11otherchanged cases, timing limits and honest scope.
Metadata-only changes since measurement pin; no runtime/harness/source edits.
Evidence scratch/issue-87/lq3b/{paired-proof,root-comparison-audit}.json and rawlogs.
Fresh report/doc task review and whole-feature/native acceptance remain pending.

Task LQ3b: complete (report/docs implementation; sourcepin645485b, metadata commit
contains this line). Fresh report reviewer specPASS/qualityAPPROVED/no findings.
Independently recomputed600 case metrics from raw qrels/ranks and both summaries,
all inventories/18changes/raw3/runtime/harness BLAKE3/305-file SHA256 source snapshot,
exact generated-report copies and protected old2reports/113inputs. No deferred
minors. No source/harness changes since actual clean paired measurements.
Ruling: global feature acceptance remains pending: whole-feature most-capable
review and docs/linux/macos/windows on the final reviewed head before readiness/
normal squash. Report task completion does not claim those later gates passed.
