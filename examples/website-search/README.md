# Website search over native IPC

A runnable Node24.22.3 example using the official read-only SDK and a fixed lexical
Website daemon. The HTTP API is `POST /search` and `GET /ready`. Authentication,
deployment and mapping Markdown paths to public URLs belong to your website.

From the repository root, prepare the pinned workspace:

```sh
corepack enable
corepack install
pnpm install --frozen-lockfile
pnpm --dir packages/client build
pnpm --dir examples/website-search check
pnpm --dir examples/website-search build
pnpm --dir examples/website-search test
```

Provision the corpus explicitly, with `hybrid=false` and no embedding/reranker
configuration. Run these commands with your existing cache/config selection:

```sh
docsbase index /absolute/corpus
docsbase status                    # record the existing project ID and schema_version
docsbase --version                 # expectedBuildId must match this exact string
docsbase serve --project /absolute/corpus
```

On Windows, use an absolute root such as `C:\docs\corpus`. Keep the foreground
daemon running in a separate terminal or your service manager. It disables
watchers and idle shutdown and refuses other projects or hybrid policy.

Copy the native endpoint from your cache's `state/daemon.json`, without guessing
its cache hash. Unix `socket` is the absolute socket path. Windows `socket` is
the published pipe name: prefix it with `\\.\pipe\` for the SDK (JSON escapes
backslashes, for example `"\\\\.\\pipe\\published-name"`). Select the actual
registered numeric project ID, deployed build string and database schema version.

Save a complete operator JSON file. These are explicit example values, not
fallbacks, measured capacity or latency recommendations:

```json
{
  "http": {
    "host": "127.0.0.1",
    "port": 3000,
    "maxConnections": 32,
    "searchActiveRequests": 4,
    "searchPendingRequests": 8,
    "controlActiveRequests": 1,
    "controlPendingRequests": 2,
    "maxHeaderBytes": 8192,
    "maxHeadersCount": 32,
    "maxBodyBytes": 16384,
    "maxQueryBytes": 4096,
    "maxResultBytes": 1048576,
    "maxHits": 10,
    "requestTimeoutMs": 30000,
    "headersTimeoutMs": 10000,
    "headerCheckIntervalMs": 1000,
    "idleTimeoutMs": 30000,
    "keepAliveTimeoutMs": 5000,
    "maxRequestsPerSocket": 100
  },
  "client": {
    "endpoint": "/absolute/published/socket",
    "projectId": 1,
    "expectedBuildId": "docsbase 0.1.0-alpha.5",
    "expectedSchemaVersion": 1,
    "settings": {
      "searchConnections": 4,
      "controlConnections": 1,
      "queueLimit": 8,
      "requestTimeoutMs": 30000
    }
  }
}
```

Replace all deployment identities, including the schema version. No policy field
is optional. Counts/bytes/milliseconds are validated once by `config.ts`; the
SDK owns validation of its complete client policy. Hosts are explicit IP addresses;
port0 may be used for an OS-selected port. Pending0 means no queue when active is
full. HTTP search/control lanes and SDK pools are separate; connection, session
and daemon worker limits remain independent, so a filled connection budget cannot
guarantee control access. Response/connection ownership lasts through actual
response completion; cancellation does not return a running SDK call's slot early.

Load the JSON into the environment and run the built example:

```sh
export DOCSBASE_WEBSITE_CONFIG="$(cat /absolute/website.json)"
node examples/website-search/dist/server.js
```

PowerShell:

```powershell
$env:DOCSBASE_WEBSITE_CONFIG = Get-Content -Raw C:\config\website.json
node examples/website-search/dist/server.js
```

The process prints only its listening address at startup. The example and SDK
have no external runtime npm dependencies; the example reuses the built workspace
SDK and pinned development tools. Use the repository workspace install, including
its development dependencies, when running this example.

```sh
curl http://127.0.0.1:3000/ready
curl -H 'Content-Type: application/json' --data '{"query":"installation","options":{"snippet":true,"dedupe":"path","n_per_doc":1}}' http://127.0.0.1:3000/search
```

`query` is a nonempty string. Optional `options` accepts `query_mode`, `limit`,
`snippet`, `explain`, `dedupe`, `n_per_doc`, `path_prefix` and `tags`. `maxHits` is
the HTTP default and cap for limit; the daemon's existing search/ranking contract
is unchanged. Requests cannot select projects, roots, tools or providers. Filters
are search scopes, not access controls. Snippets are plain text, not trusted HTML.

The application captures `performance.now()` before admission/body work and passes
`CallOptions.budget {startedAtMs,deadlineAtMs}` into the SDK. That deadline includes
queue/body/JSON/SDK preparation and native work. The SDK can only narrow it with
its own timeout measured from the same original start. Parser header timeouts
also bound time before the application receives headers. A successfully consumed
request body's `close` event does not cancel its search; aborted uploads and
unfinished response disconnects do. SIGINT/SIGTERM closes admission/connections,
cancels queued/active operations and closes the SDK without stopping the daemon.

Header byte/count, body/query/result, connection, per-socket request and separate
active/pending caps apply before retaining unbounded work. Header accounting keeps
one extra pair solely to detect/reject overflow; the Node keep-alive timeout buffer
is0 so no independent grace is added. Body high-water marks use the body byte
policy. These are implementation invariants, not hidden operator defaults.

Public failures contain a bounded stable error name, not daemon messages,
instructions, queries or document content. Invalid input is400, unknown routes404,
wrong methods405, content type415, excess body413 and headers431. Admission,
transport/offline and current-index refusal are503; deadlines504; incompatible
upstream data or excess results502. The example logs no query/document/error body.
There is no automatic request replay: a broken exchange fails; a later request
may reconnect to a restarted daemon.

Update files with the explicit operator route:

```sh
docsbase sync /absolute/corpus
docsbase status --project /absolute/corpus
```

`/ready` uses S2's `serverStatus()` and returns503 until a healthy current full
zero-error survey and stable association exist. Skipped unchanged files count as
successful survey evidence. A failed/partial latest Sync remains blocking after a
later direct index; a later successful full Sync, including a no-op, can clear it.
Missing/legacy/corrupt proof needs explicit Sync. No request repairs/registers,
autostarts, constructs providers or silently syncs. The HTTP readiness projection
contains only ready/reason/generation/restart diagnostics; full operator evidence
is available through CLI/SDK.

Ready describes the indexed corpus and recipe. With watchers off it does not
detect un-synced filesystem edits. During in-place mutation, search can be refused;
there is no pinned or continuously available old generation. The user deferred
real ~20k corpus/load/machine measurements: no production capacity, p95/p99, RSS,
throughput or update-duration claim follows from these functional tests.
