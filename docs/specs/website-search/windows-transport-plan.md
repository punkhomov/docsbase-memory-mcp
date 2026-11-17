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
- [x] RED: real persistent pipe exchange completes native writes without
  creating a flush worker; explicit final drain delivers the complete frame.
- [x] Lazily create a dedicated drain worker only for explicit final drain.
  Reserve its capacity before creation; keep its native HANDLE, buffer and
  cancellation callers owned through actual thread signal/join.
- [x] Cover pending write/cancel-return interleavings and pre-entry helper
  cancellation; retain bounded quarantine when completion is unproved.
- [x] Cross-check exact native source, execute Windows cases; fresh task review.

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

## Delta 2026-11-17 — W3 integration slices and explicit ownership API

W3 remains the application integration task; these slices avoid a single opaque
facade/root rewrite. Existing context-free constructors cannot safely own a
Windows retirement scope and must migrate at their real composition boundaries.

1. **W3a: root and platform seam.** Add an explicit synchronous transport scope
   and clonable submission capability. The outer caller owns Port/progress and
   drives retirement independently of application polling; application work runs
   on one scoped, named thread with an explicit stack reservation. Scope spans
   the application/command, never one RPC. Its work constructs/uses its runtime
   (or synchronous clients) and existing JobRoot normally. Error/unwind closes
   ingress and joins actual physical retirement before leaving the scope.
   A real blocking-work/churn test must first demonstrate that the existing
   cooperative run_on driver cannot reap while synchronous work is running.
   The platform capability supplies bind/connect_async/connect_blocking; no
   hidden global reaper or operational interprocess fallback. Native/worker
   stacks, two native regions per handle and optional drain workers are counted
   before creation. Unix remains a thin delegated capability.
2. **W3b: daemon and delivery.** One scope outside JobRoot/Runtime/lease; pass
   capability into serve/bind. Keep physical listening/admitted/shedding owners
   in the same bounded arena. Add native connection/request ownership tails
   before submission; idle reads charge connections only. Add native write
   barriers and explicit successful final close under existing deadlines.
3. **W3c: clients and entrypoints.** Explicit capabilities in blocking/async
   client constructors, CLI Context, MCP Frontend and Web pool. Blocking I/O
   polls the same owned operations with a wake/condition and actual deadline;
   timeout cancels the exchange while the outer root keeps native storage.
   Blocking clone/split must preserve one serialized writable stream. Stop
   reads a complete acknowledgement under the original overall deadline.
4. **W3d: operational acceptance.** Migrate real fixtures to the same explicit
   root API; prove capacity retention, deadlines, cancellation and final reply
   delivery through actual daemon/client paths. Remove unused core adapters,
   resolve native lints, run full/lean verification and fresh W3 review.

The root thread topology is fixed per application scope (one application work
thread plus the existing native progress thread), not proportional to requests.
Per-connection drain threads remain optional and bounded until actual join.

## Delta 2026-11-17 — W4 acceptance slices

- **W4a / #165:** deterministic real accept/registration regression, both legacy
  and JobRoot-enabled paths; mutation RED, focused lifecycle/subprocess checks,
  fresh task review. Original production grace and idle checks stay unchanged.
- **W4b:** final owned native cancellation/control-progress/early-close/deadline
  acceptance and diagnostic latency/allocation/helper measurements, then full
  source compatibility/native CI and whole-feature review.

## Delta 2026-11-17 — W3e: native arrival and withdrawal races

Required whole-suite Windows acceptance exposed two compatibility gaps after
W3d: blocking burst admission sees transient PIPE_BUSY, and a client departing
before ConnectNamedPipe yields ERROR_NO_DATA which must not stop the listener.
- [ ] Native RED/GREEN for busy admission using one caller deadline and for a
  peer synchronously closed before accept; keep original physical arena bounds.
- [ ] Explicit deadline API for Windows blocking callers, existing client2s
  IO_TIMEOUT and fixture10s admission budget; no helper/default/replay.
- [ ] Retire only the known abandoned instance, yield/rearm; preserve other errors.
- [ ] Fresh W3e review and required native CI before readiness.
