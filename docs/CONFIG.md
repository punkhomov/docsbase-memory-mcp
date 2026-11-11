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


## Versioned source planning API (CF3)

The running CLI/daemon still loads the v1 format documented above. Do not add
`config_version` or `vector_cache` to active configuration files yet: ordinary
loaders reject them. The read-only migration command below previews changes;
there is no write/apply operation in this milestone.

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
files. No files, artifacts or providers are changed. Runtime adoption, consent,
strict shared-store ownership and recovery are later #78 milestones.

## Effective values with origins

```sh
docsbase config effective --format=json
```

This versioned view resolves the current v1 global/project files and shows each
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
The CLI continues to reject the experimental v2 file format; the library can also
render an explicitly supplied CF3 preview with its v2 budget key and scope.


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
Existing v1 runtime loading is unchanged; v2 is accepted here for planning only.

The version1 JSON envelope contains `observation_id`, selected roots, registry,
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
Do not copy proposed v2 settings into active files before runtime adoption ships.

A busy/WAL/recovery-bearing registry is unavailable, not empty. The command never
stops a daemon or repairs storage. Unavailable registry/source observations block
all source rewrites. Invalid semantic settings or exceeded envelope/context limits
produce a redacted error and nonzero exit; a successfully emitted blocked plan
returns exit 0. No prompts or partial JSON are emitted on planning errors.

Sources must be regular UTF-8 files, at most 1 MiB each; final symlinks and special
files are rejected. Parent aliases are resolved and checked around reads. Official
planning limits are 256 projects, 1 MiB path/registry metadata, 16 MiB aggregate source
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
observation hashes describe exact file bytes, not semantic artifact fingerprints.
## Local Web panel

See [WEB.md](WEB.md) for `docsbase web --open`, session security, Web settings,
source-build frontend preparation and diagnostics. Official cli-full includes
Web; Cargo default stays lean.
