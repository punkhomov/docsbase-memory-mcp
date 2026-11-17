# Windows transport completion implementation plan — 2026-11-17

Goal: finish PR #164 / #155 without releasing resources still used by Windows,
with complete successful final replies and bounded failed-exchange retirement.
Architecture: preserve NDJSON and existing operation budgets. Separate native
write completion from peer drain; own native operations in one bounded IOCP
service and retain its synchronous outer owner through cancellation and unwind.
Tech stack: Rust1.100.0 stable, Tokio, existing windows-sys; no new crates.
Spec: requirements.md, design.md, windows-transport-research.md and issue #155.

## Global constraints

- No `unwrap`/`expect`/`panic!` in production `src/**`.
- No new crates without ADR; lean stays offline; official build is cli-full.
- No protocol version, baseline, threshold, ignored-test or production grace change.
- Root ownership must survive work-future cancellation/error/unwind. Cancellation
  return and logical handler return never stand in for native retirement.
- Current human-only merge rule applies. Required docs/linux/macos/windows checks
  and fresh task/final reviews precede readiness.

## W1 — capped serialization proportional to actual message size

Files: src/ipc/encoding.rs, src/daemon/scoped.rs; tests in encoding.rs.
Consumes Serialize and the existing MAX_FRAME_BYTES; produces the same Frame
and NDJSON bytes with checked allocation before serialization copying.
- [x] RED: small real Request::Hello reserves its encoded length plus delimiter,
  while escaped oversized JSON still fails and max-sized frames still fit.
- [x] Count with a capped io::Write sink before exact allocation; bound the
  second serialization independently. Drop oversized response before encoding
  its small protocol error; do not allocate a full-size fallback region.
- [x] Run focused IPC/scoped tests, lint/format; fresh task review and ledger.

## W2 — owned native write barrier and explicit peer drain

Files: src/platform/windows/{connection,flush,native_io,root}.rs.
Consumes the bounded Service/HandleToken/Operation arena; produces Pipe whose
flush waits for write completion and explicit drain waits for client reads.
- [ ] RED: real persistent pipe exchange completes native writes without
  creating a flush worker; explicit final drain delivers the complete frame.
- [ ] Lazily create a dedicated drain worker only for explicit final drain.
  Reserve its capacity before creation; keep its native HANDLE, buffer and
  cancellation callers owned through actual thread signal/join.
- [ ] Cover pending write/cancel-return interleavings and pre-entry helper
  cancellation; retain bounded quarantine when completion is unproved.
- [ ] Cross-check exact native source, execute Windows cases; fresh task review.

## W3 — platform/application ownership integration

Files: platform seam/facades, daemon lifecycle/server/scoped, IPC clients and
CLI/MCP/Web composition roots; owner policy/catalog documentation.
Consumes complete transport policy and Service; produces explicit capabilities
for bind/connect, bounded buffering and outer-root retirement.
- [ ] RED: real admitted stalled-reader regression observes actual native
  terminal state and original permit/Activity ownership; normal close delivers.
- [ ] Install one explicit outer owner per application scope; pass submission
  capability to daemon/client consumers. Never create a migrating port per RPC,
  wait indefinitely in a client timeout/Drop, or use a hidden global reaper.
- [ ] Retain permit/Activity tails before native submission; pending retirement
  consumes the same capacity. Include listening/rejected physical instances.
- [ ] Both clients await native writes, preserve poisoned exchange behavior and
  use actual blocking deadlines. Stop client reads a complete acknowledgement
  under the original overall deadline before observing process exit.
- [ ] Normal final replies drain explicitly; abort paths discard only failed
  exchanges. Signal escalation does not free unresolved native storage.
- [ ] Full/lean and relevant feature verification; fresh task review and ledger.

## W4 — deterministic lifecycle and final acceptance

Files: tests/cleanup.rs, daemon lifecycle acceptance tests, Windows transport
acceptance and website-search specs/progress.
- [ ] Replace #165's sleep/workload inference with causal registration/work
  barriers and a controllable grace clock; preserve real cleanup coverage.
- [ ] Exercise repeated cancellation/admission exhaustion, clean final delivery,
  root unwind, StopDaemon, blocking/async deadlines and control progress.
- [ ] Final-source latency/allocation/helper measurements; no budget changes.
- [ ] Required native CI and independent whole-feature review; resolve findings,
  reconcile PR body/specs/ledger and mark ready only after evidence is complete.
