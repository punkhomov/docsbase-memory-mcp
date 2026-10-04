# Config

Precedence (highest first): **CLI > project > global > defaults**.

## Files

| Level | Path | Format |
|---|---|---|
| Global | `~/.config/docsbase-memory-mcp/config.toml` (`%APPDATA%\docsbase-memory-mcp\config.toml` on Windows) | TOML, all keys optional |
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
| `hybrid` | `false` | Hybrid retrieval feature flag (phase 2; off in v1) |

Unknown keys are rejected (`deny_unknown_fields`) — typos fail loudly.

## Example

Global `config.toml`:

```toml
max_file_size = 2097152
max_docs_per_project = 20000
auto_index = false
hybrid = false
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
