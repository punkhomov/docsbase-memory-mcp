# Windows IPC retirement research — 2026-11-17

Research for PR #164 / issues #155 and #165, against PR head
`27ec2038aecaa0f2c7a0c26f7493486c22eae7dc`. This is an evidence and hypothesis
record, not accepted implementation, a replacement for the issue's delivery
contract, or native acceptance. No application code changed during this research.

## The three boundaries

1. **Logical acceptance:** an adapter copied bytes into its own storage and
   returned from `write`/`write_all`. Native work may still reference that storage.
2. **Native write completion:** the submission call returned and its native
   completion was reconciled; cancellation callers no longer reference the
   operation. Kernel pipe buffers may still contain bytes unread by the peer.
3. **Peer drain:** a successful server-side `FlushFileBuffers` establishes that
   the client read the buffered bytes. It does not establish application parsing
   or execution. An application ACK would be a different protocol guarantee.

These boundaries are different. Microsoft explicitly documents that completion
can run before `ReadFile` returns [R3]. `CancelIoEx` and `CancelSynchronousIo`
request cancellation without waiting [R1, R2]. In particular, cancellation
returning `ERROR_NOT_FOUND` does not authorize freeing a still-referenced
operation; Microsoft reproduced and explained a completion race [R4].

The resource owner therefore outlives the request future. HANDLE, OVERLAPPED,
native buffer, cancellation callers and completion dispatch must all reach their
own terminal boundaries before their storage can be reclaimed. A timeout ends
the exchange budget; it does not manufacture native completion.

## Evidence from other implementations

| Reference | Actual approach | Consequence for this PR |
| --- | --- | --- |
| Microsoft's overlapped pipe server [R5] | Persistent asynchronous reads/writes; client close is the example's signal to disconnect. It explicitly avoids a blocking flush in the event loop. | Ordinary persistent replies need not each wait for peer drain. Client EOF is useful for normal teardown, but cannot prove that an arbitrary client parsed the reply. |
| libuv v1.x, pinned source [R6] | Waits for pending write requests before shutdown; checks outbound quota with `NtQueryInformationFile`; otherwise runs `FlushFileBuffers` in `QueueUserWorkItem`, returning completion to the loop. | Separates writes from graceful shutdown. A helper can still stall; copying its pool strategy would not supply our explicit helper/admission bounds. Its native quota optimization is not a portable Win32 drain proof to copy blindly. |
| libuv v2 PR #5067 [R7] | Removes emulated pipe shutdown and returns `UV_ENOTSOCK`; named pipes have no socket-style half-close. | Do not treat `AsyncWrite::shutdown` as proof of TCP-style half-close or infer a fixed-delay delivery guarantee. This v2 change does not describe v1.x behavior. |
| Mio issue #1944, open when checked [R8] | Documents retained HANDLE/buffer references when pending operations outlive selector teardown; discussion proposes write-completion-aware flush and retaining unresolved storage. | Replacing interprocess with raw Tokio/Mio is insufficient without auditing the exact locked version and retirement owner. The proposed flush means native write completion, not client receipt. |
| compio Proactor [R9] | Driver owns operations; cancellation documentation explicitly allows the underlying operation to continue. | Completion-owned APIs are a useful model. A new runtime does not automatically solve our deadlines, byte caps, helper bounds, shutdown or existing Tokio integration. |
| Boost.Asio Windows stream handles [R10] | IOCP-based async operations; caller keeps underlying buffers alive until completion handler. | The lifetime invariant is standard, rather than a project-specific complication. |

## Alternatives and scope

**A. Keep the owned IOCP core, separate native write completion from peer drain
(preferred candidate for a bounded PR).** On a persistent connection, finish
each normal reply through a real native write barrier under its original budget.
Keep connection/native-resource charges while the connection is live. Before an
intentional server close that promises delivery, explicitly request peer drain.
Cancellation/errors use the failed-exchange abort path, never a successful-delivery
claim. The root keeps physical owners and reaps them after native completion and
helper thread signal/join. This preserves the current wire format.

The existing core's `Pipe::poll_flush` currently includes peer drain and
`Connection::new` eagerly starts a helper for both server and client endpoints.
Implementing A would require an explicit API/contract change inside this private
adapter: a write barrier is not a renamed successful peer drain. Every normal
close path must be audited. A helper would be started only when an actual drain
is needed, with capacity reserved before starting it, and retained until join.
Using short-lived dedicated drain workers first avoids introducing cancellation
against reused pool threads. Repeated close/drain churn still needs a hard bound
on active plus retiring workers, not just a bound on currently running handlers.

**B. Add explicit protocol delivery acknowledgements.** A negotiated close/ACK
exchange can avoid server flush helpers on that negotiated path and can give a
stronger application-level guarantee. However, client/server/Node changes,
version/capability handling, timeout semantics and legacy behavior become part
of the work. A subsequent request is not universally an ACK: pipelining and
malformed clients invalidate that assumption. Waiting for client close alone
cannot make an uncooperative client's shutdown finite. Retain this as a later
protocol option rather than requiring it to finish #155.

