# Progress

2026-11-24: started ASK1 on main 36f08d2. User timebox: one hour active work,
excluding GitHub Actions wait. Secret kept out of repository and receipts.

Implementation checkpoint: Rig completion codecs, existing provider factory,
shared execution, bounded read loop and MCP/IPC route implemented. Eight initial
codec/loop tests passed; expanded isolated subset 17 passed/1 live ignored.
Strict hybrid-remote Clippy passed before the final composition/cancellation tests.
Full cli-full suite is running. Frontend assets and both TypeScript packages built
with exact Node 24.22.3 and frozen pnpm lock; no dependency changes.

Live blocker: unauthenticated GET https://openrouter.ai/api/v1/models was refused
by the environment proxy (CONNECT 403). The current enforced allowlist excludes
openrouter.ai; the requested single-domain grant did not apply. No key was sent,
no model inference occurred, and no stress/retry loop was run. SC-3 remains
unverified until network access is granted; implementation is tested with fixtures.

Final local source checkpoint (before reviewer fixes, if any): full cli-full
suite PASS; then changed-boundary checks PASS (22 focused/1 live ignored, plus
mcp_docs/mcp_frontend/provider_settings), strict cli-full Clippy and lean compile.
Source validation additionally refuses same-turn guessed citations: a source must
have appeared in a previous model request, not merely in a just-executed read.
Source metadata overhead participates in the read byte limit.
Fresh ASK1 review requested; final/native acceptance will be recorded in the PR
against its exact head to avoid a documentation-only CI head cycle.
