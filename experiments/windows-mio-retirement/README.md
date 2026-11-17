# Throwaway Mio retirement experiment — 2026-11-17

This branch is an experiment, never a production PR or a replacement for #164's
required CI. Main and PR #164 remain unchanged. No provider calls are made.

Question: can existing Mio/Tokio own connection accounting through actual native
retirement, without docsbase's IOCP implementation?

The only dependency changes being tried are a guard retained as the last field
of Mio's already operation-owned `Inner`, a real pending-write flush barrier, and
Tokio forwarding those two facilities. Existing overlapped calls, completion
dispatch, buffering, error paths and cancellation stay in the libraries.

All downloaded source archives are checked against the existing root Cargo.lock
checksums before applying the committed small patches. The probe has its own
lockfile. Generated vendor directories are ignored. These are modified existing
dependencies, not a recommendation to ship forks or a new runtime dependency.

One bounded probe cycle:

1. Sensitivity control: a logical stream guard releases before a large unread
   Mio write. Cleanup consumes the complete response before asserting the
   mismatch. This isolates guard placement on the same patched write barrier;
   it is not a whole-stack old-main or interprocess performance comparison.
2. Candidate: a guard installed before registration survives logical Drop, then
   retires after the actual pending write and HANDLE/buffers finish.
3. A flush stays pending with an unread large write, then completes after native
   completion. Persistent replies do not need a peer-drain helper.
4. Verify normal final delivery, failed-exchange disconnect, repeated reuse of a
   fixed admission budget, and error/unwind with an independently live runtime.

The stop condition is a measured feasibility result, including gaps. This probe
does not rewrite application facades, blocking clients, StopDaemon or worker
lifecycle. A successful small probe is not whole-application acceptance.
Per-request Activity tails, bounded/cancellable final-drain helpers, callback and
cancel-return interleavings, runtime lifetime, blocking deadlines and realistic
performance all remain comparison obligations before adopting a replacement.

Build: `python3 prepare.py`, then `cargo test --locked -- --nocapture` on native
Windows. Cross-checking Windows code on Linux is compile-only evidence.

Use Rust1.100.0. Preserve production requirements/budgets/baselines/ignored tests.
Fresh review must assess the result, not just whether the probe is green.
