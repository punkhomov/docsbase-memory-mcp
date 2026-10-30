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

## Keys

| Key | Default | Meaning |
|---|---|---|
| `ignores` | `[]` | Extra ignore patterns, appended to built-ins (`.git`, `target`, `node_modules`, …) |
| `max_file_size` | `1048576` (1 MiB) | Files larger than this are skipped |
| `max_docs_per_project` | `20000` | Admission limit per project |
| `auto_index` | `false` | Register + index unknown projects on first connection (ADR-6: off) |
| `hybrid` | `false` | Hybrid stages (embeddings + rerank) master switch; boolean shorthand or a table, see below |

Unknown keys are rejected (`deny_unknown_fields`) — typos fail loudly.

## Hybrid stages (opt-in, spec `docs/specs/hybrid-providers`)

Hybrid search is opt-in per project. Two providers exist:

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