**C. Replace the executor/library/transport.** Windows threadpool I/O can replace
a hand-written completion dispatch thread, but requires `StartThreadpoolIo`
before every operation and correctly balanced no-callback paths. Waiting for
callbacks with cancellation enabled does not cancel pending I/O [R11]. It still
needs the same operation/storage owner and peer-drain policy. compio adds a
runtime/dependency integration decision and ADR. Windows AF_UNIX exists [R12],
but changing endpoints would affect the manifest, Node connector, discovery,
permissions and supported platform behavior. None is a small drop-in resolution
for this PR.

`PIPE_NOWAIT` is also not an event-driven cancellation solution: an empty read
completes immediately rather than registering interest for a later completion,
so consumers must poll. Microsoft describes it as legacy compatibility [R14].

## Application integration consequences of candidate A

- `scoped::reply` and pre-frame error replies currently only await `write_all`.
  Because the new adapter accepts copied writes before native completion, add
  the native write barrier before reporting normal reply completion. Preserve
  the existing one-attempt expired-error behavior; it must not acquire a fresh
  drain budget.
- Make successful early close explicit, including Hello rejection, framing
  errors and StopDaemon. Do not rely on asynchronous Drop to deliver a final
  frame. A peer already closed, cancelled work or failed drain is an abort,
  even if some bytes happened to have been read.
- `stop_daemon` currently sends and closes without reading the acknowledgement.
  Its success still means process exit, but a healthy exchange should read and
  validate the full response within the existing overall deadline. The daemon
  must retain shutdown-reply ownership until its delivery/abort boundary.
- Persistent request-response clients need native write completion and their
  response read; they do not need an eager server-style read-drain helper on
  every request. Retain buffers through actual native work even if a caller
  drops the exchange future or hits its blocking I/O timeout.
- Attach application permits/Activity tails to registered native owners before
  cancellable submission. Include accepted, rejected-but-retiring and listener
  instances in the physical cap. Shedding connections must not create unlimited
  helper work outside that cap. Idle persistent native reads need the connection
  owner; they do not imply an active application request.
- Keep the outer retirement owner and progress capability alive through runtime
  cancellation, work error and unwind. If Windows does not supply a terminal
  result, retain a bounded quarantine and report draining/quarantine. Neither
  the docs nor the existing four-case probe prove a universal finite exit.
- Compose this owner outside both daemon work and client operations. A blocking
  read timeout is ineffective if returning from it or dropping its client then
  synchronously waits for unproved native retirement. The existing context-free
  platform/client constructors need an explicit compatibility/lifetime design;
  a new port/runtime per request or an unbounded hidden global reaper is not a
  substitute. Root integration is substantial remaining work, independent of
  how few peer-drain helpers the normal exchange needs.

## Efficiency hypotheses to measure

**H1 — helper-free ordinary exchange.** Splitting write completion from peer
drain removes helper creation/wakeups from ordinary persistent traffic and from
clients. Measure thread creations, live/retiring helpers and request latency;
verify normal early-close delivery separately. Do not assume a speedup in lieu
of native measurements.

**H2 — serialization allocation dominates small requests.** `encoding::Frame`
currently reserves `MAX_FRAME_BYTES + 1`, about 8 MiB even for Hello. At the
daemon's default connection cap of64, one such outbound region per connection
alone can reserve512 MiB; this is a capacity calculation, not measured RSS or
the entire process bound. The input frames, response values, native chunks,
client allocations and allocator overhead are separate. Compare the fixed cap
with checked bounded growth or a capped size-counting pass plus exact allocation.
The alternatives must reject oversize escaped JSON during serialization and
must bound capacity, not only check the final serialized length. Two-pass
serialization trades CPU for allocation size; no choice is established yet.

**H3 — dispatch lookup overhead.** The draft IOCP driver scans the handle arena
for each packet, then up to three operation slots. The first search is O(C).
If significant at intended concurrency, use a directly validated stable slot
identity with explicit reuse/generation fencing. Do not replace the scan with
unchecked raw-key dereferences or add an unbounded index for an assumed win.

**H4 — stack and kernel memory need separate accounting.** An eager helper per
connection means C helper threads. With a hypothetical1 MiB requested stack
and C=64, that is64 MiB of requested stack space, not64 MiB of measured RSS;
Windows stack reservation/commit and rounding differ. More importantly,
`CreateNamedPipe` buffer sizes are advisory; Windows may expand pipe buffers
and use nonpaged pool [R13]. Fixed user-space chunks and limited outstanding
submissions bound exposure; they do not prove an exact kernel-memory budget.

A reusable flush-worker pool could amortize thread creation but introduces an
additional correctness requirement: a cancellation ticket must fence worker
reuse until every caller targeting its thread has returned. Otherwise a delayed
cancel can hit the next connection. A global small pool can also let stalled
data drains occupy all shutdown/control capacity. Evaluate only with explicit
active/queued/retiring accounting and control-progress tests; do not pool merely
because a generic pool is available.

## Discriminating native tests before acceptance

1. Persistent ordinary request/response: exact frames over repeated exchanges,
   actual final native write completion, no peer-drain helper on that path.
