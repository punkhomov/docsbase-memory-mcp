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
