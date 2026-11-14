# MODIFIED 2026-11-14 — PR1 coverage and operational boundary

Source checkpoint: A1/A2 accepted through `44d4f99`; A3 adds one remote-only
combined lifetime test and documentation. Local A3/task/whole-branch review and
native required CI are separate gates. This is partial delivery of #81, not
issue closure. B1–B3 remain pending until PR1 acceptance.

## Actual consumers and owners

| Entry → source trace | Current boundary |
|---|---|
| Scoped IPC/MCP/CLI daemon search → `server::handle_request`/`scoped::call` → `call_tool_in` → `search_async::run` | Migrated interactive path; original operation and request Activity |
| Legacy IPC `CallTool search_docs` → `call_tool_in` | Same async stages; legacy half-close/reply semantics preserved, no new scoped EOF/upstream timing contract |
| `SearchSession::query_vector` → `DenseCache::query_embedding_async` → existing `Flights::run_async` → stage `async_remote` → OpenRouter async methods | Retained config+stages key, independent members, one owned leader; local stage uses admitted blocking phase instead |
| `PreparedSearch::remote_rerank_input` → async reranker → `RemoteExecution`/`RemoteStage` → `ReqwestTransport::post_json` | Existing Rig codecs in admitted CPU, async queue/HTTP/backoff; charged owned input; CPU finalization uses captured payload |
| CLI initial daemon connection unavailable → `cli/search.rs::run_with_policies_started` → `tools::search_docs_in` | Legacy direct snapshot provider boundary; no async replay/fallback after connected errors |
| `server::schedule_backfill`/`spawn_sync_job` → `tools::execute_sync_job_with_vectors` → `hybrid::jobs::embed_project_chunks_from` | Legacy sync/backfill; exact terminal/Sync attribution remains existing behavior, not migrated async retirement |
| `index/job.rs::{run_full,run_incremental}`/`finish_plan` → `hybrid::jobs::embed_texts_from` → `VectorCache::embed_missing` → synchronous Embedder | Legacy document batching/network; no async metadata lookup/write phase split yet |
| `watch/mod.rs` consumer + captured `WatchRuntime` → incremental index/job and recovery | Legacy dedicated producer and synchronous provider use |
| `hybrid::wire::Transport`, synchronous OpenRouter methods and `server::route_search` | Retained compatibility/direct/job interfaces; ingress search is intercepted before whole-call worker/route_tool |
| CLI provisioning/models/connect → explicit `daemon::remote::with_sync_command` | Single command runtime owner/retired Handle bridge; compatibility probes remain single-shot |

This table follows entry points, factory selection and downstream calls. Candidate
`rg` scans in private `task-a3/{provider-candidates,sync-candidates,async-route}.txt`
include test/legacy matches and are not an exhaustive proof alone. The real
execution-only Rig native-route test plus zero synchronous fixture entries prove
selection. `search_async` awaits only owned async work: synchronous provider calls
are confined to local/legacy CPU closures, with no `blocking::enter` across await.
Reqwest's new operational async implementation contains no Runtime/block_on;
retries use cancellable Tokio waits and retain the same pinned future on member
changes. Existing codecs, roles, normalization, dimensions and ranking are reused.

## Physical resource matrix

| Resource | Existing owner and limit | Retirement / proof |
|---|---|---|
| Native ingress/memberships | Daemon max_connections (default64); one request/reply per `handle_connection_in` loop | Original Activity retains connection permit through detached leader/physical CPU; explicit unadmitted compatibility entry remains separate |
| Search CPU/local inference/codec | WorkerAdmission search_active4/pending32 by default | Physical permit moves into `request::spawn_retained`; finishes/unwinds before ingress/source Activity release; async followers hold no CPU slot |
| Independent control | Existing control_active2/pending16 and original finite control policy | Existing owned physical closure; held remote search does not occupy this lane |
| Sync producer | Existing fixed sync slot1 | Still legacy blocking job; no claim of new resumable terminal cleanup |
| Interactive remote stage | RemoteSpec active4/pending32 by default | Active permit spans batches/codec/network/retry; async stage drop retires it, queued cancellation starts no send |
| DNS native closure | WorkerAdmission helper_active1; helper pending=remote.interactive_active | Owned custom resolver; permit/source Activity held until native lookup finish/unwind after HTTP future Drop; execution shutdown cancels queue/retained resolver admission |
| Readers / owned bytes | Existing ReadPool + SearchSpec request64MiB/retained256MiB logical allowance | Short lexical/dense/payload phases; rerank extra copies admitted before allocation; CpuOutput value drops before retained charge |
| Native libraries/background producers | Existing Tantivy, ONNX/tokenizer, ANN and watcher owners | Not represented as Tokio workers or included in a process-wide thread/RSS reserve; legacy producer migration is PR2 |

