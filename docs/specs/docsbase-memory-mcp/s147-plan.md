# S147 — selected CLI sync target (#147)

Bounded bugfix; base0c6d707cae9207d370514c5ab53a8ae3ae9c7144. New crates: none.
Requirements: dated FR-17/FR-30 clarification and SC-S147 in requirements.md.
Files: src/cli/mod.rs, tests/cli_routing.rs, this spec/ledger. No other worktree.

1. Draft finite native sync peer and joined request trace. Retain existing
   sync_via_daemon_polls_until_done name as no-PATH control. Add cwd A / absolute
   B and relative B, invalid missing explicit path without handshake/direct
   mutation, and distinct-B no-daemon control. Canonicalize both recorded and
   expected paths in the test process. Keep existing sync polling semantics.
   Use async platform Listener/Stream on Unix and existing Tokio native pipe
   server/client at the same published endpoint on Windows (avoid interprocess
   blocking flush/linger workers). One cancellation/watchdog select surrounds
   accept plus all IO and retention of the accepted stream after the fourth
   reply until command completion signals finish/cancellation. Do not rely on
   EOF or FlushFileBuffers for delivery. Bind before command; cancel and join before
   assertions and on command spawn failure/unwind. Child watchdog kills/reaps.
   Keep cleanup probes for no-connect early exit/spawn error, accepted no-Hello/
   partial frame with client held open, and watchdog. No fixture branch probes.
2. Wait for scoped plan/test review PASS and controller rebase onto latest main
   (currently6826), then take the handed-off Cargo slot. Build Web/client SDK as
   required, then run the two selected-target cases on unchanged production code
   and preserve actual RED paths (A observed instead of B). No production edit
   before that RED. The direct-only old sync test is insufficient for explicit B.
   Run focused no-connect/held-native-client/watchdog cleanup evidence; watchdogs are test
   failure bounds, without elapsed-time performance assertions or budget changes.
3. Resolve selected target before connecting and use the same canonical target
   for handshake and direct fallback. Correct the PATH help text. Preserve
   ordinary Project fallback, one bound connection, wire/polling/errors,
   nested discovery and all product budgets/defaults. #79 owns profile refusals.
4. GREEN: selected cases and full cli_routing/cli_read/cli_index; relevant and
   all-target Clippy/fmt/diff, required broader functional/native verification.
   Fresh task/final review. Preserve actual command exits/logs and inventory.

Review: no target substitution on invalid PATH; no daemon/direct side effects
before target resolution; no extra connection or dropped status poll; finite
native fixture cleanup on Linux/macOS/Windows. No dependencies/features/API,
storage/index/watcher, paid providers, thresholds, ignores or deadline changes.
