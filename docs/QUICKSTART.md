# Quickstart

End-to-end in five minutes: install → index → search → sync.

## 0. Prerequisites

- Linux x86_64 or Windows x64, `docsbase` on `PATH` (see `INSTALL.md`).
- A directory with Markdown docs (any repo works).

## 1. Install

```sh
docsbase install
docsbase --help
```

Expected: `installed <data-dir>/bin/docsbase (<build-id>)`.

## 2. Index a project

```sh
cd /path/to/your/docs
docsbase index
docsbase list
```

`index` registers the project (defaults to the current directory) and runs
a full indexing pass. `list` shows indexed documents with title, size and
chunk count.

Index a different directory without `cd`:

```sh
docsbase index /path/to/other/project
```

## 3. Search

```sh
docsbase search "admission lease" --limit 5
docsbase search "watcher debounce"
```

Output is a JSON array of `{path, heading_path, lines, score}` citations.
When the daemon is reachable the query goes through it; otherwise the CLI
falls back to a read-only snapshot — same results.

Show registry-wide state from any directory:

```sh
docsbase status
docsbase config
```

## 4. Iterate: edit → sync → search

1. Edit any `.md` file in the project.
2. `docsbase sync` — via the daemon when it is running (job + poll), else a
   direct full index.
3. `docsbase search "<a phrase from your edit>"`.

The watcher picks up file changes automatically while the daemon runs;
`sync` is the explicit full pass.

## 5. Connect an agent (MCP)

```json
{
  "mcpServers": {
    "docsbase": { "command": "docsbase", "args": ["mcp"] }
  }
}
```

Then ask the agent to call `search_docs` / `get_doc`. Details in `MCP.md`.

## 6. Stop / clean up

```sh
docsbase daemon stop
docsbase uninstall        # dry-run first
docsbase uninstall --yes  # actually delete
```

Next: `CONFIG.md` (tuning), `MCP.md` (tool catalogue), `ARCHITECTURE.md`
(how it works).