`max_blocking_threads` is an executor ceiling with validated room for admitted
search+control+sync1+helper lanes, not a reservation of all native resources.
Original deadlines include queue/prep/network/retry/rerank/reply. Shared work has
its own fixed hard/attempt clock, independent member deadlines and cancellable
interest; membership wake never reacquires/resends/re-jitters pending work.
Metadata/config/project-read/preparation leases do not cross remote waits.
Captured OwnedPayload may finish after later mutation; before capture a stale
runtime/generation refuses with retry. No continuous old-generation service,
throughput, speed, provider billing-cancellation or total-process capacity claim.

## Requirements → source → actual evidence → remaining gate

Evidence names below are existing tests, not planned substitutes. A1/A2 full
executions and fresh reviews are recorded in their private packages; A3 local
commands and input-identity proof accompany its report. Native CI is pending.

| Requirement | Source / actual evidence | Remaining scope |
|---|---|---|
| FR-81-01 | `search_async`, async OpenRouter/Rig factory/transport; `remote_query_wait_releases_search_capacity_for_actual_lexical_dispatch`, `execution_only_rig_stages_run_through_actual_async_search_without_sync_bridge` | Native whole-PR gate |
| FR-81-02 | SearchOperation, RemoteCall/RemoteStage, scoped reply; A1 queue/network/backoff/jitter expiry and partially-consumed attempt tests; A2 same-HTTP early/later peer and multistage/reply tests | Background/direct original-operation adoption PR2 |
| FR-81-03 | Existing retry classification + async RemoteStage; actual controlled Retry-After/default/overridden delay/jitter chains and26 OpenRouter fixtures | Legacy jobs still use sync retry boundary |
| FR-81-04 | `tools::{start_search,prepare_with_query_vector,PreparedSearch}`; writer/reload/generation/stage rejection and owned old-rerank snapshot tests | Job phase splitting PR2 |
| FR-81-05 | Existing WorkerAdmission/provider lanes/PayloadBudget; queued cancellation, local inference, low retained budget, codec slot and abandoned-output Drop tests | Separate background limits/retained plans PR2 |
| FR-81-06 | Existing Flights + retained RuntimeRevision and pure LRU;10 async flight tests incl completion-publication gate, failure/unwind, Notify/late join; real-client reload/last-owner combined case | Native whole-PR gate |
| FR-81-07 | Existing admitted CPU spawn/Activity, local follower and real Rig codec gate after scoped EOF; normal/unwind cases | Background index/native terminal ownership PR2 |
| FR-81-08 | RemoteSpec + DaemonSpec helper, ConfigSpec composition; owner override/default/feature-off/provenance/v1 roundtrip/connect preservation, actual Shared fatal recognized key/overflow and runtime-only assessment tests | Background settings with real consumer; full CF13/v2 excluded |
| FR-81-09 | Explicitly legacy index/Sync/watch/backfill/direct inventory above | B1–B3 pending; not delivered |
| FR-81-10 | Existing scoped IPC/AsyncClient/MCP proxy/no-replay; scoped17 and full CLI client/MCP/HTTP suites; explicit command runtime error/unwind tests | Complete direct/index runtime adoption PR2 |
| FR-81-11 | Source-owned bounded resolver + execution shutdown; retained real resolver composed-owner test, queued CPU shutdown test, closure/codec ingress retirement tests | IndexHandle/job finalizer and full shutdown attribution PR2 |
| FR-81-12 | Existing operation queue/stage timing + RemoteSnapshot queue/network/backoff/CPU/counts | Complete additive versioned status/explain projection B3; no partition wall-clock claim |
| FR-81-13 | Shared ranking/scoped/refill/snippet algorithms, Rig schemas, no silent fallback/redaction;129 preservation tests + frozen full CLI1614/0/18 and lean1377/0/11 at44d4 | Public PR docs/linux/macos/windows + artifact/browser/release/performance gates pending |
| NFR-81-01/03 | Physical matrix, bounds/refusal/secret-safe owner/view/response bytes tests | No full-process/native guarantee; background byte admission PR2 |
| NFR-81-02 | Locked pins, lean offline dependency boundary and individual source checks; no dependency changes | Native platform required CI pending |
| NFR-81-04 | Actual causal REDs, A1/A2 fresh PASS/APPROVED incl scoped fixes; A3 exact receipts/input identity | A3 task/strongest final review and native CI pending |
| SC-81-01 | Actual native held-HTTP independent lexical + writer/read/config lease tests | PR native gate |
| SC-81-02 | A1 actual expiry/retry timing seams, A2 original multistage/queued/reply budget and same pinned attempt | PR2 background/direct portion |
| SC-81-03 | Existing async registry matrix + handoff Arc identity + combined real-client reload lifetime | PR native gate |
| SC-81-04 | Actual owner/default override/invalid-before-effects/assessment/effective tests | B3 complete diagnostics |
| SC-81-05 | Bounded DNS/public shutdown, local/codec closure and original ingress guards after awaiter Drop | B2 terminal jobs/full drain |
| SC-81-06 | Legacy consumer inventory is explicit | Entire criterion B1–B3 pending |
| SC-81-07 | Existing core counters and frozen golden/benchmark/offline preservation | Complete metrics B3; public native/artifact/browser gates pending |

