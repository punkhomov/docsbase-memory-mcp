# @docsbase/client

Official Node.js/TypeScript connector to the **local docsbase daemon**. Node
24.22.3 or newer, ESM. No runtime npm dependencies; the package contains compiled
JavaScript and TypeScript declarations. This first client stage does not yet
provide a complete website server profile or a corpus-publication readiness fence.

```ts
import { DocsbaseClient, DocsbaseError } from '@docsbase/client';

// Read these required values from your application's validated configuration.
const client = new DocsbaseClient({
  endpoint: settings.endpoint,
  projectId: settings.projectId,
  expectedBuildId: settings.expectedBuildId,
  expectedSchemaVersion: settings.expectedSchemaVersion,
  settings: {
    searchConnections: settings.searchConnections,
    controlConnections: settings.controlConnections,
    queueLimit: settings.queueLimit,
    requestTimeoutMs: settings.requestTimeoutMs,
  },
});
try {
  const hits = await client.search('authentication', {
    query_mode: 'literal', snippet: true, dedupe: 'path', n_per_doc: 1, limit: 10,
  }, { signal: requestAbortSignal });
  const status = await client.status();
} finally {
  client.close();
}
```

Construction validates immutable configuration and performs no I/O. Each new
native connection negotiates protocol/build/schema/capabilities, then binds the
already registered project read-only. `expectedBuildId` currently means the exact
`docsbase <Cargo package version>` string, **not** a source commit hash. Schema is
checked independently. Select both from your deployed build's documented status.

The endpoint is an absolute Unix socket path on Linux/macOS or canonical local
`\\.\pipe\...` name on Windows. The operator supplies it (the daemon state records
its native endpoint); this library does not guess cache roots or Windows hashes.
Supported processes share the same OS user and PID namespace. Remote TCP, remote
named pipes and cross-container PID namespaces are unsupported. The local socket
is privileged: the HTTP application must enforce authentication, validation and
rate limits, and must never pass arbitrary tools or filesystem roots from users.

All four policy fields are mandatory; there are no transport-owned defaults.
Pools are separate for search/control, lazy, bounded and FIFO. `queueLimit` is the
maximum pending count **per pool**, zero disables waiting. Active limits include
connection and handshake work. Coordinate the sum of all clients with daemon
connection/session limits; a client pool is not a global daemon resource budget.
Each queued request is bounded by the 8MiB protocol frame limit; configure queue
counts with the corresponding retained input memory in mind.

One original monotonic deadline covers preparation, queue, connect, negotiation,
execution and response validation. `AbortSignal`, timeout or `close()` destroys an
interrupted connection. Actual backend work may outlive transport cancellation
until a cooperative checkpoint; this does not promise cancellation of provider
billing. The library itself never invokes providers, indexes, autostarts docsbase,
stops the daemon or falls back to a CLI/direct mode. Search still follows the
project's daemon configuration: `rerank:false` alone does not disable embeddings.
Use an explicitly lexical project when no provider requests are permitted.

A failed operation is **never replayed automatically**, even for search. A later
independent call may open and negotiate a fresh connection. Retrying belongs to
the embedding application's explicit policy and original request budget.
`close()` is synchronous/idempotent and rejects active, queued and future calls.

Search options use existing `search_docs` names and semantics. Omitted options
stay omitted; the daemon resolves their defaults. `limit` counts chunk hits;
`dedupe:'path', n_per_doc:1` requests at most one chunk per path. Filters are not
ACLs. `chunk_id` is a canonical decimal **string** preserving Rust u64 exactly.
Other unsafe JSON integers are rejected rather than silently rounded. Citations
retain paths/headings/line ranges; the website owns URL mapping and safe rendering.
No page totals, stable chunk identity across reindexing or pagination is promised.
With `explain:true`, results are `{hits,meta}`; otherwise they are a citation array.
`meta` is an additive diagnostic object, not a stable fully typed telemetry schema.

`status()` returns only the configured project, `restart_required` and `notice`.
It exposes current generation diagnostics; it does not prove sync completeness or
continuous availability during index mutation. `metadata` is the last successful
Hello for diagnostics, not fresh liveness or capability admission for a new socket.

`DocsbaseError.kind`: `validation`, `protocol`, `transport`, `daemon`, `deadline`,
`cancelled`, `overloaded`, `closed`. Daemon errors additionally preserve the numeric
`code` and optional `instruction`. Malformed peer frames are not echoed in errors.
The client adds no query/document/credential logs.

Development from repository root:

```sh
pnpm install --frozen-lockfile
pnpm --dir packages/client build
pnpm --dir packages/client check
pnpm --dir packages/client test
```

Distribution is prepared locally/CI; this change does not publish an npm package.
Server lifecycle/watcher policy, explicit sync/readiness, HTTP example and real
~20k corpus acceptance remain tracked in #79.
