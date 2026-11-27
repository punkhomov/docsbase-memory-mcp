# Config

Precedence (highest first): **CLI > project > global > defaults**.

## Files

| Level | Path | Format |
|---|---|---|
| Global | `~/.config/docsbase-memory-mcp/config.toml` (`~/Library/Application Support/docsbase-memory-mcp/config.toml` on macOS, `%APPDATA%\docsbase-memory-mcp\config.toml` on Windows) | TOML, all keys optional |
| Project | `<project>/.docsbase.toml` | TOML, all keys optional |
| Ignores | `<project>/.docsbaseignore` | gitignore-style patterns |

Environment overrides (highest-priority directory resolution, used by tests):

- `DOCSBASE_CACHE_DIR` — cache root (registry, indexes, daemon state)
- `DOCSBASE_DATA_DIR` — installed binary + `install.json`
- `DOCSBASE_CONFIG_DIR` — global `config.toml` location

Global and project TOML sources have a 1 MiB limit. `models use` and
`connect` apply the same limit before parsing the current file and before
publishing a replacement. The `models use --dry-run` preview also applies
the limit. A refused edit leaves the source intact.

## Reloading configuration

The daemon reads global configuration at startup; changing it requires a daemon
restart. Project configuration is read again when a session opens that project.
A successful reopen publishes the configuration and its validated hybrid stages
together. A parse or stage-admission failure rejects the reopen and leaves the
previous valid configuration, stages and watcher policy active. Requests that
already captured the previous runtime can finish using it.

## Keys

| Key | Default | Meaning |
|---|---|---|
| `ignores` | `[]` | Extra ignore patterns, appended to built-ins (`.git`, `target`, `node_modules`, …) |
| `max_file_size` | `1048576` (1 MiB) | Files larger than this are skipped |
| `max_docs_per_project` | `20000` | Admission limit per project |
| `auto_index` | `false` | Register + index unknown projects on first connection (ADR-6: off) |
| `hybrid` | `false` | Hybrid stages (embeddings + rerank) master switch; boolean shorthand or a table, see below |

Unknown keys are rejected (`deny_unknown_fields`) — typos fail loudly.

## Search request policy (global only)

The optional global `[search]` table controls one search operation and its
preparation scope and captured payload. Project files cannot override these daemon-scoped fields.
Missing keys are initialized by the search configuration owner during parsing;
search components receive complete settings.

| Key | Default | Meaning |
|---|---|---|
| `dedupe_max_candidates` | `1000` | Ceiling for extra document-cap refill; 1…1000. Effective ceiling is the larger of this value and the ordinary base window |
| `request_timeout_ms` | `30000` | Original search budget, including startup, admission, transport and reconnect |
| `max_request_payload_bytes` | `67108864` (64 MiB) | Logical allowance for one preparation scope or captured candidate/citation/filter/text payload; the two phases do not overlap |
| `max_retained_payload_bytes` | `268435456` (256 MiB) | Shared allowance for concurrent live scopes/payloads in one daemon; not process RSS |

```toml
[search]
dedupe_max_candidates = 1000
request_timeout_ms = 30000
max_request_payload_bytes = 67108864
max_retained_payload_bytes = 268435456
```

`dedupe_max_candidates` only affects `dedupe=path`; it does not shrink the base
window or change indexing/vector/query caches. It may enlarge the single rerank
request beyond `hybrid.reranker.window`, which remains its ordinary base size.
For RRF with a document cap, the dense source is collected once up to this
effective ceiling and reused as prefixes during refill.

All values must be positive and the per-request allowance cannot exceed the
shared allowance. Payload admission refuses an oversized request or exhausted
allowance explicitly; it does not truncate provider text or wait indefinitely
for memory. These are accounted payload limits, not a process RSS limit: model
weights, ANN structures and allocator overhead have separate lifetimes.

