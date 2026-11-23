# Design

Reuse daemon::remote::with_sync_command for the three manual hybrid_live entries.
Construct clients, index and search inside a single owned command scope, following
search_bench_live. Mocks/local inference and intentionally rejected transport
calls retain their current contexts. No new crate, runtime owner or provider.

Tracker updates are additive dated checkpoints grounded in merged PRs/specs.
macOS diagnostics are test/CI-only and bounded; system evidence is corroboration,
not an inferred cause. Never infer a signing failure from signal9 alone.
