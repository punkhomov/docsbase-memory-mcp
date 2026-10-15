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

Hybrid search needs a build with the `hybrid-remote` feature
(`cargo build --features hybrid-remote`) and `OPENROUTER_API_KEY` in the
environment; the default build stays fully offline and any enabled hybrid
configuration fails with an explicit admission error instead of silently
falling back to BM25.

```toml
hybrid = true          # shorthand: both stages with defaults

# or the full form; a missing stage section disables that stage
[hybrid]
enabled = true

[hybrid.embedding]
provider = "openrouter"            # iteration 1 allows only openrouter
model = "qwen/qwen3-embedding-8b"  # default; MRL-truncated
dimensions = 1024                  # part of the vector cache key
normalize = true                   # L2; part of the cache key

[hybrid.reranker]
provider = "openrouter"
model = "cohere/rerank-v3.5"
window = 50                        # BM25 candidates sent per search (10..=100)
```

Semantics: chunk vectors are cached content-addressed
(`openrouter/<model>@d<dim>@<l2|raw>`); opening a project with incomplete
coverage enqueues a background sync job (`embed_coverage` in `status`);
searches rerank the BM25 window, `rerank=false` per request keeps strict BM25
order (the MCP `search_docs` tool accepts the same boolean).

## Example

Global `config.toml`:

```toml
max_file_size = 2097152
max_docs_per_project = 20000
auto_index = false
hybrid = false   # true = hybrid stages (needs --features hybrid-remote build)
ignores = ["drafts/**", "*.tmp.md"]
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
