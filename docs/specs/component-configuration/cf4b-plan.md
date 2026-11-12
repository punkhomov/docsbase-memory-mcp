# CF4b: bounded pure change plans

> **For agentic workers:** use superpowers:executing-plans inline; fresh task/final
> review follows AGENTS.md. No implementation delegation.

**Goal:** проверяемый pure ChangePlan для Search/Db policy, metadata schema facts
и CF3 migration draft, без нового CLI или исполнения действий.
**Architecture:** owners производят assessments; compiler только проверяет,
канонизирует и топологически сортирует DAG. Все actions пока blocked:
executor отсутствует. Никакого центрального invalidation switch по Config.
**Tech Stack:** Rust 1.88, existing serde/serde_json/blake3/std, no new crates.
**Spec:** requirements.md FR-2/9/10/11/12, NFR-3; design.md CF4b delta.

## Global Constraints

- Rust 1.88, Linux/macOS/Windows, cli-full; lean default offline.
- No production unwrap/expect/panic, no new dependencies, no baseline changes.
- No provider/network/download/DB writes; pure compiler accepts borrowed inputs.
- Current runtime behavior and source loader remain unchanged.

## Task CF4b — graph and real owner assessments

**Files:** new `src/config/change.rs`, `src/config/change/compile.rs`, `src/config/change/path.rs`,
`tests/config_plan.rs`; modify `src/config/{mod,schema,migrate}.rs`,
`src/search/settings.rs`, `src/store/{mod,settings}.rs`, new `src/store/plan.rs` and dated specs/ledger.

**Interfaces:**

```rust
pub fn compile(assessments: &[Assessment], limits: PlanLimits) -> Result<ChangePlan>;
pub struct PlanLimits {
    pub max_actions: usize,
    pub max_dependencies: usize,
    pub max_metadata_bytes: usize,
}
// Each ResourceId has at most one Observation and one Action after merging.
// ResourceId = Runtime(String) | Artifact(ArtifactId) | Source(SourceId).
// ExpectedState = Missing | Unknown | NeedsRecovery | KnownIdentity(String)
//               | SuppliedSourceRevision(String).
// Action = resource, effect, target, reasons, dependencies, required_grants.
// Target = Identity(String) | VectorBudgetBytes(u64) | MetadataSchemaVersion(u32).
// Assessment = expected, actions, blockers.
impl SearchSpec {
    pub fn assess_change(old: Option<&SearchSettings>, next: &SearchSettings)
        -> Result<Assessment>;
}
impl DbSpec {
    pub fn assess_change(old: Option<&DbSettings>, next: &DbSettings)
        -> Result<Assessment>;
    pub fn assess_schema(facts: &ArtifactFacts) -> Result<Assessment>;
}
impl MigrationDraft {
    pub fn assess_change(&self) -> Result<Assessment>;
}
```

- [x] RED: add integration tests and run `cargo test --locked --test config_plan`.
  Missing module/API is the initial expected failure. Core assertions:

```rust
let old = search::settings::standalone()?.into_settings();
let mut next = old.clone();
next.request_timeout_ms += 1;
let p = compile(&[SearchSpec::assess_change(Some(&old), &next)?], limits())?;
assert_eq!(p.body.actions.len(), 1);
assert_eq!(p.body.actions[0].effect, Effect::ReconfigureRuntime);
assert!(matches!(p.body.actions[0].resource, ResourceId::Runtime(_)));
assert!(!p.body.blockers.is_empty()); // no executor in this slice
```

- [x] Implement graph: BTreeMap keyed by resource, merge identical core actions
  with union of reasons/dependencies; reject competing effects/targets/grants,
  inconsistent observations, missing observations/dependencies and cycles.
  Kahn ordering uses sorted ready set, not recursive traversal. Every action
  gets ExecutorUnavailable; grants are requirements only, never consent.
  Evict/DeleteStore/Rebuild are distinct and cannot share a resource in one plan.
- [x] Bound raw actions/edges before cloning; stream borrowed JSON through a
  bounded writer before building indexes. Input and final output each must fit
  max_metadata_bytes. Canonical body digest domain `docsbase.change-plan.v1`,
  with sorted reasons/dependencies/blockers/observations and plan_version=1;
  plan_id excluded from digest, included in final size admission. Limits have
  no defaults and are not hashed as state. Empty/unchanged plans remain explicit.
- [x] Search/Db owners validate old/next, use existing impact, fingerprint ONLY
  their typed policy values under different v1 domains. Db duration fingerprint
  uses seconds+nanos (preserves sub-ms typed inputs). No artifact invalidation
  follows a timeout change. Known old policy is caller-supplied, not a live probe.
- [x] Schema owner consumes exact metadata_db_schema artifact only; missing
  observation blocks, Missing creates InitializeStore, NeedsRecovery creates
  RecoverStore, Unknown blocks. Recognize only canonical sqlite-schema-v1:V:C
  with integer version/cookie: current schema is unchanged header identity;
  older schema proposes blocked MigrateSchema; future/unknown format blocks.
  No ready rows/index/generation claims.
- [x] Migration owner rejects unsupported draft version and missing selected cap
  in complete draft. Incomplete drafts expose all blockers even with zero edits.
  Complete drafts propose blocked AdoptSharedStore; each RewriteSource depends
  on it. Bind before/after exact revisions without outputting source text;
  hash mismatch rejects. Source revisions are explicitly SUPPLIED, not observed
  file existence; every rewrite requires SourceObservationRequired. Strict store
  adoption remains blocked regardless of runtime_activation_available input.
- [x] GREEN regression coverage: deterministic input permutations/duplicates;
  dependency order; conflicting targets/effects/facts; cycle/missing nodes;
  versioned identity changes on state/target/blocker; exact boundaries for raw
  action/edge/JSON budgets; unchanged/init/reconfigure real owners and invalid
  settings; real MigrationDraft complete/incomplete; real filesystem snapshots
  for missing/schema mismatch/WAL/stray sidecar with inspect_readonly.
- [x] Run targeted tests, full cli-full, lean targeted + all-target clippy, fmt.
- [ ] Delivery:
  Append dated deltas and ledger; fresh task review, fix, commit; fresh final
  review; PR and all four required native checks. Apply earlier user merge
  authorization only after reviewed head and tested tree match.

## Self-review / explicit remainder

This covers graph validation plus three actual owner families. CLI plan/migrate,
full registry inspection, active daemon revision, actual apply, vector semantic
fingerprints and ANN dependency propagation remain later CF4/CF5/CF7/CF8/CF11
slices. The serializer's metadata cap bounds logical bytes, not allocator RSS.
No claim that CF4 or #78 is complete. Supplied source hashes are stale-source
tokens from CF3; no provider credential/model fingerprint is introduced.

### Review correction acceptance

- Source path component serialization follows Path Eq/Ord including parsed native
  Windows prefixes; preserve parent/verbatim components, no filesystem calls.
- All resource references use the same path encoding; input permutation, duplicate
  aliases and single-alias substitution preserve plan JSON/ID.
- Charge raw source-path bytes separately before cloning; canonical shrinking
  cannot bypass metadata limits. Exact raw action/edge/input JSON boundaries
  accepted, one less rejected. Unsupported draft version tested on valid draft;
  complete draft with missing selected cap rejected independently.
