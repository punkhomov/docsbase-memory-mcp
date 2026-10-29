# docsbase — local-first documentation memory for coding agents

[![CI](https://github.com/punkhomov/docsbase-memory-mcp/actions/workflows/ci.yml/badge.svg)](https://github.com/punkhomov/docsbase-memory-mcp/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/punkhomov/docsbase-memory-mcp?include_prereleases)](https://github.com/punkhomov/docsbase-memory-mcp/releases)
[![License: MIT](https://img.shields.io/badge/license-MIT-green.svg)](LICENSE)
[![MSRV 1.88](https://img.shields.io/badge/MSRV-1.88-blue.svg)](rust-toolchain.toml)

> **Alpha:** current version [`0.1.0-alpha.4`](CHANGELOG.md). Breaking changes
> are expected before `0.1.0`. Git tag `v<version>` always equals
> `version` in `Cargo.toml`.
>
> Read this in another language: [Русский](README.ru.md) · **English**

The core is a plain Rust library (`docsbase_memory`); the `docsbase` binary is
its first consumer with three roles: **CLI**, **stdio MCP server**, and a
**per-account daemon**. Embedding the library into applications and pipelines
(as a cargo dependency) is an explicit long-term direction - API stability is
not promised before 1.0. The daemon owns the project registry, per-project
full-text indexes, file watchers and sync jobs. The `.md` files on disk are
the only source of truth; the MCP surface is read-only over the index.
No network calls at runtime by default — no telemetry, no cloud, no Docker.
Optional hybrid search stages — **local** (`hybrid-local`: fastembed + ONNX
Runtime, fully offline after provisioning) or **remote** (`hybrid-remote`:
OpenRouter, requires `OPENROUTER_API_KEY`) — are opt-in per project/request
with RRF fusion over the cached vectors via `ranking = "rrf"`. The default
build stays fully offline and never falls back silently (see
[`docs/CONFIG.md`](docs/CONFIG.md)). Default remote pins are free OpenRouter
routes: they may train on prompts and the roster rotates monthly. Paid pins
via `docsbase models use` (see the hybrid section in `docs/CONFIG.md`).

Supported platforms: **Linux x86_64** (static binary), **Windows x64**
(`.exe`) and **macOS 12+ on Apple Silicon**. Intel Macs are out of scope
(`x86_64-apple-darwin` is Tier 2 upstream and Intel hardware is EOL).

## Quickstart (5 minutes)

```sh
# 1. Install the binary. The script verifies the release and registers the
#    binary with docsbase for you. Alpha period: every release is a
#    prerelease, so opt in to that channel; stable is the default once
#    0.1.0 ships.
curl -fsSL https://raw.githubusercontent.com/punkhomov/docsbase-memory-mcp/main/install.sh | DOCSBASE_CHANNEL=prerelease sh

# 2. Index your docs repo (defaults to the current directory)
cd /path/to/your/docs
docsbase index

# 3. Search
docsbase search "admission lease" --limit 10

# 4. Re-index on demand
docsbase sync
docsbase status
```

Windows (PowerShell):

```powershell
$env:DOCSBASE_CHANNEL = "prerelease"; irm https://raw.githubusercontent.com/punkhomov/docsbase-memory-mcp/main/install.ps1 | iex
```

Building from source or installing manually? Run `docsbase install` once to
register the binary — the scripts above already do it for you.

Full walkthrough: [`docs/QUICKSTART.md`](docs/QUICKSTART.md).

### Optional: local embeddings (fastembed, `hybrid-local`)

Build with the feature, install the ONNX Runtime, fetch the pinned models,
then point the stages at them (all provisioning commands are offline-safe and
idempotent):

```sh
cargo build --release --features hybrid-local
# 1. Install the pinned ONNX Runtime (downloads once; `--check` reports state)
docsbase connect fastembed
# 2. Fetch a registry model (revision + sha256 pinned in code)
docsbase models fetch bge-small-en-v1.5
docsbase models fetch jina-reranker-v1-turbo-en
# 3. Pin the stages (dims inferred from the registry for fastembed)
docsbase models use embed bge-small-en-v1.5
docsbase models use rerank jina-reranker-v1-turbo-en
```

`models use` validates a project file; outside any project pass `--global`.
Everything above works fully offline after the explicit fetch steps.

## Install

Supported: **Linux x86_64**, **Windows x64** and **macOS (Apple Silicon)**.
Details, verification and manual options: [`docs/INSTALL.md`](docs/INSTALL.md).

### Linux x86_64

```sh
curl -fsSL https://raw.githubusercontent.com/punkhomov/docsbase-memory-mcp/main/install.sh | DOCSBASE_CHANNEL=prerelease sh
```

Installs to `~/.local/bin` (override with `DOCSBASE_BIN_DIR`), verifies
`sha256`, and registers the binary (`docsbase install`). Pin a version with
`DOCSBASE_VERSION=v0.1.0-alpha.4`.

### Windows x64 (PowerShell)

```powershell
$env:DOCSBASE_CHANNEL = "prerelease"; irm https://raw.githubusercontent.com/punkhomov/docsbase-memory-mcp/main/install.ps1 | iex
```

Installs to `%USERPROFILE%\.local\bin` (added to the user `PATH`), verifies
`sha256`, and registers the binary. Same `DOCSBASE_VERSION` pinning.

### macOS (Apple Silicon)

```sh
curl -fsSL https://raw.githubusercontent.com/punkhomov/docsbase-memory-mcp/main/install.sh | DOCSBASE_CHANNEL=prerelease sh
```

Installs to `~/.local/bin`, verifies `sha256` (via stock `shasum`), and
registers the binary. The binary is ad-hoc signed; `curl` does not set the
quarantine attribute, so the terminal install runs without Gatekeeper
prompts (notarization is not needed for this path).

### Build from source (any supported platform)

```sh
cargo install --locked docsbase --version 0.1.0-alpha.4
docsbase install   # register the binary with the daemon
```

Requires Rust 1.88+ (MSRV). Local development pins nightly in
`rust-toolchain.toml`; CI validates the MSRV toolchain.

### Manual

Download the asset and its `.sha256` from
[Releases](https://github.com/punkhomov/docsbase-memory-mcp/releases), verify
( `sha256sum -c` on Linux, `shasum -a 256 -c` on macOS, `Get-FileHash` on
Windows), optionally verify SLSA
provenance with `gh attestation verify`, put the binary on `PATH`, then run
`docsbase install`.

### Updates and verification

`docsbase update` checks the latest release, verifies it, and replaces the
binary (pin with `docsbase update --version v0.1.0-alpha.4`). Every release
ships `sha256sum` files, an SBOM, and SLSA build provenance (Sigstore).

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

All OS-dependent code lives behind `src/platform/` (Unix socket on
Linux/macOS vs Windows named pipe). Public contracts (MCP tools, CLI, config format, socket
protocol) are versioned; breaking changes bump the major version.
More: [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md).

## Multilingual search

Search is offline and dictionary-free: no extra crates, no network. The
tokenizer is script-aware and versioned — changing the pipeline bumps
`TOKENIZER_VERSION` and the index rebuilds from SQLite automatically.

| Script | Normalization | Analyzer | Example |
|---|---|---|---|
| Cyrillic | — | Snowball Russian (standalone words) | `замены` finds `замена` |
| Latin | fullwidth → ASCII, `İ` → `I` | Snowball English | `storing` finds `store` |
| Arabic | harakat/tatweel removed, `أإآ`→`ا`, `ة`→`ه`, `ى`→`ي` | Snowball Arabic | `مَطَار` finds `مطار` |
| CJK | — | bigrams over every character, sequential positions | `東京` matches inside `東京国際空港` |
| All | `ё`→`е` (case-preserving), width folding | — | `ёлка` finds `елка` |
| Identifiers | not stemmed (`_`, CamelCase, ALL_CAPS, digits) | exact multi-token | `assessment_plan_id`, `MAX_FRAME_BYTES` |

Measured at SQ18 (directional; frozen copy kept as
[`tests/fixtures/search-bench/legacy-baseline.json`](tests/fixtures/search-bench/legacy-baseline.json)
after the search-quality harness was retired):
all six golden classes at hit_rate/ndcg 1.0; search p95 ≈ 0.6 ms (Latin) and
≈ 5 ms (CJK) over 50k chunks; a CJK index stays ≈ 0.8× the corpus text size.

Notes: BM25 scores are engine-internal and intentionally not snapshotted
(they drift across tantivy upgrades); fenced code blocks and tables stay
atomic (they may exceed the chunk cap); phrase matching never matches inside
identifiers (`"plan id"` ≠ `assessment_plan_id`).

## Configuration

Precedence: **CLI > project > global > defaults**.

- Global: `~/.config/docsbase-memory-mcp/config.toml`
  (`~/Library/Application Support/docsbase-memory-mcp/config.toml` on macOS)
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
