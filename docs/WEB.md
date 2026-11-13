# Local Web panel

The next official `cli-full` release includes a read-only local panel.
The published alpha.5 release predates this feature; build this branch from source
until the next release is published. It needs no
Node.js, CDN, account or internet connection to load its interface.

```sh
docsbase web --open
docsbase web --port 8080
```

The default port is assigned by the OS. Only `127.0.0.1` is supported. Open the
launch URL printed in the terminal: its fragment contains a single-use bootstrap
token. The panel exchanges it for a process-specific HttpOnly/SameSite=Strict
cookie and removes the fragment. Refreshing works; to open another tab, copy the
base address after the first login. A new process requires its new launch URL.
Do not share the launch URL. No remote hosting, users or CORS integration is offered.

Projects, document/chunk counts, search, and safe effective configuration come
from the shared daemon. Opening the panel neither registers nor indexes a folder.
Run `docsbase index` in a project beforehand. The current directory selects the
initial registered project, when possible. Configuration origins are shown as
unavailable where the daemon retains only a resolved snapshot.

Search accepts the existing Syntax/Literal interpretation, limit, prefix, tags,
document deduplication and reranker override. Search mode comes from project
configuration. Snippets are plain text. Scores are engine scores, not confidence
percentages. A configured remote provider can incur cost. Searches run only on
form submission and are never automatically retried, including after connection
loss. Queries/results are not stored in browser storage or URL parameters.

Ctrl+C stops Web and its requests; the shared daemon remains under its existing
idle grace policy. Closing a browser tab does not stop Web. An occupied explicit
port is an error. If the daemon stops, the panel marks data as last known and
polls safely; restart the daemon explicitly to restore service. Incompatible
capabilities require restarting the daemon and Web from matching binaries.

## Web policy

`web.toml` in the same config directory as `config.toml` configures the Web
component. `--port` overrides its port. Restart Web to apply; no index rebuild.

```toml
port = 0
search_connections = 4
control_connections = 2
queue_limit = 16
max_body_bytes = 65536
control_timeout_ms = 5000
startup_timeout_ms = 15000
shutdown_timeout_ms = 3000
poll_interval_ms = 5000
```

Pool capacities are positive and their sum is at most 16; queue limit is at most
1024. Body size cannot exceed the 8 MiB IPC envelope. Timeouts/intervals are
positive; the browser polling interval is at most 2,147,483,647 milliseconds
(the browser timer representation). Search deadlines and payload budgets come from the daemon's search
settings, and cover admission plus IPC. Control capacity is separate from search.
Cancelled exchanges are closed, never returned to the pool; daemon accounting
continues until actual provider work ends. Hidden tabs pause status polling.

## Building and testing

From Git, prepare frontend assets explicitly before compiling `web`/`cli-full`:

```sh
corepack enable
corepack install
pnpm install --frozen-lockfile
pnpm --dir web format:check
pnpm --dir web check
pnpm --dir web test:unit
pnpm --dir web build
pnpm --dir packages/client build
cargo build --locked --features cli-full
cargo test --locked --features cli-full
pnpm --dir web exec playwright install chromium
pnpm --dir web test
```

Rust 1.88 and the exact Node.js version in `.node-version` are build tools;
installed users need neither. The root `package.json` pins pnpm, including its
package checksum. Run all installation commands from the repository root. Cargo's
build script only embeds already-built `web/dist` files and fails with an
instruction if absent. Default Cargo features remain lean and do not need UI
assets. Crates.io publication is not currently supported; release binaries are
the distribution channel. For `cargo install --path . --features cli-full`, run
the frontend preparation first.

The full `web`/`cli-full` Rust test suite also runs the native Node IPC client
acceptance test. Build `packages/client` explicitly before that suite (or a
selected `--test node_client` run with either feature): workspace installation
does not generate its ignored `dist` exports. CLI builds and selected Rust test
targets that do not run the Node acceptance test need only the Web assets.

The pnpm workspace rejects packages published less than 14 days ago, including
transitive dependencies and entries in a frozen lockfile. Missing publication
dates and provenance downgrades fail installation. Registry integrity, exact
package-manager/Node versions, and peer dependencies are checked. New dependency
install scripts fail until explicitly reviewed; current dependency scripts are
denied. pnpm does not automatically install agent skills. These checks are
configured in the root `pnpm-workspace.yaml`; do not bypass them with local flags
or regenerate the lockfile with npm.

Use `pnpm --dir web format` to format frontend source and
`pnpm --dir web format:check` to check it without rewriting files. Native CI runs
the same format check before typechecking and building. Generated Rust contract
declarations are checked by their contract test rather than rewritten by the
frontend formatter.

Rust DTOs generate `web/src/shared/api/generated.ts`. After changing contracts:
`DOCSBASE_GENERATE_TYPES=1 cargo test --features web --test web_contract`.
The same test checks committed declarations in CI. Browser JSON is also checked
at runtime. Playwright uses `target/debug/docsbase` by default; override with
`DOCSBASE_WEB_BINARY`. For an installed Chromium, set
`PLAYWRIGHT_CHROMIUM_EXECUTABLE`.

Diagnostics log request ID, status and duration, without query/document contents
or session credentials. HTTP errors have stable code/message/request_id fields.
Browser failures retain traces, screenshots and console/Web logs in CI. Binary
smoke is `.github/scripts/check-web-artifact.py`; it works in an empty directory
without frontend sources. Bundle size is reported by Vite and Cargo; executable
and compressed distribution sizes are measured without a fixed size ceiling.
