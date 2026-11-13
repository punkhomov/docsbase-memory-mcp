# Tasks

- [x] Verify stable policy semantics and pin toolchains/CI.
- [x] Regenerate locks and independently verify every registry entry.
- [x] Add fail-closed CI audit and regression tests.
- [x] Complete local fmt/Clippy/functional validation and independent review.
- [x] Resolve combined integration, test it and document merge order.

External release gate: final-head docs/linux/macos/windows checks must be green
before either PR is merged. Live status belongs to PR #126/#130 and Actions,
not a static checkbox; no merge into main is performed by this task.
