# Synchronous remote caller audit

Baseline069ff64 (#168), 2026-11-24. Searched tracked Rust sources, tests,
examples and benches for ReqwestTransport::new, OpenRouter constructors,
from_config, provider probes, blocking::enter and with_sync_command; followed
aliases/factories through their callers, rather than treating every marker as a bug.

| Path | Finding |
|---|---|
| tests/hybrid_live.rs, three ignored entries | Missing runtime. Smoke had only a marker; two golden measurements constructed legacy stages/index jobs without an owner. All construction/index/search now runs in one with_sync_command scope. |
| tests/search_bench_live.rs, three ignored entries | Already fixed by #168; same owning scope covers construction and final call. |
| tests/hybrid_openrouter.rs; hybrid_jobs/config; representation_matrix | Injected transport/fake stages, disabled paths or validation/local-provider tests. Do not require a network driver. |
| tests/hybrid_async.rs transport-drop test | Deliberately constructs and drops a runtime-free client; never executes sync HTTP. |
| src/daemon/remote.rs retired transport test | Deliberately rejects calls after its owner retires. Keep rejection coverage. |
| src/cli/mod.rs → models::run/connect | Existing with_sync_command covers provider probes and real construction. |
| src/cli/command.rs; daemon/server.rs | Operational remote factories receive RemoteExecution; legacy Shared composition explicitly supplies its bridge. Feature-off branches cannot create remote clients. |
| src/hybrid/jobs.rs, provider.rs, openrouter.rs; index/job.rs | Legacy library helpers inherit the caller's bridge. Manual golden index calls are inside the repaired scope; operational jobs use prepared/async stages. No new ambient runtime introduced. |
| local inference / mocks in daemon/tools/search_async, index/async_job, dense | Blocking markers serve CPU/local or injected stage execution, not missing ownership of real remote HTTP. |

Regression uses the real synchronous Reqwest POST with a malformed URL: the old
marker gives Admission("...explicit command runtime"), the owned scope reaches
typed Reqwest builder validation without network or credentials. All three live
entries stay ignored and preserve their original keys, models, cases and metrics.
This does not claim live provider quality/availability was measured.
