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
| `search_docs` | project | `{query: string, limit?: int ≥ 1 (default 10), scope?: "project", rerank?: bool}` | Lexical BM25 over chunks, or hybrid when the project enables it; returns `{chunk_id, path, heading_path, lines, score}` citations. `scope` is reserved: v1 serves the current project only. `rerank` overrides the project setting per request; `true` requires a rerank stage |
| `get_doc` | project | `{path: string}` | Full document by project-relative path |
| `read_neighbors` | project | `{chunk_id: int, before?: int (default 1), after?: int (default 1)}` | Adjacent chunks for progressive disclosure |
| `list_docs` | project | `{limit?: int (default 50), cursor?: string}` | Keyset-paginated `{title, size, chunks}` |
| `list_projects` | registry | `{}` | Projects + indexing status; works from any directory |
| `index_project` | registry | `{path?: string}` | Register + full index (defaults to session dir); long-running (600 s budget) |
| `sync_start` | registry | `{project_id?: int}` | Start (or return active) sync job → `{job_id}`; quick |
| `sync_status` | registry | `{job_id: int}` | Job `{state: queued\|running\|done\|error, stats}`; poll every ~250 ms |
| `status` | registry | `{}` | `{protocol_version, build_id, schema_version, projects, sessions, fd_count, threads}`; each project carries its state, document and chunk counts, `root_state` and `embed_coverage` |

Project tools require a session bound to a registered project; otherwise the
server returns a `Project` error with a fix-up instruction (e.g. run
`docsbase index`). `index_project` may take the full indexing budget —
clients must allow the 600 s IPC timeout; `sync_*` only enqueue/read rows.

## Scores

`score` is not one scale, and the response does not label which one it is yet:

- lexical config — raw BM25, unbounded and corpus-relative;
- `ranking = "rrf"` — the fused value (`1/(k+rank)` magnitudes);
- an active rerank stage — the reranker's own `0..1` output.

Compare scores only inside one configuration. Across a configuration change,
compare order, `path` and `lines`, never the numbers.

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
