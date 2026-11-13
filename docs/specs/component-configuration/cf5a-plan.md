# CF5a: deterministic consent policy and plan-bound decisions

> Execution: superpowers:executing-plans, inline; fresh task/final reviewers.

**Goal:** implement the pure consent part of FR-13/14 without claiming runtime
activation or an executor. **Architecture:** the controlling caller captures
entrypoint/TTY once; `PolicyContext` resolves legacy/managed policy, and an immutable
`ConsentReview` owns a bounded compiled plan. Receipts record consent for that
exact canonical identity. **Tech:** Rust1.100.0 stable, existing crates only.
**Spec:** requirements FR-12/13/14/15 and design section Plan, policy and errors.

## Constraints and boundary

No production unwrap/expect/panic, ambient environment/terminal reads, filesystem,
stdin, daemon/provider construction, downloads, new crates, Cargo.lock or frozen
baseline changes. cli-full and lean remain offline. Independent native Linux,
macOS and Windows inventories retain all parent tests and ignores. User-authorized
squash delivery follows reviewed green required checks.

This is library foundation, not operational CF5 completion. No CLI flags, new
exit behavior in existing commands, source writes, v2 activation, adoption,
live apply or clearing planner blockers. The compiler still adds
ExecutorUnavailable to every action; consent cannot remove it. Actual apply must
reobserve state under its owning admission, revalidate receipt identity, validate
execution capability/blockers and publish using the future recovery contract.

## Decisions

- `PolicyMode::LegacyV1` plus no explicit policy resolves `PolicyContext::Legacy`;
  it does not reinterpret existing startup. An explicit policy or Managed mode
  resolves once to a private-context `ResolvedPolicy`.
- Auto is Prompt only for Entrypoint::Cli with TTY; otherwise Fail. Explicit
  Prompt does not turn service/MCP/autostart/Standalone into a controlling CLI,
  including inherited TTY. Explicit Apply cannot grant those loops rebuild rights.
- Every plan is compiled from owner Assessments using caller-supplied PlanLimits;
  `ConsentReview` has no mutable plan accessor. No unvalidated caller-mutated
  ChangePlan is accepted. The existing compiler owns canonicalization and limits.
- No actions, InitializeRuntime, ReconfigureRuntime and InitializeStore require
  no consent only when they require no grants. First creation must carry Missing
  observation; other actions require consent. No-grant initialization can be
  consent-satisfied for all modes; blockers still prevent execution.
- Fail rejects a required change with code3, regardless of grants. Apply on a
  controlling CLI requires every owner-declared Remote/Downloads/Destructive
  grant; all other consent refusals use code5. No ambient grant defaults exist.
- Prompt on controlling CLI+TTY yields an opaque PromptRequest without performing
  I/O. Answering binds the displayed plan ID, explicit accept/decline and additional
  grants. Base and additional grants are unioned; each permission is independent.
- An answer for a different plan or a receipt checked against a different freshly
  compiled review is stale/code4. Consent receipts are not execution tokens and
  do not represent observed filesystem freshness. Revalidation is a comparison
  against the newly observed owner assessments supplied by the caller.
- Decision names explicitly separate ConsentSatisfied, PromptRequired and Refused;
  only Refusal has an exit code. Neither consent success nor PromptRequired implies
  successful apply or code0. Invalid plans remain normal Result errors.

## Files

- Create `src/config/policy.rs`: PolicyMode/InvalidationPolicy/Policy/PolicyContext,
  ResolvedPolicy, explicit GrantSet, ConsentReview, ConsentDecision,
  PromptRequest, ConsentReceipt and ConsentRefusal.
- Modify `src/config/mod.rs`: export policy module only.
- Create `tests/config_policy.rs`: actual SearchSpec/DbSpec assessments, actual
  migration plans, bounded compiler, literal launch matrix and consent outcomes.
- Dated deltas in requirements/design/tasks, docs/CONFIG and progress ledger.
  No daemon/CLI/watcher edits: Agent B owns current #79 integration.

## Task CF5a

- [x] RED: missing API integration target. Run cli-full/lean config_policy.
- [x] Legacy/no-flags, Auto launch matrix for all5entrypoints with/without TTY,
  explicit overrides. Use literal expected enum values, not resolution helpers.
- [x] Real runtime-owner initialization/reconfiguration vs Rebuild/RewriteSource/
  recovery/eviction classes; all policies treat first no-grant creation as
  consent-free, without clearing original blockers or changing input assessments.
- [x] Independent grants, fail precedence, noninteractive/loop refusal, accepted/
  declined/pending prompt, wrong displayed ID and changed-observation receipt.
  For example:
  ```rust
  let review = ConsentReview::compile(&[assessment], limits()).unwrap();
  assert_eq!(review.evaluate(policy, GrantSet::empty()).refusal(),
      Some(ConsentRefusal::RequiredChange));
  ```
- [x] Reject invalid/over-limit inputs through actual compiler. Malformed DAG,
  unknown observation/recovery blockers and legacy migration blockers retain
  their existing meaning; consent never exposes source values or provider secrets.
- [x] Implement minimal pure policy, run focused full/lean suites, then complete
  cli-full and lean suite, full/lean all-target Clippy/fmt/diff. Mutation-check
  TTY/entrypoint, grant union and plan-binding branches with causal RED/GREEN.
- [x] Fresh task reviewer; fix Important/Critical and rerun affected checks.
  Record Task CF5a complete + rulings/minors in progress before commit.
- [ ] Fresh final review, required docs/linux/macos/windows, downloaded inventories,
  PR and verified final tree. Keep CF5/CF7/#78 open and record remaining execution
  work explicitly. Integrate completed watcher PR148 before final native delivery.

## Remaining CF5 steps

CF5b must specify stopped/online admission and persisted adoption recovery before
writing any source: one store-wide transaction cannot atomically rewrite several
files. CF5c may connect accepted local runtime/resource actions to controlling CLI
only with implemented capability checks and stale reobservation. Strict startup
requires persistent owner admission and all registered/external v1 conflict rules;
neither this foundation nor a receipt switches SourcePreview activation on.

### Validation repair and integration

Full lean exposed the predecessor CF7c2 admission child's inherited
DOCSBASE_CONFIG_DIR. A controlled malformed global config reproduced the failure;
its own empty TempDir now replaces that variable in the child only. Existing
provider/key isolation and admission/no-enqueue assertions are unchanged. The
4-line test-only `src/daemon/project_state_tests.rs` correction is included in
this task's validation; no production daemon ownership is changed.

Origin/main advanced to PR148 squash6826aaf. Integrated before review/delivery;
4append-only spec conflicts were resolved only after verifying both sides retain
the exact ancestor prefix. Both complete suffixes preserved; implementation patch
relative to updated main is unchanged. Local suite evidence below predates this
upstream integration; focused policy/actual Shared and native checks cover the
integrated tree. The first combined --lib/--test command filtered the policy
integration target to0; separate unfiltered policy commands are required evidence.
