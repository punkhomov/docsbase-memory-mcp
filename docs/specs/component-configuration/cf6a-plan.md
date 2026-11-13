# CF6a: explicit query-cache policy adoption

> **For agentic workers:** use superpowers:executing-plans inline. No implementation delegation; fresh task/final review per AGENTS.md.

**Goal:** consume the existing DenseSpec query-cache limits in the actual shared cache, daemon and direct CLI.
**Architecture:** DenseSpec keeps sole ownership of defaults/typed overrides; a QueryCacheSettings projection supplies complete entry/byte policy. QueryCache owns that immutable policy with its shared state. Explicit constructors validate before resource creation; legacy adapters obtain owner defaults. CLI resolves once and passes policy through lifecycle and direct search.
**Tech Stack:** Rust 1.100.0 stable, existing std/serde/resolver; cli-full includes Web assets built with pinned Node/pnpm.
**Spec:** requirements/design/tasks Delta 2026-11-13 CF6a; inventory I30.

## Global constraints

- Rust 1.100.0 exact stable, Linux/macOS/Windows, official cli-full and lean offline.
- No new crates, production unwrap/expect/panic, hidden runtime fallbacks or baseline/threshold changes.
- Current default query capacity remains 256 entries/4MiB. Model/query cache identity, ranking and singleflight behavior remain the existing contracts.
- No new TOML keys or live settings updates in this slice. CF5/CF7 adoption and broader CF6 resource accounting remain separate gates.
- Cache byte accounting covers retained vector payload; externally held Arc clones, keys/allocator metadata and running requests have their existing separate ownership. No new RSS or total-lifetime bound is claimed.

## Design decisions

The existing dense_settings.rs already defines query_cache_max_entries/bytes, so do not add a second SettingsSpec/default catalog. Add QueryCacheSettings{max_entries,max_bytes} projection and positive validation in that owner. DenseSpec's query fields gain actual consumers; other dense settings retain their recorded owning tasks. Compatibility constants in dense.rs alias the owner's constants.

The policy lives inside QueryCache beside entries/order/bytes/flights. sharing_query_cache shares the policy and data together. New cache construction accepts a complete explicit policy. Query insertion/eviction reads it under the existing mutex; no per-request cap resolution, added atomics or second independent global budget.

CF6a precedes configurable-apply CF5 because it adds typed propagation at existing construction boundaries only. No source activation, consent bypass, dynamic shrink, source migration or persistence invalidation is introduced. Full resident/backfill/ANN policy must later be propagated together (jobs::ann_max_bytes remains that slice's responsibility).

## Task CF6a — query policy and real production propagation

Files: modify src/hybrid/{dense_settings,dense}.rs, src/cli/{mod,search}.rs,
src/daemon/{lifecycle,server}.rs; new tests/query_cache_settings.rs; dated specs,
progress and configuration docs. Do not add a new query owner module.

Interfaces:
```rust
pub struct QueryCacheSettings { pub max_entries: usize, pub max_bytes: u64 }
impl QueryCacheSettings { pub fn validate(&self) -> Result<()>; }
impl DenseSettings { pub fn query_cache_settings(&self) -> QueryCacheSettings; }
pub fn standalone_query_cache() -> Result<QueryCacheSettings>;
// DenseCache explicit checked constructor; existing public signatures remain adapters.
pub fn with_policy_limits_and_query_settings(policy: AnnPolicy, cache: &Path,
    limits: DenseCacheLimits, query: &QueryCacheSettings) -> Result<DenseCache>;
// Shared and lifecycle explicit checked construction; old variants are adapters.
pub fn open_with_resource_policies(cache: &Path, db: &Resolved<DbSettings>,
    pool: &Resolved<ReadPoolSettings>, query: &QueryCacheSettings) -> Result<Arc<Shared>>;
pub fn run_daemon_with_resource_policies(cache: &Path, grace: Duration,
    db: &Resolved<DbSettings>, pool: &Resolved<ReadPoolSettings>,
    query: &QueryCacheSettings) -> Result<()>;
```

CLI top-level resolves `standalone_query_cache()` once. Direct search's explicit
`run_with_policies` and daemon launch consume it. Compatibility entrypoints resolve
omitted policies once at their composition boundary. Shared/lifecycle reject
invalid imported policy before DB/state/lease creation. Direct CLI uses a small
production constructor helper to make propagation independently observable.

- [x] RED: tests/query_cache_settings.rs compiles against missing APIs and demonstrates explicit 2-entry/16-byte LRU/byte eviction, oversized bypass and shared policy. Query typed resolution rejects 0 fields and project scope. Add actual Shared and direct CLI factory propagation tests and invalid-before-filesystem unit tests at their owning seams.
- [x] GREEN: implement owner projection/validation, shared policy, checked explicit constructor; compatibility adapters retain defaults. Propagate through CLI/lifecycle/Shared without per-consumer defaults.
- [x] Acceptance: concurrent cache clones share one cap, successful cached query skips a counting embedder, oversized completed vector is served but not retained, existing singleflight tests remain unchanged. Assert actual stats/hits/eviction rather than only DTO values.
- [x] Verify focused full/lean suites, cli-full all-target Clippy/fmt and frozen benchmark; fresh task review/fixes and ledger commit.
- [ ] Delivery: fresh whole-branch final review, native docs/linux/macos/windows gates and executed-test inventory audit; integrate using existing user authorization.
