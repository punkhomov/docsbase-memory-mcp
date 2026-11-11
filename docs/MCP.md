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
| `search_docs` | project | `{query: string, query_mode?: "syntax"\|"literal", limit?: int ≥ 1 (default 10), scope?: "project", rerank?: bool, explain?: bool, snippet?: bool, dedupe?: "none"\|"path", n_per_doc?: int 1…10, path_prefix?: string, tags?: [string]}` | Lexical BM25 over chunks, or hybrid when the project enables it; returns `{chunk_id, path, heading_path, lines, score}` citations. `scope` is reserved: v1 serves the current project only. `rerank` overrides the project setting per request; `true` requires a rerank stage. `explain` returns `{hits, meta}` — see [Telemetry](#telemetry). `path_prefix`/`tags` define the search scope; `dedupe` caps documents — see [Scope, snippets and document caps](#scope-snippets-and-document-caps) |
| `get_doc` | project | `{path: string}` | Full document by project-relative path |
| `read_neighbors` | project | `{chunk_id: int, before?: int (default 1), after?: int (default 1)}` | Adjacent chunks for progressive disclosure |
| `list_docs` | project | `{limit?: int (default 50), cursor?: string}` | Keyset-paginated `{title, size, chunks}` |
| `list_projects` | registry | `{}` | Projects + indexing status; works from any directory |
| `index_project` | registry | `{path?: string}` | Register + full index (defaults to session dir); long-running (600 s budget) |
| `sync_start` | registry | `{project_id?: int}` | Start (or return active) sync job → `{job_id}`; quick |
| `sync_status` | registry | `{job_id: int}` | Job `{state: queued\|running\|done\|error, stats}`; poll every ~250 ms |
| `status` | registry | `{}` | `{protocol_version, build_id, schema_version, projects, sessions, fd_count, threads, query_cache}`; each project carries its state, document and chunk counts, `root_state`, `embed_coverage`, `index_generation` and `ann` (`{state, vectors, estimated_bytes, reason}`) |

Project tools require a session bound to a registered project; otherwise the
server returns a `Project` error with a fix-up instruction (e.g. run
`docsbase index`). `index_project` may take the full indexing budget —
clients must allow the 600 s IPC timeout; `sync_*` only enqueue/read rows.

## Query interpretation

`query_mode` selects `syntax` (the default, also used for omitted/null values)
or `literal`. Syntax supports the existing fields, operators and quoted phrases;
invalid syntax remains a query error. For natural-language text, use:

```json
{"query":"Crohn's disease: alpha (+)","query_mode":"literal","limit":10}
```

Literal analyzes each whitespace-separated word through the existing field
analyzers and searches their OR combination. Punctuation, quotes, field-like
prefixes and signs are text rather than query operators; the whole input is not
made a required phrase. A single word that analyzes into multiple tokens keeps
the analyzer's phrase/position behavior. Input without searchable terms is a
query error when search executes. Embedding and reranking receive the original
text unchanged. Existing ranking, filters and response shapes apply in both modes.

Literal requires the daemon's `literal-query-v1` capability. An older daemon
produces a protocol error asking for restart before the search is sent; clients
do not downgrade or retry the query in syntax/direct mode. The protocol version
remains 1 and default syntax remains compatible with older scoped-search daemons.

CLI: `docsbase search "Crohn's disease: alpha (+)" --query-mode literal`.
Rust: `IndexHandle::search_with_mode(query, limit, QueryMode::Literal)` and the
same method on `ReadIndex`; existing `search(query, limit)` stays Syntax. The
CLI `search::run_with_mode` accepts the same enum while `Options` and `run` keep
their existing contract. Supported sync/async tool helpers negotiate literal;
low-level `Client::call(Request)` / `AsyncClient::call(Request)` leave capability
negotiation to the caller.

Measured comparison, including retrieval gains and losses: [SciFact Syntax/Literal](specs/search-benchmark/reports/scifact-query-modes-2026-11-11.md).

## Scores

`score` has different scales. The default array does not label the scale;
`explain: true` reports it in `meta.score_scale`:

- lexical config — raw BM25, unbounded and corpus-relative;
- `ranking = "rrf"` — the fused value (`1/(k+rank)` magnitudes);
- an active rerank stage — the reranker's own `0..1` output.

Compare scores only inside one configuration. Across a configuration change,
compare order, `path` and `lines`, never the numbers.

## Telemetry

`search_docs {explain: true}` returns `{hits, meta}`; without the flag the response
is the bare array above, unchanged.

`meta`:

| Field | Meaning |
|---|---|
| `score_scale` | `bm25`, `rrf` or `rerank` — which scale `score` is on, derived from the path that actually ran (`rerank: false` on a project with a stage answers `rrf`) |
| `stages` | Stages that executed, in order: `bm25`, `dense`, `rrf`, `rerank` |
| `stage_ms` | Milliseconds per executed stage plus original end-to-end `total`, including startup, queueing and transport through reply receipt; a stage that did not run has no key |
| `queue_ms` | Admission delay already included in `stage_ms.total`: frontend/worker/metadata-reader waiting; do not add it to total again |
| `window` | Planned eligible candidate window: `limit` for lexical search (×4 with a document cap), `max(limit, window)` when fused or reranked, clamped to 1000. Actual candidates may be fewer |
| `index_generation` | Index generation the answer was served from |
| `degraded` | Reasons the tier did not serve in full: `ann-building`, `ann-insufficient`, `ann-error`, `exact-fallback`, `no-rerank-stage`. Empty is the normal answer |

## Scope, snippets and document caps

All four are opt-in; without them the response is the bare array above, unchanged.

| Key | Effect |
|---|---|
| `snippet: true` | Adds `snippet` (≤ 240 characters, cut on a character boundary) and `snippet_truncated` to each hit. The fragment is a prefix of the chunk's own text and costs no extra round-trip |
| `dedupe: "path"` | Keeps at most `n_per_doc` chunks per document, preserving the relative order of the survivors |
| `n_per_doc` | Chunks per document when deduping, `1…10`. Defaults to `2`: measured on the `ru_morph` benchmark slice, `1` loses 3 of 11 expected-path hits while `2` loses one and still halves the repetition. Rejected without `dedupe: "path"` |
| `path_prefix` | Literal, case-sensitive prefix of the project-relative path (`docs/` matches `docs/a.md`, not `Docs/a.md` or `docsx.md`) |
| `tags` | Frontmatter tags, AND over all of them; a document without frontmatter has no tags. Both `tags: a, b` and block lists (`tags:` followed by `- a` lines) are indexed |

For example, both `tags: api, docs` and this frontmatter match
`"tags": ["api", "docs"]`:

```yaml
---
tags:
  - api
  - docs
---
```

Block-list items accept paired quotes, blank lines and comments. This is a
limited frontmatter parser; flow lists (`tags: [api, docs]`), nested maps,
anchors, multiline scalars and YAML escapes are unsupported. After upgrading
from a build that missed block-list tags, run `docsbase sync` in the project
or call `sync_start` and wait for `sync_status` to complete. A full sync refreshes
the old metadata even if the source files have not changed.

Path and tag filters define the search scope **before** each source's top-k.
BM25 keeps corpus-wide statistics; dense search selects the best cached vectors
within the scope. RRF combines ranks within these scoped source lists, and the
reranker receives only eligible candidates, once. Empty scope returns an empty
result. No reindex or configuration change is needed after upgrading.

Filtered dense currently uses exact streaming over eligible chunks, avoiding
whole-project vector/ANN loading. Broad scopes may be slower than unfiltered
ANN. Hash pages and candidate heaps are bounded; scope preparation uses the
configured search byte allowance and can fail explicitly if it exceeds it.
This allowance measures logical retained data/preparation, not process RSS.
No scope cache is retained; sync/tag/path changes are observed on the next
request, and a generation change during preparation asks the caller to retry.

Document caps still apply after ranking over a bounded window (lexical ×4,
maximum 1000); they can produce fewer than `limit` hits. Diversity/refill is
separate work. `explain` echoes the requested scope/cap in `meta.filters`.
`meta.trimmed.filtered_out` counts only rejections observed by the final
check, normally zero because scope exclusion happened before collection;
it does not count excluded corpus documents. Missing document metadata never
counts as a filter rejection. `deduped` counts observed document-cap drops.

`query_cache` reports `{hits, misses, entries, bytes}` for the query-embedding
cache: a repeated query with the same model is answered without calling the
provider. It is bounded (256 entries / 4 MiB, least-recently-used eviction) and
never caches a failed embed. Zeroes mean the caller cannot see the daemon's cache
(CLI direct mode), not that the cache is off.

`status.projects[].ann.state` is `disabled`, `loading`, `building`, `ready`,
`too_large`, `fallback`, `stale` or `unknown`; `reason` is present for every state
except `ready`. The record is kept by the daemon, so `docsbase status` without a
running daemon reports `unknown` with `reason: "daemon not running"` rather than
claiming the tier is off.

## Timeouts and cancellation

- `search_docs`: one global `search.request_timeout_ms` budget (default 30 s),
  shared by daemon startup, connection admission, search, reply and at most one
  MCP reconnect. Configure it in [global config](CONFIG.md#search-request-policy-global-only).
- Other quick tools: the existing 30 s exchange budget; `index_project`: 600 s.
- CLI `sync` polls `sync_status` up to ~610 s.

MCP request cancellation closes that caller's active scoped search connection.
Cancelling a request still waiting for the shared connection leaves the active
call alone. Expired/cancelled searches cannot replay paid work. The frontend
uses asynchronous local IO on Linux, macOS and Windows; search does not depend
on blocking socket timeout setters.

A synchronous provider or already-running startup operation can still finish
after its caller stops waiting. Cooperative checks prevent subsequent paid
stages, and shared work continues for other live callers. Actual daemon work
and pending reply delivery retain their request/connection charges until they
settle. An expired backpressured reply can end at EOF or a partial frame; a
complete timeout error is not guaranteed on that connection.

New MCP/CLI search requires the daemon's `search-context-v1` capability. Restart
an older daemon when prompted; matching package versions alone do not prove
support. Legacy `CallTool` clients remain compatible, including write-half-close
and reply behavior, but do not gain scoped EOF cancellation or propagated caller
budgets. See [the IPC contract](ARCHITECTURE.md#scoped-search-ipc).

## Guarantees

- MCP is **read-only** over user docs: the only source of truth is the
  `.md` files; tools never write them.
- File access is confined to registered roots (symlink escape rejected).
- Errors are machine-readable (`code`, `message`, `instruction`); a missing
  project is actionable, not fatal.
