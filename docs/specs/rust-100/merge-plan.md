# Merge plan — Rust 1.100, Web and Rig

2026-11-13. Both PR branches were rebased onto main 7baf744. Web's CONFIG.md
conflict with main was resolved by preserving both the configuration APIs and
Web documentation. Both manifests now require Rust1.100.0; both locks were
regenerated under the stable 14-day deny policy and independently audited.

## Prepared integration

`integration/rust100-web-rig` is a prepared result, not a merge into main.
Its parent is the updated Web branch; its delta adds the Rig feature on top.
The independent branches produce conflicts in 12 files: five workflows,
Cargo.toml, Cargo.lock, README.md, README.ru.md, docs/INSTALL.md,
docs/specs/constitution.md, docs/supply-chain.md. Runtime sources merge cleanly.

Resolution retains Web build/frontend steps and documentation, adds Rig to
the lean dependency boundary, combines both manifest dependency sets, and
regenerates the combined lock rather than choosing either old lock. Both
feature constitution deltas precede the superseding Rust1.100 delta. Web source,
frontend files and build.rs are unchanged by integration.

Local combined validation: 1313 tests passed, zero failed, 18 existing ignored;
fmt, cli-full Clippy, actionlint and age audit pass (490 registry entries).
Independent integration review approved with no code findings. Native results
are recorded in GitHub Actions for the integration branch; latest PR-head
checks remain mandatory before merging either PR.

## Order

1. Merge Web PR #126 first (human action).
2. Bring the prepared Rig delta from `integration/rust100-web-rig` onto the
   resulting main and update PR #130. Use the tested Web parent as the old
   upstream when rebasing this delta; do not replay the already-integrated Web
   commits after a squash merge. This also works if Web was merged with history.
3. Recheck the updated PR #130 against its actual main base, then merge it.

The separate branch preserves the resolved result for the second step without
changing either PR's base or merging the two features prematurely. The plan
and tests apply to these prepared trees; newer source changes require review.
