# macOS support — progress ledger

Plan: `docs/specs/macos-support/tasks.md` · Spec: `docs/specs/macos-support/requirements.md`
Machine mode: fresh reviewer subagent per task; ledger appended with each task commit.

Plan created: M1 (шов: liveness/counters/paths/socket guard) → M2 (тесты + CI-джоба macos) → M3 (release/installer/update) → M4 (доки/конституция).
Decisions: aarch64-only (ADR-M2), min macOS 12, ad-hoc подпись + `codesign -v`, нотаризация отложена (ADR-M3), `libc` как macOS target-dep (ADR-M1, уже в графе транзитивно).
