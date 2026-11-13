# Progress

2026-11-11 — base main `4811720`, original requirements retained verbatim.

Task W1: complete (audit, ADR-WEB-1, daemon-owned read-only contracts and WebSpec).
Ruling: fresh design review identified mutating legacy registration; dedicated
`BindReadOnly` avoids indexing, backfill and provider initialization.

Task W2: complete (embedded HTTP, secure bootstrap/session, bounded IPC,
shared deadlines, actual socket cancellation, lifecycle/platform integration).
Ruling: backend review APPROVED after bounded error-path shutdown, body deadline,
ANN owner classification and separate cancellation/expiry policies were verified.
Transport tests: 4 passed; lifecycle: 3 passed; platform boundary and generated
contracts passed. A provider-secret DTO regression is included in the final suite.

Task W3: complete (Svelte UI, generated/runtime contracts and real-daemon browser tests).
Ruling: fresh frontend review APPROVED after serialized initialization,
settled sibling reads, project-scoped cancellation and retained bootstrap retry.
Seven Playwright scenarios passed with retries=0, including restart recovery.
Desktop/mobile screenshots inspected; long paths/headings/configuration wrap.
Evidence: [projects](evidence/projects.png), [search](evidence/search.png),
[mobile](evidence/mobile.png), [configuration](evidence/configuration.png),
[empty](evidence/empty.png), [error](evidence/error.png),
[loading](evidence/loading.png), [cancelled](evidence/cancelled.png),
[disconnected](evidence/disconnected.png).

Task W4: complete (delivery implementation and local acceptance).
Final native acceptance is recorded in [PR #126 checks](https://github.com/punkhomov/docsbase-memory-mcp/pull/126/checks).
Ruling: fresh delivery review APPROVED. Explicit locked frontend builds in native
jobs; Linux browser evidence, safe daemon/Web/console logs; source-free artifact
smoke with empty executable PATH passed. Size cap replaced by measurements in
all affected normative specs. Bundle: 54,879 bytes (uncompressed embedded files).
Frontend typecheck/build/state test and six CI-observer Python tests passed.
Final Clippy cli-full/all-targets and opt-in artifact/performance targets passed.
Lean and web-only source checks passed. Local functional verification: 1222
successful tests; one concurrent timing-gate failure passed in isolation at the
original threshold, and remaining targets were then completed. No gate changed.
Native macOS passed all checks; Linux passed the full Rust suite, then exposed
an unsupported Ubuntu 26.04 browser installer in Playwright 1.56.1. Updated to
Playwright 1.64.1 (published 2026-10-14, beyond the 14-day supply-chain window),
which explicitly supports that runner. Required CI checks remain the acceptance
record; no failing check is waived.

Environment note: managed sandbox exposes a synthetic `/tmp/.git` directory,
which aliases unrelated temporary test projects. Full Rust tests use
`TMPDIR=/var/tmp` outside that synthetic tree; no product/baseline workaround.
No golden or performance baselines changed. Merging remains a human action.


2026-11-11 — requested tooling/UI follow-up

Task W5: complete (root pnpm workspace, exact Node 24.22.3 and pnpm 12.16.1,
strict 14-day supply-chain policy, Biome 2.6.3 and native workflow migration).
Ruling: fresh security review APPROVED. Independent negative probes rejected
fresh resolution, a fresh frozen-lock entry and an unapproved postinstall; its
marker was not created. Clean frozen install verified 145 lock entries. No
dependency scripts ran. Node archive SHA-256 verified against official sums.
Node is the sole user-approved security cooldown exception. Formatting,
typecheck, unit test and build passed. Audit: zero moderate/high/critical; one
unpatched LOW development-only deepmerge advisory explicitly documented.

Task W6: complete (Lucide SVG icons and pinned shadcn-svelte source primitives,
existing sage theme and native select behavior, embedded third-party notices).
Ruling: fresh UI review APPROVED; source semantics, native form behavior,
component provenance, licenses and CSP checked. Desktop/mobile captures inspected.
Seven real-daemon browser scenarios passed with Node 24.22.3 and pnpm; desktop
and mobile evidence refreshed. Biome's experimental Svelte support was exercised
on the actual application and checked by svelte-check, compile and browser tests.
Official Svelte/shadcn AI docs, reviewed skills and optional docs-only MCP setup
are recorded in docs/AI-TOOLING.md; no integration is implicitly activated.

Windows CI's previous generated-contract failure was solely CRLF checkout
conversion. Frontend source attributes now enforce LF, preserving exact contract
comparison and consistent formatting on all native runners. Final CI results
remain recorded in PR #126 checks.
