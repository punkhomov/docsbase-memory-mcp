# docsbase — local-first documentation memory for coding agents

[![CI](https://github.com/punkhomov/docsbase-memory-mcp/actions/workflows/ci.yml/badge.svg)](https://github.com/punkhomov/docsbase-memory-mcp/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/punkhomov/docsbase-memory-mcp?include_prereleases)](https://github.com/punkhomov/docsbase-memory-mcp/releases)
[![License: MIT](https://img.shields.io/badge/license-MIT-green.svg)](LICENSE)
[![MSRV 1.88](https://img.shields.io/badge/MSRV-1.88-blue.svg)](rust-toolchain.toml)

> **Alpha:** current version [`0.1.0-alpha.3`](CHANGELOG.md). Breaking changes
> are expected before `0.1.0`. Git tag `v<version>` always equals
> `version` in `Cargo.toml`.
>
> Read this in another language: [Русский](README.ru.md) · **English**

One `docsbase` binary, three roles: **CLI**, **stdio MCP server**, and a
**per-account daemon**. The daemon owns the project registry, per-project
full-text indexes, file watchers and sync jobs. The `.md` files on disk are
the only source of truth; the MCP surface is read-only over the index.
No network calls at runtime — no telemetry, no cloud, no Docker.

Supported platforms: **Linux x86_64** (static binary) and **Windows x64**
(`.exe`). macOS is out of scope for v1.

## Quickstart (5 minutes)

```sh
# 1. Install from a GitHub Release asset (see docs/INSTALL.md for options).
# Alpha period: every release is a prerelease, so opt in to that channel;
# stable is the default once 0.1.0 ships.
curl -fsSL https://raw.githubusercontent.com/punkhomov/docsbase-memory-mcp/main/install.sh | DOCSBASE_CHANNEL=prerelease sh
# Windows (PowerShell):
# $env:DOCSBASE_CHANNEL = "prerelease"; irm https://raw.githubusercontent.com/punkhomov/docsbase-memory-mcp/main/install.ps1 | iex

# 2. Register the owned binary + manifest
docsbase install

# 3. Index your docs repo (defaults to the current directory)
cd /path/to/your/docs
docsbase index

# 4. Search
docsbase search "admission lease" --limit 10

# 5. Re-index on demand
docsbase sync
docsbase status
```

Full walkthrough: [`docs/QUICKSTART.md`](docs/QUICKSTART.md).

## Install

| Method | Command | Notes |
|---|---|---|
| Linux script | `curl -fsSL …/install.sh \| sh` | Downloads the release asset, verifies sha256, runs `docsbase install` |
| Windows script | `irm …/install.ps1 \| iex` | Same for the `.exe` asset |
| Cargo | `cargo install --locked docsbase --version 0.1.0-alpha.3` | Builds from source, MSRV 1.88 |
| Manual | Download asset from [Releases](https://github.com/punkhomov/docsbase-memory-mcp/releases), `sha256sum -c`, then `docsbase install` | Verify SLSA provenance, see [`docs/INSTALL.md`](docs/INSTALL.md) |

Updates: `docsbase update` (checks the latest release, verifies it, replaces
the binary). Pin a version with `docsbase update --version v0.1.0-alpha.3`.

Every release ships: `sha256sum` files, SBOM, and SLSA build provenance
(Sigstore). Verify with `gh attestation verify`.

## CLI reference (short)

```text
docsbase install            # install/update the binary + owned manifest
docsbase update             # fetch the latest release, verify, replace the binary
docsbase index [PATH]       # register and index a project (default: cwd)
docsbase search QUERY [--limit N]
docsbase list               # indexed documents of the current project
docsbase sync [PATH]        # re-index (daemon job when reachable, else direct)
docsbase config             # effective config for the current directory
docsbase status             # registry-wide status
docsbase mcp                # run the stdio MCP server (used by agents, not by hand)
docsbase daemon stop        # stop the daemon
docsbase uninstall [--yes]  # dry-run without --yes; deletes owned artifacts with --yes
```

Details: [`docs/CONFIG.md`](docs/CONFIG.md), `docsbase --help`.

## MCP setup

`docsbase mcp` is a stdio server. Register it once per agent client:

```json
{
  "mcpServers": {
    "docsbase": { "command": "docsbase", "args": ["mcp"] }
  }
}
```

Tools: `search_docs`, `get_doc`, `read_neighbors`, `list_docs`,
`list_projects`, `index_project`, `sync_start`, `sync_status`, `status`.
Details and timeouts: [`docs/MCP.md`](docs/MCP.md).

## How it works

```text
agents (stdio JSON-RPC) -> docsbase mcp (frontend) -> local socket/pipe -> daemon
CLI -------------------------------------------------------------> daemon / snapshot
daemon -> registry.db (SQLite WAL) + projects/<id>/tantivy + per-project watcher
```

All OS-dependent code lives behind `src/platform/` (Unix socket vs Windows
named pipe). Public contracts (MCP tools, CLI, config format, socket
protocol) are versioned; breaking changes bump the major version.
More: [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md).

## Configuration

Precedence: **CLI > project > global > defaults**.

- Global: `~/.config/docsbase-memory-mcp/config.toml`
- Project: `.docsbase.toml` + `.docsbaseignore` in the project root
- Overrides: `DOCSBASE_CACHE_DIR`, `DOCSBASE_DATA_DIR`, `DOCSBASE_CONFIG_DIR`
- Defaults: `max_file_size = 1 MiB`, `max_docs_per_project = 20000`,
  `auto_index = false`, `hybrid = false`

Example and all keys: [`docs/CONFIG.md`](docs/CONFIG.md).

## Development

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test --locked
cargo test --locked --release   # incl. perf budgets (single-threaded)
```

Toolchain: `rust-toolchain.toml` pins nightly for local dev; CI validates
MSRV 1.88 explicitly. `Cargo.lock` is committed; new crates need an ADR and
must satisfy `min-publish-age = 14 days`.

Branching: GitHub Flow (`main` + PR + `v*` tags). Conventional Commits.
Releases only from tags, with provenance — see
[`docs/specs/constitution.md`](docs/specs/constitution.md).

## Security

Local-only by design: socket/pipe on localhost, `0600`/`0700` on Unix
(best-effort ACLs on Windows), file ops confined to registered roots,
manifest-validated uninstall. Supply-chain policy (threat model, quarantine,
what Dependabot does and does not cover): [`docs/supply-chain.md`](docs/supply-chain.md).
Report issues per [`SECURITY.md`](SECURITY.md).

## License

MIT — see [LICENSE](LICENSE).
