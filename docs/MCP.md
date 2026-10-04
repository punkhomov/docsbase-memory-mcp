# MCP

`docsbase mcp` runs a **stdio MCP server** for one agent. It ensures the
daemon is up, binds the session to the calling directory, and proxies tool
calls over the local transport (Unix socket / Windows named pipe).

Client registration:

```json
{
  "mcpServers": {
    "docsbase": { "command": "docsbase", "args": ["mcp"] }
  }
}
```

Set the client's working directory to the project root so project-scoped
tools bind correctly.

## Tools

| Tool | Scope | Input | Notes |
|---|---|---|---|
| `search_docs` | project | `{query: string, limit?: int ≥ 1 (default 10)}` | FTS over chunks; returns `{path, heading, lines, score}` citations |
| `get_doc` | project | `{path: string}` | Full document by project-relative path |
| `read_neighbors` | project | `{chunk_id: int, before?: int (default 1), after?: int (default 1)}` | Adjacent chunks for progressive disclosure |
| `list_docs` | project | `{limit?: int (default 50), cursor?: string}` | Keyset-paginated `{title, size, chunks}` |
| `list_projects` | registry | `{}` | Projects + indexing status; works from any directory |
| `index_project` | registry | `{path?: string}` | Register + full index (defaults to session dir); long-running (600 s budget) |
| `sync_start` | registry | `{project_id?: int}` | Start (or return active) sync job → `{job_id}`; quick |
| `sync_status` | registry | `{job_id: int}` | Job `{state: queued\|running\|done\|error, stats}`; poll every ~250 ms |

Project tools require a session bound to a registered project; otherwise the
server returns a `Project` error with a fix-up instruction (e.g. run
`docsbase index`). `index_project` may take the full indexing budget —
clients must allow the 600 s IPC timeout; `sync_*` only enqueue/read rows.

## Timeouts

- Fast tools: 30 s IPC read timeout.
- `index_project`: 600 s.
- CLI `sync` polls `sync_status` up to ~610 s.

## Guarantees

- MCP is **read-only** over user docs: the only source of truth is the
  `.md` files; tools never write them.
- File access is confined to registered roots (symlink escape rejected).
- Errors are machine-readable (`code`, `message`, `instruction`); a missing
  project is actionable, not fatal.