Restart the daemon **and MCP frontend processes** after changing global search
policy. CLI search reads its global policy on each invocation; direct fallback
uses that same snapshot for project configuration and retains the original
operation. A running daemon still clamps client time to its startup policy.
Invalid global configuration prevents MCP startup instead of selecting another
policy. See [search timing and cancellation](MCP.md#timeouts-and-cancellation).

## Remote execution policy (global only)

The optional global `[remote]` table controls asynchronous remote embedding and
rerank for daemon/direct searches, indexing, Sync, watchers and vector backfill.
CLI/MCP forwarding uses the daemon policy; direct commands capture global policy
once for the whole command.
`RemoteSpec` resolves defaults, validates values and records provenance before
runtime construction. Project overrides are rejected. Enabled remote stages
still require an available provider and credentials; this policy does not enable
hybrid search or select another provider on failure.

| Key | Default | Meaning |
|---|---|---|
| `interactive_active` | `4` | Concurrent remote stages, including their codec, batch and retry waits |
| `interactive_pending` | `32` | Queued interactive stages; zero refuses a busy stage immediately |
| `background_active` | `1` | Concurrent background embedding stages |
| `background_pending` | `8` | Queued background stages |
| `background_timeout_ms` | `600000` | Whole background job budget, including admission, capture, batches, retries and terminal response |
| `attempt_timeout_ms` | `30000` | Maximum time per HTTP attempt, narrowed by the remaining original budget |
| `retry_attempts` | `4` | Total attempts, including the first |
| `retry_initial_ms` | `250` | Initial retry delay |
| `retry_max_ms` | `4000` | Maximum exponentially multiplied retry delay |
| `retry_multiplier` | `2` | Retry delay multiplier |
| `max_retry_after_ms` | `30000` | Cap on a retryable 429 response's Retry-After seconds |
| `jitter_max_ms` | `50` | Exclusive upper bound for added jitter; zero disables it |
| `embed_batch_texts` | `128` | Maximum embedding inputs per batch |
| `embed_batch_bytes` | `524288` (512 KiB) | Target input bytes per embedding batch; one oversized text remains a single batch |
| `max_response_bytes` | `33554432` (32 MiB) | Maximum actual streamed response bytes, independent of Content-Length |

```toml
[remote]
interactive_active = 4
interactive_pending = 32
background_active = 1
background_pending = 8
background_timeout_ms = 600000
attempt_timeout_ms = 30000
retry_attempts = 4

[daemon]
helper_active = 1
background_active = 1
background_pending = 8
```

`[daemon].helper_active` defaults to `1` physically running DNS lookup. Its
pending allowance is derived from `remote.interactive_active + remote.background_active`.
The existing daemon worker admission and activity owners retain the helper's
physical charge after a cancelled lookup. `[daemon].background_active` (default
`1`) bounds admitted background CPU phases and owned native jobs;
`background_pending` (default `8`) bounds waiting jobs. These are separate from
remote provider stages, so a network wait releases background CPU capacity.
The daemon's `max_blocking_threads` must fit
`search_active + control_active + background_active + helper_active + 1`
reserved terminal worker; its default remains `512`. The terminal lane retires
already accepted jobs even after ordinary admission closes. This is a Tokio
blocking ceiling, not a preallocated pool or a bound on native library threads.

Active counts, attempts, batch counts/bytes and response bytes must be positive;
pending may be zero, and the attempt timeout must be positive. Retry delays and
jitter may be zero, initial delay cannot
exceed its maximum, and all counts/durations must fit their runtime domain.
Retry/backoff and provider queue waits consume the original search deadline.
Shared query flights use an independent hard budget: a later live peer can keep
the same HTTP attempt alive after an earlier caller expires, within the attempt
and hard limits. Membership changes do not resend requests. Cancelling a caller
does not imply the provider cancelled or refunded its work.

Change these global policies by restarting the daemon. Effective configuration
exposes values, units, source and provider capability; recognized invalid remote
policy is fatal before listener/state/provider effects. Policy changes request a
runtime restart and do not rebuild lexical/vector/ANN data or change model,
dimensions, normalization, ranking or completed query-cache identity.

Index/Sync, automatic registration, watcher updates and open-time backfill use
bounded captured plans and asynchronous embedding batches. Search and job payloads
share the configured `[search]` byte allowance; capture refuses before project
markers/index changes if the plan or codec workspace cannot fit. In-place jobs
retain the project writer lease and may refuse same-project searches while
remote work is pending. They do not promise old-generation availability.

Direct Search/Index/Sync own one command runtime outside the work future; errors
and unwind close and drain native/physical work before runtime destruction.
Only an initial failed daemon connection permits direct fallback. A connected
failure is not replayed, and Website restrictions remain enforced. Models/connect
probes retain their explicit synchronous compatibility scope. No process RSS,
process-wide thread reserve, provider throughput or increased capacity is claimed.

Explained search responses add `meta.execution` version `1`: `cpu_ms`,
`network_ms`, `backoff_ms`, `provider_attempts`, and `shared_work`. Existing
`meta.queue_ms` and `meta.stage_ms.total` retain their meanings. CPU time is
elapsed time inside admitted closures, not OS thread CPU accounting. A shared
query reports that waiter's actual embedding wait as network time and marks
`shared_work`; its shared provider attempts/backoff are counted on the shared
owner, not copied into each request. An ordinary rerank records its own attempts.
The status tool adds numeric `remote_execution` lane occupancy, attempt/call/
cancellation/refusal totals and retained payload bytes. Timings can overlap and
are not a wall-clock partition. Metrics contain no query, document, body, model
or credential labels. Physical CPU/helper occupancy lasts through finish/unwind.

## Dense and query-vector cache policy

Set any of these values in the global v2 file. Unspecified fields keep the
defaults shown here. Time values use milliseconds.

```toml
[dense]
max_vectors = 1000000
max_bytes = 1610612736
max_projects = 8
min_vectors = 4096
build_backoff = 30000
max_ann_disk_bytes = 4294967296
ann_keep_generations = 2
query_cache_max_entries = 256
query_cache_max_bytes = 4194304
backfill_pin_ttl = 3600000
ann_outcome_ttl = 3600000
```

The query cache retains up to 256 vectors and 4 MiB of vector payload by default.
Both limits apply to the same shared cache; hits refresh recency and insertion
evicts the least recent entries. Larger vectors can serve a request without
being retained. This byte count excludes keys, allocator overhead, externally
held vector references and in-flight requests.

Both query cache capacities must be positive. Daemon startup and direct CLI
search use one resolved policy. Restart the daemon after editing it. Rust
callers can still supply explicit policies to the cache or daemon constructors.

`max_vectors` limits resident rows per project. `max_bytes` covers resident
entries and live build reservations across projects. `max_projects` limits
resident projects, and `min_vectors` controls eligibility for ANN. A zero
resident capacity permits exact streaming without retaining those rows.
`min_vectors = 0` permits ANN for any nonempty corpus that fits the other caps.
`build_backoff = 0` permits retry on the next eligible lookup.

`ann_keep_generations` must be positive. Retention cleanup keeps the newest
generations for each project and model, including their different ANN recipes.
`max_ann_disk_bytes` is a best-effort cleanup target. Disk cleanup skips
locally pinned slots and keeps at least one candidate even when the target
is zero. These settings leave the graph recipe and cache keys intact.

Dense resident, count, backoff and disk policy also comes from `DenseSpec`.
Rust callers can pass complete `DenseSettings` to `DenseCache::with_settings` or
`Shared::open_with_dense_policy`. Snapshots retain immutable limits. The v1
daemon and direct CLI use the project `hybrid.cache.max_bytes` fallback for
the vector store. In v2, the cache root uses the shared `vector_cache.max_bytes`
budget from the global configuration.

ANN backfill pins use the same static eligibility as construction: chunk-row
count, minimum/maximum vectors, project capacity, model width and graph plus exact
row bytes. Small or otherwise ineligible corpora take no ANN pins; duplicate text
hashes do not reduce the chunk-row count used for admission or pin renewal.
Eligibility reserves no memory: the builder still checks actual resident and
live-worker charges before admission.
This check does not revoke pre-existing leases; their ownership and expiry
remain unchanged.

## Hybrid stages (opt-in, spec `docs/specs/hybrid-providers`)

Hybrid search is opt-in per project. Official `cli-full` binaries include both
providers; feature flags below describe custom source builds. Two providers exist:

- `openrouter` (spec `docs/specs/hybrid-providers`): remote embeddings +
  rerank; needs a build with `--features hybrid-remote` and
  `OPENROUTER_API_KEY` in the environment.
- `fastembed` (spec `docs/specs/hybrid-local`): local ONNX models; needs a
  build with `--features hybrid-local`, the ONNX Runtime installed
  (`docsbase connect fastembed`), and pinned registry models fetched
  (`docsbase models fetch fastembed/<name>`).

Provider entries (`[providers]`) live only in the global config; a project
may reference a provider but never define one. The default build stays fully
offline and any enabled hybrid configuration whose provider is unavailable
fails with an explicit admission error instead of silently falling back to
BM25.

```toml
hybrid = true          # shorthand: both stages with defaults

# or the full form; a missing stage section disables that stage
[hybrid]
enabled = true
ranking = "lexical"                # "lexical" | "rrf" (see below)

[hybrid.embedding]
provider = "openrouter"            # or "fastembed" (local ONNX)
model = "nvidia/nemotron-3-embed-1b:free"  # openrouter default; registry name for fastembed
dimensions = 2048                  # part of the vector cache key (registry-inferred for fastembed)
normalize = true                   # L2; part of the cache key

[hybrid.reranker]
provider = "openrouter"            # or "fastembed"
model = "nvidia/llama-nemotron-rerank-vl-1b-v2:free"  # openrouter default; registry name for fastembed
window = 50                        # candidate window per search (10..=100)

```

The global `vector_cache.max_bytes` bounds the content-addressed vector cache
in v2. A v1 source uses `hybrid.cache.max_bytes`. The default
(1 GiB) fits the 75k×2048 ANN target (~586 MiB); lowering it below the project's
working set keeps `embed_coverage` under 100% and therefore disables ANN
(ann-index FR-6 / hybrid-providers DELTA-9).

ANN admission is separate and **not configurable**: a background build is
admitted only when `graph + exact rows` fits the resident cap
`DenseCacheLimits.max_bytes` (1.5 GiB default, which covers the 75k×2048 peak
of ~1.22 GiB). A larger vector-cache budget leaves the ANN admission cap unchanged;
past roughly 92k chunks at `d2048` the project stays on the exact path.

Default remote pins are free OpenRouter routes: they may train on prompts and
the roster rotates monthly. Paid pins (e.g. `openrouter/qwen/qwen3-embedding-8b`,
`openrouter/cohere/rerank-v3.5`) are one explicit `docsbase models use` away and stay
pinned once set. Local models have no default: every fastembed stage must
name a registry row (`docsbase models list`), fetched first with `docsbase
models fetch fastembed/<name>`.

Models are pinned purpose-first, one stage per call, and every model is
addressed as `<provider>/<model>` — the provider is the segment before the
first `/`, so remote ids keep their own slashes
(`openrouter/nvidia/nemotron-3-embed-1b:free`). A bare name is rejected with the
corrected command; there is no provider inference:

```
docsbase models use embed  <provider>/<model> [--dims N]   [--local|--global]
docsbase models use rerank <provider>/<model> [--window N] [--local|--global]
docsbase models fetch  <provider>/<model> [--force]        # providers with a local cache
docsbase models remove <provider>/<model> [--force]
docsbase connect <provider> [--check] [--force]
```

`--dims` is optional only for a provider with a catalog (it is inferred from
the registry row and must agree); a provider without a catalog requires it.
`docsbase models list` shows provider states, the selected pins and per-model
cache sizes. `docsbase models remove fastembed/<name>` removes every cached revision of
a model and refuses while the model is loaded (pass `--force` to delete
anyway). `docsbase uninstall --dry-run` lists the owned artifacts — including
the ONNX Runtime install and the model cache (FR-13) — before any deletion.
Scope: inside a project the project `.docsbase.toml` is written by default
(`--local` optional); `--global` writes the machine config and is required
outside a project; `--path` overrides the project root. `--dry-run` prints
the merged table without probing or writing. The model is always an address
(`<provider>/<model>`); there is no provider inference.

Semantics: chunk vectors are cached content-addressed
(`openrouter/<model>@d<dim>@<l2|raw>` or
`fastembed/<name>@d<dim>@<l2|raw>@r<rev8>@p<prompt_version>`); opening a
project with incomplete coverage enqueues a background sync job
(`embed_coverage` in `status`);
searches rerank the candidate window (`max(limit, window)` candidates: plain
BM25, or under `ranking = "rrf"` the fused BM25 + dense list truncated to the
same size); `rerank=false` per request skips the rerank stage (the MCP
`search_docs` tool accepts the same boolean).

`ranking = "rrf"` (opt-in) fuses the BM25 window with a dense candidate list
(cosine over the cached vectors of the current revision, Reciprocal Rank
Fusion with a rank constant of 60 by default) before the rerank stage runs on top of the fused order. It
requires the embedding stage and stays `"lexical"` by default: switching the
ranking is a measured, deliberate change (NFR-6). Without an active rerank
stage the exposed `score` is the RRF score (rank-based scale, not comparable
to BM25 values); chunks without vectors are simply absent from the dense list
until the backfill completes. `rerank = false` disables only the rerank
stage - the order is then whatever `ranking` selects (`"lexical"` = strict
BM25). [RRF settings](#reciprocal-rank-fusion)

## Local model and runtime provisioning

Use `[provisioning.models]` for `docsbase models fetch` and
`[provisioning.runtime]` for `docsbase connect fastembed` in a global v2 file.
Both commands resolve the policy before download. The values affect
installation work and leave the identity and validity of installed artifacts intact.

| Field | Models default | Runtime default |
|---|---:|---:|
| `max_file_bytes` | 2 GiB | |
| `max_total_bytes` | 3 GiB | |
| `max_archive_bytes` | | 256 MiB |
| `max_library_bytes` | | 512 MiB |
| `headroom_bytes` | 256 MiB | 256 MiB |
| `connect_timeout_ms` | 30000 | 30000 |
| `response_timeout_ms` | 120000 | 120000 |
| `body_timeout_ms` | 300000 | 600000 |
| `lock_attempts` | 50 | |
| `lock_retry_delay_ms` | 20 | |

Set size limits in bytes up to the defaults shown. The model total must
cover its per-file limit. Free-space admission requires 1.5 times the
pinned download size for models or the archive plus library sizes for
the runtime, then adds the configured headroom. Set headroom to at least
268435456 bytes. HTTP timeouts are positive and downloads stay on HTTPS.
The model installer allows one or more exclusive lock attempts and zero
delay between attempts. Use
`docsbase config effective` to see the source of each value.

## Example

Global `config.toml`:

```toml
max_file_size = 2097152
max_docs_per_project = 20000
auto_index = false
hybrid = false   # true = hybrid stages (per-provider build feature)
ignores = ["drafts/**", "*.tmp.md"]

# Providers live only in the global config (never in a project file).
# fastembed: after `docsbase connect fastembed`; runtime_path is optional
# (the data-dir install is used otherwise).
[providers.fastembed]
enabled = true
# runtime_path = "/opt/onnxruntime/lib/libonnxruntime.so"
```

Project `.docsbase.toml` (layered on top of global):

```toml
ignores = ["internal/**"]
```

`.docsbaseignore`:

```gitignore
archive/**
*.draft.md
```

## Inspect

```sh
docsbase config   # effective values for the current directory (JSON)
```

```json
{
  "ignores": [],
  "max_file_size": 1048576,
  "max_docs_per_project": 20000,
  "auto_index": false,
  "hybrid": false
}
```

## Rules

- Zero limits are rejected at load (`Admission` error).
- Switching the cache root is only safe with the daemon stopped
  (`docsbase daemon stop` first).
- Daemon reads the global config once at start; project config is read when
  the project is opened.
- Provisioning changes need a daemon restart: `docsbase connect fastembed`
  (runtime library) and `docsbase models fetch fastembed/<name>` (a revision that
  actually landed on disk) both print a restart note, because a running
  daemon keeps the ORT library and its loaded model instances until it is
  restarted.
- Local inference memory: the embedder holds one ORT session per cache key
  (~model size, int8 rows are the small case referred to by NFR-4). The
  reranker additionally holds the ONNX bytes in RAM while its session is
  built (`OnnxSource::Memory`, which is what makes load-time sha256
  verification possible), so a 1.1 GiB fp32 reranker such as
  `bge-reranker-base` peaks at roughly twice its file size during load.


## Configuration v2

A new installation uses v2 when the global `config.toml` is absent. Reading
settings requires no file creation. The first global write from `connect` or
`models use --global` records the version explicitly. To set the shared
cache budget by hand, create a global file like this:

```toml
config_version = 2

[vector_cache]
max_bytes = 536870912
max_entries = 250000
busy_timeout = 5000
```

All projects in the same cache root share this budget. SQLite opens the vector
database on the first vector operation. Lexical indexing and search can run
without creating it.

The whole `[vector_cache]` table is global-only. Each omitted field keeps
its default.

| Key | Default | Behavior |
| --- | ---: | --- |
| `max_bytes` | 1073741824 | Positive limit on retained vector payload bytes. |
| `max_entries` | 1000000 | Positive limit on retained vectors across all projects and models. |
| `busy_timeout` | 5000 | SQLite contention budget in milliseconds, from 0 (immediate) through 2147483647. |

An existing unversioned global file selects v1. Create an empty unversioned
global file to retain the project-specific budgets of legacy projects. To use
v2 with those projects, move `hybrid.cache.max_bytes` to the global
`vector_cache.max_bytes`. A project can opt into field-wise hybrid inheritance
by setting `config_version = 2` in its `.docsbase.toml`. An unversioned project
file keeps the whole-value hybrid replacement rule. Use
`docsbase config effective` to check the selected values and their origins.

With v2, `docsbase index` and `docsbase sync` use the running Shared daemon
for both existing and new projects. On a fixed Website daemon, use
`docsbase sync ABS`. Stop and restart the daemon after edits to the
global config. Restart any running MCP frontend when the global version changes.
Use a separate cache root to try v2 alongside an active v1 setup.
`models use` and its dry run resolve a v2 edit against
the same inherited settings as the runtime. Unedited fields in a table remain
inherited. When replacing one stage in a boolean reset, the editor keeps the
effective state of the other stage. The migration command still produces a
read-only plan.

On Shared v2, connected `docsbase index` requires its own job so it uses
fresh project settings. If a sync for that project is active, wait for it to
finish and retry `docsbase index`. Ordinary `sync` can join the active job.

### SQLite connections and metadata readers

Set these global values in a v2 file. Busy timeouts and WAL retry delay
use milliseconds.

```toml
[store]
write_busy_timeout = 30000
read_busy_timeout = 5000
wal_attempts = 3
wal_retry_delay = 100

[read_pool]
max_connections = 4
```

The daemon and direct index, search and sync commands capture the
connection policy at startup. The daemon and direct search prepare the
configured number of read connections. Zero busy timeout requests
immediate SQLite busy errors. WAL negotiation requires at least one
attempt and accepts zero delay. The read pool requires at least one
connection. The pool opens every connection during construction, so choose
a size within the process's file descriptor and memory limits. Exhausting
these resources can fail startup even when the count is representable.

Restart the daemon to apply a global change. Connection policy is
independent of persisted index formats. With the Shared daemon stopped,
`status` and `list` use their standalone SQLite policy even if `[store]`
or `[read_pool]` is invalid. The Website profile still requires the daemon.

### Watcher debounce

Set these fields in a v2 global or project file. Each project field can
override its global value independently.

```toml
[watch]
debounce_ms = 1500
max_debounce_ms = 2000
```

The watcher waits up to `debounce_ms` after the last event. It starts
flushing once `max_debounce_ms` elapses from the first received event.
Indexing still depends on the downstream queue and writer. Both fields must be at least 1 millisecond,
and the quiet window must fit within the maximum of 2000 milliseconds.
When the project config is reopened, the next batch uses the updated windows.

Watcher queues, retries, and directory expansion are also configurable in v2.

| Field | Default | Scope |
|---|---:|---|
| `native_queue_capacity` | 4096 | Global |
| `batch_queue_capacity` | 4096 | Global |
| `transient_retries` | 3 | Global or project |
| `transient_backoff_ms` | 100 | Global or project |
| `lease_retry_ms` | 250 | Global or project |
| `max_expand_files` | 10000 | Global or project |
| `max_expand_depth` | 32 | Global or project |

Queue capacities count pending path notifications and debounced batches,
respectively. Each accepts 1–4096 and applies when the watcher starts.
Restart the daemon to apply global file changes. Lower counts apply
backpressure sooner. They leave the native filesystem's event buffer and
search payload budgets independent.

A batch keeps the retry policy it accepted at the start. Transient retries
accept 0–21, with zero surfacing the first transient failure in status. The
delay triples after each transient retry and must fit the platform clock.
A busy writer lease uses a separate positive delay and leaves the transient
count intact.

For directory events, `max_expand_files` limits expansion to 1–10000 batch
paths, counting paths already selected from earlier events. The event directory
is at depth zero and `max_expand_depth` accepts 1–32. Direct file events and
manual sync retain their own limits. If expansion reaches a limit and skips
paths, the daemon records a warning in the indexing outcome. Run
`docsbase sync` to reconcile the project.

### Lexical search

Use `[lexical]` in the global or project v2 configuration to tune BM25
scoring and how it collects candidates. A project can override individual
fields.

```toml
[lexical]
text_boost = 1.0
title_boost = 2.0
heading_boost = 1.5
identifiers_boost = 2.5
long_chunk_penalty = 0.5
initial_fetch_multiplier = 4
initial_fetch_additive = 16
widening_multiplier = 4
```

These values are the defaults. The title field indexes the outermost
heading in a chunk's heading path. Weights must be finite and nonnegative
as 32-bit floats, with at least one positive weight. Tantivy treats weights
within one float epsilon of 1.0 as 1.0. A zero weight keeps matches eligible
and contributes zero score. A query reports an error if its scores or its
explicit syntax boosts overflow.

The long-chunk penalty multiplies BM25 scores for chunks longer than 1650
Unicode characters, the default chunk cap plus overlap. Its value must be
greater than zero and at most one. Use `1.0` to disable the penalty.

The initial lexical collection window is the larger of the requested
candidate count times `initial_fetch_multiplier` and the count plus
`initial_fetch_additive`, capped by the number of indexed chunks. If an unseen
candidate can still beat the lowest final score in the requested window,
the collector grows it by `widening_multiplier` and tries again. The initial
multiplier must be at least 1, the additive count must be nonnegative,
and the widening multiplier must be at least 2.

Reopen the project to apply project settings in a running daemon. These
settings apply to queries on the existing indexes and vector cache.

### Reciprocal rank fusion

Projects using `ranking = "rrf"` under `[hybrid]` can tune fusion with
`[rrf]` in a global or project v2 source. Each project field can override
its corresponding global value.

```toml
[rrf]
rank_constant = 60
lexical_weight = 1.0
dense_weight = 1.0
```

The defaults weight both lists equally. A candidate contributes
`weight / (rank_constant + rank)` for each list where it appears.
Rank starts at one. A smaller constant gives the first few ranks more
influence. Both weights must be finite positive 32-bit floats. The rank
constant must be a positive integer. The weights are independent
multipliers and do not need to sum to one.

Reopen the project to apply the settings in a running daemon. The new
scoring uses the existing lexical index, vectors, and ANN graph.

## Versioned source planning API (CF3)

For offline tooling, `config::source::SourceLayer::parse` accepts supplied TOML
text with an independent version per file (absent/1 or 2), and
`config::resolve::preview_sources` resolves it without reading paths, credentials
or model files. Every preview reports `runtime_activation_available = false`.
V1 replaces the entire hybrid value. V2 hybrid tables inherit absent fields;
a stage table enables or updates it, `false` disables it, and whole `hybrid`
bools retain the v1 reset semantics. An existing stage inherits provider, model
and dimensions independently; changing provider does not silently reset the
other fields. `enabled` also inherits when absent.

V2 source previews resolve the global `[vector_cache]` policy. Project overrides
of that table and v2 `hybrid.cache` are rejected. An explicit
legacy project cache override under a v2 global source is reported as a scope
conflict. The preview uses the shared budget from the global configuration.
V2 reports the three vector cache fields with daemon scope. V1 reports
the legacy project-scoped key `hybrid.cache.max_bytes`.

`config::migrate::draft_migration` takes an explicit list of registered project
config paths, loaded/observed-missing sources and a budget decision. Uninspected
projects block a complete proposal. The suggested maximum includes v1 effective
budgets or, under a v2 global, its capacity and explicit legacy conflicts; v2
bool resets do not reintroduce the old default budget. A caller must choose
`UseBytes` or `AcceptSuggestedMaximum`; resource changes remain visible.

Drafts expose original text/hash and canonical proposed TOML for review. They
warn that comments and formatting may be lost. V1 hybrid tables materialize
current effective values, while bool shorthand and existing v2 inheritance are
retained. All proposals are parsed and resolved again to check non-resource
settings. Observed-missing project files remain missing. Source/edit debug
output omits file text; explicit before/after views should be treated as source
files. The draft has no side effects. Automatic application and recovery are
later #78 milestones.

## Effective values with origins

```sh
docsbase config effective --format=json
```

This versioned view resolves the current global/project files and shows each
file setting's typed value, unit, scope and source. It does not contact the daemon:
a running process can still hold its earlier configuration. The output therefore
identifies its basis as `source_resolution`, sets `live_runtime_observed` to
`false`, and limits coverage to `file_settings`. Bare `docsbase config` keeps its
existing JSON format.

`view_version` is `1`. Fields appear in key order; for example:

```json
{
  "key": "search.request_timeout_ms",
  "value": 30000,
  "source": { "kind": "defaults" },
  "scope": "daemon",
  "unit": "milliseconds",
  "capability": null
}
```

Sources distinguish defaults, global/project paths and supplied snapshots whose
original sources are unavailable. A disabled stage has null values. Provider
capabilities report compiled support from the existing registry; they do not
prove that credentials, models or runtimes are available. `provisioning_checked`
is `false`: the command does not probe providers, load models, start a daemon or
open a database.

Provider records expose only fields selected by their owner. Arbitrary parameters
are omitted and indicated by `unexposed_parameters`; secrets and raw configuration
text are not printed. For FastEmbed the public projection includes `runtime` and
`runtime_path`. Invalid input produces an error without echoing the source text.
The library can also render an explicitly supplied CF3 preview with its v2
budget key and scope.


## Project configuration plan

```sh
docsbase config plan
docsbase config plan ./project --check
```

The command compares the current v2 sources with the lexical and embedding
recipes of the selected project generation in a stopped v5 catalog. Changing
chunk size or the embedding model can require replacing the generation.
Query-only settings such as `lexical.long_chunk_penalty` leave the recipe intact.
The preview covers lexical and embedding artifacts. It reports the latest
recorded coverage through `full_survey`. Markdown freshness, vector cache
coverage, active daemon settings, and ANN graph settings require separate
observations.

The JSON report includes the bounded action graph and a `plan_id` bound to
the observed sources, installation and generation. A document-vector action
prepares any missing entries using the new recipe. Existing entries with
compatible keys remain reusable. The `remote` grant marks possible provider
usage at application time. The preview reads existing files without starting
the daemon, loading models, or contacting providers. For an unavailable registry
it reports `observation_failure`, and an existing legacy pair without recipe
evidence produces an `unsupported_identity` blocker. Invalid effective settings,
including hybrid stages in a Website project, produce `invalid_settings`.
An embedding action whose provider is absent from the build produces an
`executor_unavailable` blocker.
Source and catalog observations capture their identities at the time of each read.

By default a completed report exits with code 0. With `--check`, exit code 3
means that the configuration requires replacing the selected generation,
5 means the check could not establish the relevant facts, and 0 means the
observed state requires no replacement. A missing index is a first build.
Use the explicit `index` command to publish a generation from current settings.

### Apply a reviewed plan

```sh
docsbase config apply ./project --plan-id '<plan_id>'
docsbase config apply ./project --plan-id '<plan_id>' --allow-remote
```

Stop the daemon and pass the ID from a fresh `config plan` report. The command
rechecks the sources and the selected v5 generation, then uses staged indexing
to prepare and publish a replacement. A current plan with no actions returns
`"applied": false`. A completed replacement returns `"applied": true` with
its project ID and indexing statistics.

Exit code 4 means the ID differed at preflight. Exit code 5 means execution
was blocked by the plan, the daemon had admission, or the plan required
permission for possible OpenRouter embedding requests.
`--allow-remote` grants that permission for this invocation. FastEmbed uses
explicitly provisioned models. If configuration, indexed inputs, or the
selected generation changes during preparation, the replacement can fail with
an execution error. The selected generation remains usable on a failed
preparation. Compatible vectors already fetched during an authorized attempt
can be reused on retry.

### Inspect the running daemon

```sh
docsbase config plan ./project --live --check
```

This mode reads a registered v5 project's active settings and selected
generation through the running daemon. It leaves project registration and
runtime unchanged. `restart_required` compares the current global file with
the daemon's startup source. A query-only policy edit can change
`project_runtime` without replacing the index. A changed construction recipe
can require replacing a confirmed ANN graph while reusing vectors.
If a source edit also requires a new index, the plan orders its replacement
before reconfiguring the project or restarting the daemon.
When the active graph is cold, the planner can report `ann.state = "persisted"`
and the size of its dump files. It looks for a slot with the active index and
recipe before hashing chunks from the leased SQLite generation. The digest
scan uses `search.max_request_payload_bytes` and the live request deadline.
The request uses `daemon.control_timeout_ms`, capped at the IPC exchange
timeout. The SQLite read allows a full supported chunk.
Eviction leaves the plan identity unchanged when the slot still qualifies. The
inspection checks file presence, lengths and provenance without loading HNSW.
Unconfirmed slots and absent runtime provenance report `unknown`.

In live mode `--check` returns 3 for a reported change or restart and 5 when
observation is blocked. Apply a live project runtime plan by its exact ID.

```sh
docsbase config apply ./project --live --plan-id <id>
```

The daemon checks the reviewed sources, active runtime and published index
revision before changing project settings. You can apply changes such as
lexical scoring and ANN traversal while retaining the same indexed data.
Stale reviews exit with code 4. Plans requiring a daemon restart or an
unavailable executor exit with code 5. Use a stopped plan with `config apply`
for an offline replacement.

### Start after configuration changes

```sh
docsbase serve --on-invalidation fail
docsbase serve --on-invalidation prompt
docsbase serve --on-invalidation apply --allow-remote
```

Before startup, v5 Shared checks the registered projects. Website checks its
fixed project. First startup and a restart with compatible recipes need no
approval. A foreground terminal defaults to `prompt`. A noninteractive
launch, including MCP autostart, defaults to `fail`.

`fail` exits with code 3 when a replacement is required. `prompt` displays the
plans and asks for consent before starting the service. A required prompt
without a terminal exits with code 5. `apply` accepts reviewed local replacements.
Remote embedding requires `--allow-remote` or separate interactive consent.
All approvals are collected before the first replacement. Each project is
published independently, so a later execution failure can leave earlier
approved replacements complete. Rerun the preview to review the remaining work.
A stale plan exits with code 4 and an unavailable plan or missing consent uses
code 5.

On an ordinary restart after interruption, the startup command can
checkpoint already committed SQLite transactions under stopped-daemon
admission before it checks the configuration. This can change the registry
file and remove its recovery sidecars even when the configuration then
requires a replacement and startup refuses. Recovery preserves published
generations and does not build indexes, open vector storage or call providers.
Explicit policies and dry-run planning report unsettled state without
changing SQLite.

The remote grant covers the reviewed startup work. Opening an indexed project
on a managed daemon leaves vector repair to an explicit Index or Sync.
Older catalog formats retain their ordinary startup when no policy flag is
passed. Existing indexes require a v5 catalog for an explicit startup review.

## Observed migration plan (CF4c2)

```sh
docsbase config migrate --to 2 --format=json
docsbase config migrate --to 2 --accept-suggested-vector-budget
docsbase config migrate --to 2 --vector-max-bytes 536870912
```

This command reads the selected config/cache roots and the complete persisted
project registry. It includes registered roots that no longer exist; current cwd
and active daemon sessions are not the project inventory. Missing config files
remain distinct from empty files. It does not create directories, locks, databases
or sidecars, write configuration, start a daemon, load models or call providers.
The command accepts both source versions for planning.

The version 2 JSON envelope contains `observation_id`, selected roots, registry,
`registry_stable`, all source states/revisions, the budget decision, a summary and
the versioned `plan` action graph. The summary shows observed/proposed numeric
budgets, legacy scope conflicts, source rewrites with target versions/hashes and
formatting/materialization warnings. It omits original/proposed TOML, comments,
models, provider parameters and other arbitrary configuration values. File paths
and revision hashes remain visible. Canonical migration may discard comments;
non-resource effective values are checked for equivalence by the pure planner.

Without a budget choice, `vector_budget_decision_required` blocks rewrites.
`--accept-suggested-vector-budget` explicitly selects the largest observed legacy
capacity; `--vector-max-bytes` selects a positive capacity instead. These flags
conflict and neither grants permission to apply. Even a complete source proposal
retains `strict_store_adoption_unavailable` and `executor_unavailable` blockers.
For a new v2 installation, configure it directly as described above.

A busy/WAL/recovery-bearing registry is unavailable, not empty. The command never
stops a daemon or repairs storage. Unavailable registry/source observations block
all source rewrites. Invalid semantic settings or exceeded envelope/context limits
produce a redacted error and nonzero exit; a successfully emitted blocked plan
returns exit 0. No prompts or partial JSON are emitted on planning errors.

Each existing source must be a regular UTF-8 file of at most 1 MiB with one hard
link. Multiple links at the first open report `multiple_links`. A link added
during the read and detected on recheck reports `changed`. If the filesystem
cannot report file identity or link count, the source reports
`identity_unavailable`. Each condition blocks the complete proposal, as do final
symlinks and special files. Parent aliases are resolved and checked around reads.
Official planning limits are 256 projects, 1 MiB path/registry metadata, 16 MiB aggregate source
bytes and 8 MiB encoded plan/output. Registry SQL uses a 1 MiB value limit and 256 KiB
soft page-cache target. These bound logical inputs/output, not exact RSS or time.
The library takes explicit alternate `PlanningLimits`; these are planning policy,
not runtime/search configuration knobs.

Each file is read independently, with conservative metadata race checks. The
registry is observed again after source reads; a difference blocks proposals.
`atomic_snapshot=false` is intentional: equal boundary observations do not prove
absence of intermediate changes. The identity binds context, registry, every input
source (including unchanged files), choice and output, not future freshness. Future
apply must revalidate all those facts under its own publication protocol. Source
byte revisions (`source-bytes-v1`) describe exact TOML bytes. For every loaded
source, `precondition` (`observed-source-v2`) also binds the native file ID and
the journal's basic file attributes. On Unix these are the owner, group and
permission/special mode bits. On Windows they are the Win32 file attributes.
For example, changing mode from 0600 to 0640 changes both `plan_id` and
`observation_id` even when the proposed edits are identical. A change detected
between the two opens reports `changed` and blocks the proposal. Re-run the
command to refresh an earlier review. The JSON envelope stays at version 2.
ACLs, extended attributes and alternate streams are outside these basic profiles.
File IDs may be reused after deletion, and observations expire as files change.

## Local Web panel

See [WEB.md](WEB.md) for `docsbase web --open`, session security, Web settings,
source-build frontend preparation and diagnostics. Official cli-full includes
Web; Cargo default stays lean.


### ANN traversal policy

Use `[ann.search]` in the global config or a project file. Omitted project fields
inherit the global values. Each field and its origin appear in
`docsbase config effective`.

```toml
[ann.search]
ef_base = 64
ef_max = 4096
fetch_multiplier = 4
```

With an explicit table, the ANN traversal uses the larger of `ef_base` and
`fetch × fetch_multiplier`, capped by `ef_max`. The domain is
`1 <= ef_base <= ef_max <= 4096` with a positive multiplier. Candidate windows
above `ef_max` use exact streaming without loading or building a graph for that request. Changing
these values keeps the existing graph and vectors. A project without an
explicit or inherited table retains the existing ANN behavior.

### ANN construction

`[ann.build]` sets the HNSW graph recipe. Global settings supply defaults for
projects. A project can override either field.

```toml
[ann.build]
m = 16
ef_construction = 200
```

`m` accepts 8 through 16. `ef_construction` accepts twice `m` through 200.
The persisted HNSW format uses 16 layers. `docsbase config effective` shows
the two resolved values and their sources.

After a project config reload, the daemon selects a graph with the new recipe
or builds one using its saved vectors. It reuses the lexical index and embedding
keys. Search uses exact results while the matching graph is unavailable.
Direct CLI search can load a matching graph and otherwise searches the vectors
directly. Global config edits apply after a daemon restart.

### ANN lifetimes

`dense.backfill_pin_ttl` must be a positive multiple of 1000 ms. The daemon
uses it for new backfill pins and successful renewal. Existing leases keep
their expiry until renewal. `dense.ann_outcome_ttl` must be positive and controls
how long the daemon reports an ANN outcome as current. Status measures its age
using a monotonic clock. Both fields default to one hour.


### Shared vector-store construction policy

The library exposes `vector_cache::VectorStore` for explicit shared ownership.
Resolve `vector_cache::settings::standalone()`, or provide a complete validated
`VectorStoreSettings`, acquire the cache root once, then call `connect()` for
independent connections. Entry/byte limits and SQLite timeout belong to the owner;
connections cannot override them. Defaults remain 1,000,000 vectors, 1 GiB of
vector payload and a five-second busy timeout. Strict capacities must be positive;
timeout zero means immediate failure on writer contention.

Constructors also use that busy timeout as one shared contention/retry window.
After mode ownership and before SQLite opens, a persistent `vectors.init.lock`
native lease serializes WAL, schema and constructor sequence initialization for
the same root. Independent roots remain independent. Waiting for this lease uses
the same window as SQLite journal-mode negotiation; expiry refuses admission
before SQLite opens. An immediate SQLite BUSY may retry until its original deadline;
time already spent inside SQLite counts, and each remaining wait is capped.
Zero makes one immediate attempt, while other SQLite errors propagate directly.
A BUSY returned after expiry starts no further attempt. A running synchronous
SQLite call is not preempted; its success is retained even if it finishes later.
Successful initialization restores the configured timeout for normal operations.
No extra retry setting, schema migration or lease-format change is introduced.

An owner and its connections retain an exclusive native `vectors.lock` lease.
Current legacy `VectorCache::open/open_with_limits` connections retain shared
leases, so either mode rejects the other before SQLite opens. Clones and live
connections retain ownership after the original owner drops. Failed opens release
only their own resources. The coordination file remains after release; never
remove or replace it while a cache is in use. After all handles close, a legacy
caller can open the store again. This advisory protocol covers cooperating current
callers, not older binaries, raw SQL or external cache/lock file replacement.

Owned connections share LRU recency restored from persisted rows. Writes and
resulting eviction, and lease release with resulting eviction, commit atomically.
Leased rows can temporarily exceed eviction limits; vector payload limits are not
physical database/WAL/disk or RSS quotas. Provider calls hold no SQLite transaction.

Explicit v2 startup uses this ownership policy. Legacy budgets and zero-cap
semantics belong to v1. Automatic adoption via a migration plan still reports
strict-store-adoption and executor blockers.

The experimental Rust consumer APIs can now receive that owner explicitly:
`DenseCache::with_vector_store` and `VectorCacheSource::Owned` retain its lifetime;
`hybrid::jobs::{embed_texts_from, embed_project_chunks_from, coverage_for_key_from}`
use the same connection policy. Dense cold loads, ANN worker lease release and
full/scoped/streaming retrieval all reuse the owner. A different cache root is
rejected before provider work. `DenseCache::set_vector_cache_max_bytes` now returns
`Result<()>`: owner-backed caches reject project budgets, including equal values,
without changing policy or published config. Legacy adapters still accept them.
The daemon selects this owner from its v2 global snapshot, and sync and status
use that snapshot for vector work.

### Consent policy library foundation

The experimental `config::policy` API resolves explicitly supplied launch facts
and consent policy without terminal I/O. Legacy v1 startup remains unchanged.
Managed Auto prompts only for a direct CLI with a TTY; services, MCP and autostart
cannot consent to rebuilding through an inherited terminal or explicit Apply.

`ConsentReview::compile` owns a bounded canonical plan from component assessments.
It evaluates no-consent initialization, Fail, explicit Apply grants, or a prompt
request. Remote expense, downloads and destructive changes have independent
permissions. Answers bind the displayed plan ID; receipts must be checked against
newly observed and compiled state. Changed identity is refusal4, required change
under Fail is3 and other consent refusals5; existing commands gain no new exits.

Consent satisfaction only records consent. Every plan blocker remains, including
missing executors and strict-store adoption. This API does not observe freshness,
apply changes, write source files, start providers or activate configuration v2.
An executor must enforce admission, capabilities, blockers and recovery separately.


## Website service configuration

`docsbase serve --project ABS` selects the complete lexical Website profile over
the global profile; `--profile shared` selects complete Shared. Website requires
an existing indexed canonical root, rejects hybrid/provider policy, and owns
watcher-off/no-idle invariants. Explicit Shared grace0 remains valid; Website
cannot take an idle-grace override. Daemon resources remain complete restart-time
`[daemon]` policy; they do not become HTTP settings.

To initialize v5 for a new lexical Website deployment on a supported
filesystem, select the project in the global configuration before the first
`docsbase index ABS`:

```toml
config_version = 2

[daemon]
profile = { mode = "website", project_root = "/absolute/path/to/project" }
```

Use the configured path for `ABS`. The first index creates the catalog, and
`docsbase serve` binds to the published project. Existing registries retain
their format.

The [Node HTTP example](../examples/website-search/README.md) has a separate
complete application-owned JSON policy, passed through `DOCSBASE_WEBSITE_CONFIG`.
Its config owner validates every HTTP count/byte/timer and constructs the existing
SDK's immutable fixed-project client once, with no fallback operational values.
This does not activate v2 configuration or change index/vector/ANN invalidation.
Use explicit `docsbase sync ABS` and read-only `docsbase status --project ABS` for
updates/readiness; watcher-off filesystem freshness is not inferred.

## Markdown chunk settings

Set `[index.chunk]` in global `config.toml` or project `.docsbase.toml`.
The project can override one setting and inherit the other. Both settings
count Unicode characters.

```toml
[index.chunk]
max_chars = 1000      # 1–1500, default 1500
overlap_chars = 100   # 0–150, default 150
```

The prose overlap is capped at one tenth of `max_chars`. A heading merged
with its prose can use the full overlap allowance. Fenced code and tables
remain atomic. `docsbase config effective` reports both values and their sources.

After changing the recipe, run `docsbase sync ABS` to update the entire project.
A watcher batch applies the recipe to the files it processes. Website readiness
requires a full sync at the current recipe and reports `recipe_changed` when
the last full sync used a different one. Embeddings for identical chunk text
and the same embedding model remain reusable.

On upgrade from index schema 3, the next full sync records the recipe and
recomputes chunks. Existing index files are retained until the index job
updates them. For an existing Website deployment, stop the daemon and run
`docsbase index ABS` once before restarting `docsbase serve --project ABS`.
Website launch checks the schema before opening the index for writes.

## Lexical index writer settings

Set `[index.writer]` in global v2 `config.toml`. Omitted fields keep the
defaults shown here.

```toml
[index.writer]
heap_bytes = 20000000
worker_threads = 1
merge_threads = 4
staging_disk_bytes = 536870912
```

The heap is divided evenly among indexing workers. Each worker must receive
at least 15,000,000 bytes and fewer than 4,293,967,295 bytes. Both thread
counts are between 1 and 8. The heap is Tantivy's per-writer indexing arena
target. Search buffers, merge memory and the rest of the process have their
own resource use.

Direct index and sync commands apply the current global settings. Restart
the daemon to apply them to daemon jobs and watcher updates. These settings
leave chunk recipes and existing indexes intact. `docsbase config effective`
reports each value and its source.

`staging_disk_bytes` reserves logical file bytes for one unpublished lexical
pair in an initialized v5 catalog. The default is 512 MiB. The staged
metadata and Tantivy writers enforce the reservation during the build. Direct
`docsbase index` uses this policy together with the existing retained
indexing payload budgets. Open SQLite registry
readers keep their snapshots while a later search sees the newly published
pair. A rejected build leaves
the old head available. A later index retries cleanup of its retired local pairs once their readers
release them. A fresh lexical Website deployment can initialize v5 through
the global profile above on a supported Unix filesystem or on writable
local NTFS on Windows. A fresh Windows cache outside the user's profile
and LocalAppData also needs permission to flush its parent directories.

Shared with v2 configuration initializes a fresh v5 catalog at startup or on
the first stopped Index on supported Unix filesystems and writable local NTFS
on Windows. An existing v4 catalog keeps its schema. On Windows,
a cache on another filesystem or a network share can continue using v4.
For publication from an unsupported cache location, move the v5 cache to a
writable local NTFS directory while the daemon is stopped. Index and Sync
prepare the replacement pair before switching the active head. A complete
lexical pair can be reused after a fresh scan confirms the same Markdown and
index recipe. The first Index can publish verified files with warnings about
omissions. A replacement requires a complete survey and keeps the selected
generation if any source file could not be inspected. Website readiness stays
partial until a complete survey succeeds. A watcher retry can rebuild a
generation left pending by a failed commit. With embeddings enabled, the watcher
prepares vectors before applying edits. After enabling or disabling
embeddings or changing the embedding recipe, run `docsbase index` to
publish a matching generation. In a managed daemon, explicit Sync repairs
missing vectors without requiring a configuration migration. The coverage counter reports
persisted cache keys. Search and refill also validate the vector payload.
Stopped `docsbase status` and `docsbase list` report the selected generation's
metadata.