A1/A2 reviews, including their fix1 closures, are PASS/APPROVED with no new
Critical/Important findings. A3 resolves the previously separate real-client Drop
and retained-key lifetime evidence in one hermetic test. This coverage does not
promote remaining native/task/final-review gates to completed acceptance.

## MODIFIED 2026-11-14 — final-review I1/I2 execution boundaries

The final whole-branch challenge found two PR1 gaps; its original verdict was
PARTIAL/CHANGES. The author fix runs an owned QueryPublication through the same
admitted search CPU phase. Existing Flights retains the successful terminal
membership through the asynchronous handoff, bounded by the member's original
operation. Publication checks before work, after revision admission and after
cache-lock acquisition prevent cancellation/deadline warming after contention.
The actual ProjectCell publication writer gate on a current-thread runtime proves
independent control and cancellation/expiry finish before releasing the writer,
then proves the cancelled physical publication left no cached entry.

RemoteStage now runs all non-2xx error-envelope parsing and tree/body destruction
through existing RemoteExecution.cpu, with the same original call/physical/
ingress/source owners. A >1MiB response with an occupied actual CPU lane proves
admission and cancellation before decoding; retry classification/status helpers,
backoff/jitter and final-attempt rules remain shared. True backoff cancellation
and expiry use the real existing timing-entry seam with the same30.049s assertion.
The integration name `cancelled_backoff_starts_no_new_attempt` is deliberately
corrected to `cancelled_retry_processing_starts_no_new_attempt`: its first Pending
now can be the admitted response codec. No assertion/budget/ignore is removed;
actual backoff-phase proof stays in the gated unit case. The successful handoff
Arc/call-count regression now holds an asynchronous publication gate.

Focused/current lint/source checks pass; new full CLI/lean/release-perf executions
on the fixed frozen source and original strongest reviewer's fix-only verdict
are independent pending receipts at commit time. Earlier c3 native/performance
results are historical, not acceptance of this changed production input.
