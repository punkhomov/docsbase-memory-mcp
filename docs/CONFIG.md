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

## Query-vector cache policy

The query cache retains up to 256 vectors and 4 MiB of vector payload by default.
Both limits apply to the same shared cache; hits refresh recency and insertion
evicts the least recent entries. Larger vectors can serve a request without
being retained. This byte count excludes keys, allocator overhead, externally
held vector references and in-flight requests.

Rust callers can project `DenseSettings::query_cache_settings()` and pass a
validated `QueryCacheSettings` to the explicit cache or daemon constructors.
Both capacities must be positive. CLI search and daemon startup receive the
owner's defaults at their composition boundary. These settings have no TOML
keys yet; changing a live cache's policy is a later configuration task.

Dense resident, count, backoff and disk policy also comes from `DenseSpec`.
Rust callers can pass complete `DenseSettings` to `DenseCache::with_settings` or
`Shared::open_with_dense_policy`; snapshots retain immutable limits. At daemon
and direct CLI composition, the existing `hybrid.cache.max_bytes` still overrides
the dense-path vector-store fallback. This does not activate strict shared-store
quotas or live settings updates.

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

[hybrid.cache]
max_bytes = 1073741824             # vector-cache budget; ANN needs full coverage
```

`hybrid.cache.max_bytes` bounds the content-addressed vector cache. The default
(1 GiB) fits the 75k×2048 ANN target (~586 MiB); lowering it below the project's
working set keeps `embed_coverage` under 100% and therefore disables ANN
(ann-index FR-6 / hybrid-providers DELTA-9).

ANN admission is separate and **not configurable**: a background build is
admitted only when `graph + exact rows` fits the resident cap
`DenseCacheLimits.max_bytes` (1.5 GiB default, which covers the 75k×2048 peak
of ~1.22 GiB). Raising `hybrid.cache.max_bytes` does not admit a larger corpus;
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
Fusion `k = 60`) before the rerank stage runs on top of the fused order. It
requires the embedding stage and stays `"lexical"` by default: switching the
ranking is a measured, deliberate change (NFR-6). Without an active rerank
stage the exposed `score` is the RRF score (rank-based scale, not comparable
to BM25 values); chunks without vectors are simply absent from the dense list
until the backfill completes. `rerank = false` disables only the rerank
stage - the order is then whatever `ranking` selects (`"lexical"` = strict
BM25).

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

To opt in, put the version at the top of the global `config.toml`:

```toml
config_version = 2

[vector_cache]
max_bytes = 536870912
```

This works on a new installation without a prior migration. The global vector
budget applies to all projects in the same cache root. Enable field-wise hybrid
inheritance in a project by putting `config_version = 2` at the top of its
`.docsbase.toml` after opting in globally. Runtime v1 global files omit the
version key. Under v2 global policy, legacy project files retain their whole
hybrid replacement semantics.
Remove any project `hybrid.cache.max_bytes` when opting into v2 globally. Use
`docsbase config effective` to verify the shared setting and its origin.

Run `docsbase index` with the daemon stopped. With the daemon running, use
`docsbase sync` for both existing and new projects. Stop and restart the daemon
after edits to the global config. Use a separate cache root to try v2 alongside
an existing v1 setup. `models use` and its dry run resolve a v2 edit against
the same inherited settings as the runtime. Unedited fields in a table remain
inherited. When replacing one stage in a boolean reset, the editor keeps the
effective state of the other stage. The migration command still produces a
read-only plan.

### Lexical weights

Use a `[lexical]` table in the global or project v2 configuration to tune
the field weights used by BM25. A project can override individual fields.

```toml
[lexical]
text_boost = 1.0
title_boost = 2.0
heading_boost = 1.5
identifiers_boost = 2.5
```

These values are the defaults. The title field indexes the outermost
heading in a chunk's heading path. Weights must be finite and nonnegative
as 32-bit floats, with at least one positive weight. Tantivy treats weights
within one float epsilon of 1.0 as 1.0. A zero weight keeps matches eligible
and contributes zero score. A query reports an error if its scores or its
explicit syntax boosts overflow. Reopen the project to apply project settings in a running daemon.
The stored index and vector identities are independent of these weights.

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

V2 source previews place the vector budget in global `vector_cache.max_bytes`.
Project overrides of that key and v2 `hybrid.cache` are rejected. An explicit
legacy project cache override under a v2 global source is reported as a scope
conflict. It does not change the strict global budget in the preview. V2 provenance
uses `vector_cache.max_bytes` with daemon scope; v1 retains the legacy
project-scoped key. The preview settings retain the compatibility Rust layout.

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


### ANN lifetime construction policy

The internal typed DenseSpec also owns the existing one-hour backfill-pin and
ANN-status windows. AnnLifetimes validates positive whole signed seconds for pins
and a positive Duration for status before construction; partial typed overrides
preserve the other value. The policy follows actual dense cache/sync ownership,
with no new file keys or live mutation. Both initial backfill pins and successful
renewal use it. Existing leases keep their expiry/ownership; this does not revoke
leases or invalidate vectors, query caches or ANN graphs. Status uses the cache's
monotonic authority window and describes stale observations as older than the
configured TTL. Compatibility helpers retain their one-hour defaults.


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

The [Node HTTP example](../examples/website-search/README.md) has a separate
complete application-owned JSON policy, passed through `DOCSBASE_WEBSITE_CONFIG`.
Its config owner validates every HTTP count/byte/timer and constructs the existing
SDK's immutable fixed-project client once, with no fallback operational values.
This does not activate v2 configuration or change index/vector/ANN invalidation.
Use explicit `docsbase sync ABS` and read-only `docsbase status --project ABS` for
updates/readiness; watcher-off filesystem freshness is not inferred.
