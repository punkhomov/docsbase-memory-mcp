# CF4c2: observed migration planning

> **For agentic workers:** use superpowers:executing-plans inline; no implementation delegation. Fresh task and final reviews per AGENTS.md.

**Goal:** `config migrate --to 2 --format=json` surveys real registered source files without writes and returns a bounded, redacted migration plan.
**Architecture:** A planning owner admits explicit limits and paths, captures CF4c1 registry, reads each registered source through no-follow file handles, then checks registry again. It feeds complete observations into CF3 and replaces supplied-text preconditions with actual Missing/KnownIdentity states in CF4b. The envelope identity binds context, registry, all source states, explicit choice and proposed actions. Raw edits never cross the display boundary.
**Tech Stack:** Rust1.88, std, existing serde/serde_json/blake3/clap/rusqlite. No new crates.
**Spec:** requirements/design Delta 2026-11-12 CF4c2 below; CF3/CF4b/CF4c1.

## Global constraints

No runtime activation, provider construction/request, download, config locks, DB migration/recovery, file creation or rewriting. Default source build stays offline; official cli-full. No baseline/threshold changes or production unwrap/expect/panic. No atomic multi-file snapshot claim. Every future apply must reobserve registry/context and source bytes/existence, including unchanged inputs. Rig/web do not block this slice.

## Decisions and alternatives

- Registry, not cwd or active sessions, defines project inventory, including missing project roots. Missing registry is observed empty; unavailable registry is incomplete. A second unequal/unavailable observation blocks proposals, without retry loops.
- Missing global uses synthetic empty text for defaults only; external precondition remains Missing. Existing empty source is KnownIdentity(hash(empty)). Never serialize the synthetic hash as existing global.
- Regular UTF-8 source files only; final symlinks/reparse points, directories and FIFOs are unavailable. Parent directory aliases are resolved/rechecked; sources are independently observed, with metadata checks around each bounded read. No strict wall-clock/RSS/hostile-filesystem snapshot guarantee.
- CLI uses one explicit `PlanningLimits::standard()` owner policy: 256 projects, 1 MiB registry/path metadata, 16 MiB total source bytes, 8 MiB plan/output; individual sources retain MAX_STATE_BYTES (1 MiB), SQLite values 1 MiB and soft page cache 256 KiB. API accepts explicit alternate limits. Limits are admission bounds, not hidden search defaults or new config knobs.
- Aggregate source admission happens before parsing/retention. Draft output is bounded by project count and per-source limits and checked against aggregate bytes; JSON serialization uses bounded writer before stdout. Limits don't claim exact allocator RSS.
- Display only paths, source states/revisions, numeric budgets/changes, finite warnings, version transitions and proposed byte hashes. No arbitrary source values, raw TOML, comments, provider parameters, models or URLs. This sacrifices textual diffs for a safe default; explicit raw export is not part of this slice.
- Complete observed source inputs remove SourceObservationRequired, but strict-store/executor blockers remain. Invalid/unavailable inputs block all rewrites, not a partial migration. Explicit `--vector-max-bytes N` and `--accept-suggested-vector-budget` conflict; neither implies consent.
- Output exit0 means a plan was produced, including a blocked plan; malformed arguments/context/limit admission errors exit nonzero with redacted errors. Bare config/effective behavior stays unchanged.

## Task CF4c2 — observer and usable CLI

Files: new `src/config/observe.rs`, `src/config/observe/{source,view}.rs`, `tests/config_observation.rs`; modify `src/config/mod.rs`, `src/cli/{config,mod}.rs`, product docs and dated specs/ledger.

Interfaces:
```rust
pub struct PlanningLimits { /* explicit registry/source/output limits */ }
impl PlanningLimits { pub const fn standard() -> Self; }
pub fn migration_plan(config_root: &Path, cache_root: &Path,
    db: &Resolved<DbSettings>, limits: PlanningLimits,
    decision: Option<VectorBudgetDecision>) -> Result<MigrationPlan>;
impl MigrationPlan { pub fn to_json(&self, max_bytes: usize) -> Result<String>; }
```

- [x] RED: real CLI missing-root test initially rejects unknown migrate command; assert JSON actual missing state, no created directories, explicit choice produces blocked rewrite with Missing precondition. API tests then pin persisted multi-project inventory, missing/empty distinction, secret canaries, context-sensitive identity, unchanged source identity, unavailable store/source and explicit bounds.
- [x] GREEN: implement source observer and planning owner, safe versioned view/identity, CLI args and existing Db settings propagation. Capture roots once, resolve relative roots against captured cwd without creating paths. Feed only complete observations to CF3.
- [x] Adversarial tests: invalid UTF-8/TOML, overlarge and aggregate caps, directory/symlink/FIFO source; changed source/registry via private deterministic hooks. V2 preview independent of operational v1 loader; exact bytes/namespace snapshots prove no writes.
- [x] Verify focused tests, full cli-full suite/clippy, lean focused tests, fmt; fresh task review and fixes, ledger task commit.
- [ ] Fresh whole-branch final review, native docs/linux/macos/windows CI and test inventory audit; ready PR with explicit migration limitations. Respect latest user authorization for integration.
