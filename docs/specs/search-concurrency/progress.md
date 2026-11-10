# Search concurrency progress — #66

2026-11-10: S1 design ready; S2–S4 explicitly require detailed dated design gates.
Ruling: user authorized autonomous current-queue work including PR/merge; machine
mode retains fresh task/final reviews and latest-head four required checks.
Ruling: split #66 into safe increments. S1 prepares bounded readers but leaves
search routing unchanged; singleflight precedes concurrency. #66 stays open.
Design challenge: default acquire timeout would violate CF0; acquire therefore
requires caller absolute deadline and owns no timeout default. Startup eagerly
prepares all readers and fails before serving if preparation fails.

Task S1: implementation verified (base6e14280; cli-full958 passed/0failed/13ignored
across65targets; lean integrations30/0, pool units3/0, startup3/0; full/lean
all-target Clippy, opt-in lint, fmt and diff checks clean). Fresh task review pending.
Ruling: ReadLease field order unlocks its slot before ReturnSlot advertises
availability; no second unbounded mutex wait after deadline-aware admission.
Ruling: Shared reader preparation occurs before listener/state advertisement.
Validation ruling: an intermediate local full run is excluded: concurrent lean
build replaced target/debug/docsbase during full CLI tests (missing local-model
commands). Profiles were rerun sequentially; final sole cli-full run passed.

Task S1: complete (base6e14280; verification above; fresh task review
Spec compliance PASS / Quality APPROVED, no findings).
Ruling: cleanup-failure quarantine is source-verified defensive behavior; real
SQLite tests cover successful rollback/unwind cleanup. No driver-failure injection
or native result is claimed. Exact command/exit observations retained in session
evidence; final-head native CI remains a delivery gate.
