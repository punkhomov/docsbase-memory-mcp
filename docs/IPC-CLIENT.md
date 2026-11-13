# Node.js / TypeScript IPC connector

The `@docsbase/client` workspace package is the official local Node connector.
It ships compiled ESM JavaScript and declarations, has no runtime npm dependencies,
and requires Node >=24.22.3. Package API and usage: [client README](../packages/client/README.md).
This first delivery is the connector stage of [#79](https://github.com/punkhomov/docsbase-memory-mcp/issues/79).
The dedicated fixed-project server profile, disabling watchers, service-manager
lifecycle and explicit publication readiness follow separately. It does not yet
constitute full website-mode acceptance.

## Obtain the package

From a checkout, use the exact Node and pnpm versions pinned at the root:

```sh
corepack enable
corepack install
pnpm install --frozen-lockfile
pnpm --dir packages/client build
pnpm --dir packages/client pack --pack-destination /your/output/directory
```

Install the resulting `docsbase-client-*.tgz` in the website's Node application
using its normal package manager. This PR does not publish to npm. Authentication,
HTTP rate limits, request validation and Markdown-path-to-public-URL mapping stay
in that application. There is no HTTP server, N-API or embedded Rust runtime in
the connector. Embedded Rust facade #80 is not a dependency.

## Deployment / compatibility

The daemon must already be running against the intended cache. Index/register the
project explicitly with the CLI before binding. The operator selects its numeric
registry project ID and native endpoint; no browser-controlled roots/tools.
`cache/state/daemon.json` records the endpoint: on Unix its `socket` is the absolute
socket path; on Windows its `socket` is the pipe **name**, which the operator must
prefix with `\\.\pipe\` for Node's native endpoint argument. The client does not
hash arbitrary cache paths to discover a pipe or reinterpret it as TCP.

Supported placement is same host, same OS user, same PID namespace. Session
janitor checks the registered Node PID. Separate container PID namespaces and
remote backend access are not supported by this contract. Socket permissions
remain the daemon's existing authority boundary; path/tag filters are not ACLs.

Configure required `expectedBuildId` from the deployed build (`docsbase <Cargo
version>`) and `expectedSchemaVersion` from its status. Build equality is version
string equality, not source revision identity. Every new connection performs
Hello, validates protocol/build/schema and additive capabilities, then uses
`BindReadOnly`. It does not use `RegisterSession`, whose legacy path can load
providers, schedule backfill or index. Unsupported literal mode fails before
Search; it never silently becomes Syntax.

A foreground Website daemon advertises `active-website-profile-v1` in Hello.
This marks its **active profile**, not general implementation support. Shared
omits it; the Hello shape and the client's required capabilities are unchanged.
Website accepts `BindReadOnly` only for its existing fixed project, rejects
mutating registration with Admission (`-32010`), and keeps watchers disabled.
CLI sync uses that binding and the existing explicit sync job; an unavailable
globally selected Website requires foreground `docsbase serve`, without direct
sync or autostart. A connected denial or interruption is never replayed directly.

## Lifetime, concurrency and errors

See the package README for complete typed options. The application configuration
boundary must supply all pool/queue/deadline policy; the library has no hidden
operational defaults. Pools are lazy and separate for search/control, with one
exchange per connection. Queue count is per pool and zero means no waiting.
Choose capacities jointly with all other clients and daemon limits.

One operation starts before argument preparation and carries its original budget
through queue/connect/Hello/bind/encoding/execution/decoding. The same caller-owned
checkpoint runs immediately before every write: delayed JS timer callbacks cannot
authorize late work. Cancel, timeout or close destroys interrupted exchanges;
actual backend CPU work remains accounted for by existing SearchOperation/scoped
cancellation until it physically finishes. Remote billing cannot be revoked by
closing a socket. The connector performs no provider HTTP calls itself.

An already started operation is never replayed automatically. Transport failure
is returned even if the daemon might have completed its work. A later independent
call may reconnect and negotiate again; any application retry must be explicit
and remain within its original HTTP request budget.

`status()` returns only the bound project plus restart notice. `index_generation`
is diagnostic; it is not a publication token proving a complete sync. During index
mutation the backend can reject work rather than expose mixed generations. This
client does not promise serving an old generation uninterrupted. A failed or
partial sync must not be interpreted as a successful website deployment; the
explicit sync/readiness contract is the next server milestone.

Search follows project configuration. To guarantee no inference costs, provision
an explicitly lexical project; `rerank:false` alone does not disable embedding.
Search `limit` counts chunks; `dedupe:'path', n_per_doc:1` selects at most one chunk
per document. No page totals, stable chunk IDs across indexing or pagination are
promised. Chunk IDs are decimal strings, preserving u64 exactly. The website must
render snippets safely; the connector does not produce trusted HTML.

## Verification and supported claims

All three required native jobs build/typecheck/test the package, inspect a local
npm tarball and run Rust's `tests/node_client.rs` against the actual built daemon.
The fixture gate is also present in lean Rust builds without requiring Node.
It generates JSON from actual Rust protocol/citation/error types and constants;
map key order is canonicalized because cli-full enables serde_json/preserve_order.
No numeric IDs are coerced while canonicalizing.

Before a full Rust suite with `web`/`cli-full`, or a selected `--test node_client`
run with either feature, run `pnpm --dir packages/client build` after the root
workspace install. The native script imports the public package export from
ignored `dist/index.js`; dependency installation alone does not create it.
Required native CI jobs and the nightly release suite prepare it explicitly.
See [Web build/test preparation](WEB.md#building-and-testing) for the full sequence.

Regenerate a deliberately changed wire fixture:

```sh
DOCSBASE_GENERATE_IPC_FIXTURE=1 cargo test --test node_client rust_wire_fixture_matches_node_contract
pnpm --dir packages/client test
```

This does not authorize a protocol change by itself; update SDK compatibility and
its tests with the Rust contract. Normal CI compares the checked-in fixture.

Native E2E uses a small hermetic lexical corpus and checks snippets, scoped
filtering/page cap, project isolation, bounded admission, errors, no autostart,
stop/restart/reconnect and no model/vector artifacts. Fake-peer tests independently
cover cancellation, malformed frames and deterministic clock boundaries. No live
API keys/model downloads or performance thresholds are used.

The user deferred the real ~20k Markdown corpus/load/machine run on 2026-11-13.
It is **not verified**. Synthetic coverage is not a substitute and establishes no
production p95/p99, RSS, throughput or update-duration promise.