2. Normal server early close and StopDaemon: gate the reader, prove complete
   frame delivery before successful disconnect; Stop client reads the response
   and then observes process exit. Do not use unread-byte counts as a proxy for
   a particular pending native operation.
3. Large positively observed pending write: drop/expire the handler while the
   peer is gated; original connection/native charges survive until actual
   write/cancel/helper terminal evidence. A legitimate failed-exchange abort
   may retire before the peer consumes the original response.
4. Flush worker pre-entry abort, abort during positively observed pending flush,
   normal completion racing abort, and a delayed cancellation caller after
   thread completion. Thread signaling/join, not a cancellation return or a
   phase flag alone, establishes retirement.
5. Capacity under repeated cancellation and failed admission: active plus
   retiring handles/helpers/buffers never exceed the owner envelope; exercise
   control progress and the next-listener allocation when capacity is full.
6. Root error, unwind and signal during normal shutdown: the original admission
   lease remains while native owners survive; unresolved work is explicitly
   quarantined and never described as clean shutdown.
7. Blocking and async client deadlines: the logical deadline expires without
   freeing native-referenced memory or silently making the connection reusable.
8. #165 separately: causal admission/registration barriers plus a controlled
   grace clock, preserving the named regression and real idle exit. An800-file
   workload and a100 ms sleep are not causal registration evidence.

Preserve existing checks, test inventory, latency budgets and baselines. Final
source must pass required docs/linux/macos/windows CI plus independent review;
controlled core-state tests alone are not whole-transport acceptance.

## Sources checked on 2026-11-17

- R1: [CancelIoEx](https://learn.microsoft.com/en-us/windows/win32/api/ioapiset/nf-ioapiset-cancelioex).
- R2: [CancelSynchronousIo](https://learn.microsoft.com/en-us/windows/win32/api/ioapiset/nf-ioapiset-cancelsynchronousio)
  and [cancellation considerations](https://learn.microsoft.com/en-us/windows/win32/fileio/canceling-pending-i-o-operations).
- R3: [Raymond Chen: completion may precede ReadFile return](https://devblogs.microsoft.com/oldnewthing/20230215-00/?p=107832).
- R4: [Microsoft Q&A: ERROR_NOT_FOUND during completion](https://learn.microsoft.com/en-us/answers/questions/116109/cancelioex-spuriously-fails-with-error-not-found-w).
- R5: [Microsoft's overlapped pipe server](https://learn.microsoft.com/en-us/windows/win32/ipc/named-pipe-server-using-overlapped-i-o),
  [FlushFileBuffers](https://learn.microsoft.com/en-us/windows/win32/api/fileapi/nf-fileapi-flushfilebuffers),
  [DisconnectNamedPipe](https://learn.microsoft.com/en-us/windows/win32/api/namedpipeapi/nf-namedpipeapi-disconnectnamedpipe).
- R6: [libuv v1.x pipe.c, immutable e5dab843](https://github.com/libuv/libuv/blob/e5dab8432177f43af97bbe032037d273234642ac/src/win/pipe.c):
  `uv__pipe_shutdown`, `pipe_shutdown_thread_proc`, `uv__process_pipe_shutdown_req`.
- R7: [libuv v2 shutdown removal #5067](https://github.com/libuv/libuv/pull/5067),
  merged2026-03-16; base `master`, distinct from v1.x.
- R8: [Mio #1944](https://github.com/tokio-rs/mio/issues/1944),
  open, last update2026-10-30 when checked. Locked-source behavior was separately
  established in #155; upstream proposals are not assumed shipped here.
- R9: [compio Proactor ownership and cancellation contract](https://docs.rs/compio/latest/compio/driver/struct.Proactor.html).
- R10: [Boost.Asio stream HANDLE support](https://www.boost.org/doc/libs/latest/doc/html/boost_asio/overview/windows/stream_handle.html)
  and [async write buffer lifetime](https://www.boost.org/doc/libs/latest/doc/html/boost_asio/reference/windows__basic_stream_handle/async_write_some.html).
- R11: [StartThreadpoolIo](https://learn.microsoft.com/en-us/windows/win32/api/threadpoolapiset/nf-threadpoolapiset-startthreadpoolio),
  [CancelThreadpoolIo](https://learn.microsoft.com/en-us/windows/win32/api/threadpoolapiset/nf-threadpoolapiset-cancelthreadpoolio),
  [WaitForThreadpoolIoCallbacks](https://learn.microsoft.com/en-us/windows/win32/api/threadpoolapiset/nf-threadpoolapiset-waitforthreadpooliocallbacks).
- R12: [Microsoft: AF_UNIX on Windows](https://devblogs.microsoft.com/commandline/af_unix-comes-to-windows/).
- R13: [CreateNamedPipeW buffer/quota remarks](https://learn.microsoft.com/en-us/windows/win32/api/namedpipeapi/nf-namedpipeapi-createnamedpipew).
- R14: [Raymond Chen: overlapped PIPE_WAIT versus PIPE_NOWAIT](https://devblogs.microsoft.com/oldnewthing/20110114-00/?p=11753).
