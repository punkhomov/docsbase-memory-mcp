# CF8a — ANN search policy at the library boundary

Execution is inline; fresh task and final reviews follow AGENTS.md. Original draft on 84fc45f, integrated on main
af0aeab with Rust 1.100.0. New crates: none. This is a bounded part of CF8,
not completion of CF8/#78 or activation of source-format v2.

## Contract and alternatives

Move I33's existing base64, cap4096 and multiplier4 to an ungated component owner.
`AnnSearchSpec` resolves project-scoped typed patches and field provenance through
`Resolution`; no TOML/CLI/server setting is introduced. `AnnSearchSettings` is
Copy with private fields and a checked constructor. Each overlay validates its candidate before committing settings and origins,
including direct calls to the public SettingsSpec trait; partial patches must
leave a valid policy at every layer boundary. Defaults are computed
without allocations at the existing per-query compatibility boundary.

The explicit path accepts 1 <= ef_base <= ef_max <= 4096 and a positive
fetch_multiplier. 4096 is the existing qualified search-beam ceiling, not a new
runtime default or an RSS budget. Expanding that domain needs separate resource
qualification. Saturating multiplication followed by the cap cannot wrap.
`beam_for(fetch)` rejects fetch > ef_max before entering HNSW; for admitted fetch,
max(ef_base, fetch * fetch_multiplier), capped at ef_max, is always >= fetch.
Zero fetch returns an empty result after the existing vector validation.

The pinned [hnsw_rs 0.3.5 implementation](https://docs.rs/crate/hnsw_rs/0.3.5/source/src/hnsw.rs)
uses `ef_arg.max(knbn)` in search/search_filter (lines 1459 and 1528); search_layer allocates two heaps with initial
capacity ef.max(2) and has a visited map. A wrapper-only cap therefore is not a
hard limit when fetch exceeds it, and bounded beam is not bounded total query
memory. The legacy `AnnIndex::search` keeps that high-fetch behavior deliberately;
its documentation must state the limitation. `search_with_settings` uses an
immutable borrowed policy per call with no policy publication, locks or new cache.

Rejected alternatives: silently rejecting legacy high-fetch calls changes the
existing API; accepting arbitrary usize beams can request pathological dependency
allocations; resolving provenance on every legacy query adds needless allocations;
changing graph recipes for search-only policy rebuilds unrelated artifacts.

## Interfaces and ownership

- `hybrid::ann_settings::{AnnSearchSettings, AnnSearchPatch, AnnSearchSpec}`.
- `AnnSearchSettings::new(ef_base, ef_max, fetch_multiplier) -> Result<Self>`;
  getters and `beam_for(fetch) -> Result<usize>`.
- Fields: `ann.search.ef_base`, `ann.search.ef_max`,
  `ann.search.fetch_multiplier`; Scope::Project and Unit::Count.
- `AnnIndex::search_with_settings(query, fetch, &AnnSearchSettings)` uses the
  same built/loaded HNSW and result validation/order as the existing search.
- Default owner values have one definition; legacy `ef_for` delegates to them.
- ComponentImpact is runtime-only: unchanged / initialize / reconfigure. No
  graph/key/meta/vector/query-embedding identity change and no invalidation action.

## Task CF8a

1. Add `tests/ann_settings.rs`: owner defaults and layered project provenance;
   invalid zero/order/domain values and saturation; runtime-only impact; actual
   built and persisted graph queries, over-cap admission, legacy high-fetch,
   zero-fetch/vector errors and unchanged persisted bytes. Observe API RED.
2. Add `src/hybrid/ann_settings.rs`, export it, route only the compatibility beam
   and additive explicit API in `ann.rs`. Preserve all old ef expectations.
3. Run focused tests full/lean, causal bounded-fetch mutation with small fetch,
   existing ANN suites and frozen search gate. Restore every mutation.
4. Run official and lean suites, Clippy/fmt/diff; append dated spec deltas and
   progress evidence. No baseline/threshold/ignore/lockfile changes.
5. Fresh task/final reviews and fixes, ready PR and required native CI with test
   inventory audit. Human merge only per current AGENTS.md.

Remaining CF8 work: graph construction policy and qualified estimator domain,
versioned recipe/meta/provenance, dense construction composition. Later runtime
configuration must publish request-consistent snapshots before policy adoption;
this library-only additive slice does not claim those capabilities.
