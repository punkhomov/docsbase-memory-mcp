# CF6b: dense construction policy and backfill eligibility

Execute inline with superpowers:executing-plans; no implementation delegation.
Fresh task/final review follows AGENTS.md.

## Scope and architecture

Consume the existing DenseSpec resident/vector-count/project-count/minimum-vector/
backoff/disk settings in DenseCache, daemon construction and direct CLI. The owner
remains dense_settings.rs; compatibility DenseCacheLimits::default projects the
same owner's values. Query policy delivered by CF6a travels with the same complete
DenseSettings at composition boundaries. Existing constructors remain adapters.

Legacy Config hybrid.cache.max_bytes still overrides the dense-path vector-cache
fallback at daemon/direct composition and per-project admission, as before. This
is not strict-store activation or a new vector eviction mode. No TOML keys,
source writes, live mutation/shrink, model requests/downloads, algorithm recipes,
baselines or thresholds change. Existing zero dense limits retain their meanings;
query capacities remain positive. Backfill pin TTL remains its existing owner task.

One pure eligibility predicate on DenseCacheLimits covers vector count, minimum
count, nonzero project capacity, model width and graph+exact peak bytes. The actual
builder and backfill use it. Eligibility is not a memory reservation: existing
live-build/resident accounting and admission continue to enforce the shared cap.

Intentional correction: backfill must not pin a corpus that fails this policy.
Small corpora no longer take temporary ANN pins before embedding. Eligible duplicate
texts are judged by chunk-row count both before embedding and when renewing pins,
not by distinct hash count. Peak arithmetic fails closed on overflow, even when
the explicit byte cap is u64::MAX. The debug process-global ANN-cap override is replaced
by explicit per-call policy, eliminating cross-test/process hidden state.

## Interfaces and ownership

- dense_settings::standalone() -> Result<Resolved<DenseSettings>>.
- DenseCacheLimits: From<&DenseSettings>; owner defaults; pure
  ann_build_eligible(count, dim) / ann_build_eligible_for_model(count, model).
- DenseCache::with_settings(policy, root, &DenseSettings) -> Result<DenseCache>;
  limits() returns a copy used by production backfill. No extra Arc/lock.
- Shared::open_with_dense_policy(cache, db, pool, &DenseSettings) and lifecycle
  run_daemon_with_dense_policy(...); older APIs adapt omitted owner settings.
- CLI resolves DenseSettings once; direct cache factory preserves legacy vector
  budget precedence while consuming supplied resident/query settings.
- Explicit jobs helpers carry DenseCacheLimits from Shared through synchronous
  and background sync paths; compatibility helpers resolve owner defaults once.
  No per-request independent default in the production backfill path.

## Task CF6b

- [x] RED: owner projection and nondefault actual cache behavior; explicit per-call
  lease probe rejects byte/vector-count/minimum/project-cap ineligible corpora,
  admits eligible duplicate chunk rows, retains their pins after success, and
  keeps independent policies isolated. Existing default zero semantics remain.
- [x] GREEN: owner projections/default adapters, explicit daemon/lifecycle/CLI
  propagation, shared builder/backfill eligibility and removal of debug override.
- [x] Acceptance: real metadata/vector cache and counting embedder; daemon/direct
  propagation; invalid query policy fails before resources; existing gated worker
  reservation/invalidation and ANN load/build/cancellation regressions pass.
- [x] Verify Rust 1.100.0 cli-full and lean, full/lean Clippy, fmt, unchanged frozen
  benchmark; fresh task review, fixes and progress ledger commit.
- [ ] Delivery: whole-branch fresh final review, required native CI and inventory
  audit, authorized squash merge. Broader CF6 live shrink and CF7 stay open.
