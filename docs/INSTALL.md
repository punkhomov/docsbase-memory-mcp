# Install

Supported: **Linux x86_64** and **Windows x64**. macOS is out of scope for v1.
Alpha version: `0.1.0-alpha.2` (`v0.1.0-alpha.2` tag).

## Option A — script (recommended)

Linux:

```sh
curl -fsSL https://raw.githubusercontent.com/punkhomov/docsbase-memory-mcp/main/install.sh | sh
```

Windows (PowerShell):

```powershell
irm https://raw.githubusercontent.com/punkhomov/docsbase-memory-mcp/main/install.ps1 | iex
```

What the script does:

1. Resolves the version (`DOCSBASE_VERSION` env or latest GitHub Release).
2. Downloads the asset for your OS/arch
   (`docsbase-linux-x86_64.tar.gz` / `docsbase-windows-x86_64.zip`).
3. Verifies `sha256sum` against the published `.sha256` file. This is an
   integrity check (truncated/corrupt download); the checksum ships in the
   same release as the asset, so it is not a security boundary. For
   authenticity, releases carry SLSA provenance — verify it manually with
   `gh attestation verify` (below).
4. Installs the binary into `~/.local/bin` (`%USERPROFILE%\.local\bin` on
   Windows, added to `PATH` for the current user) and runs `docsbase install`.

Pin a version explicitly:

```sh
DOCSBASE_VERSION=v0.1.0-alpha.2 sh install.sh
```

```powershell
$env:DOCSBASE_VERSION = "v0.1.0-alpha.2"; iex (irm .../install.ps1)
```

## Option B — Cargo

```sh
cargo install --locked docsbase --version 0.1.0-alpha.1
docsbase install
```

Requires Rust 1.88+. Local dev uses the nightly pinned in
`rust-toolchain.toml`; MSRV builds use `--locked`.

## Option C — manual

1. Download the asset + `.sha256` from
   [Releases](https://github.com/punkhomov/docsbase-memory-mcp/releases).
2. Verify: `sha256sum -c docsbase-*.sha256` (Linux) or
   `Get-FileHash` against the `.sha256` content (Windows).
3. Verify provenance (recommended):
   `gh attestation verify <asset> --repo punkhomov/docsbase-memory-mcp`.
4. Put the binary on `PATH` and run `docsbase install`.

## What `docsbase install` does

- Stops the daemon, waits for the admission lease, clears stale state.
- Atomically copies the current executable to the data dir
  (`~/.local/share/docsbase-memory-mcp/bin/docsbase` on Linux,
  `%LOCALAPPDATA%\docsbase-memory-mcp\bin\docsbase.exe` on Windows).
- Writes the owned manifest `install.json` (binary, socket/pipe, cache root,
  `build_id`, protocol/schema versions).
- Prints the installed path and manifest location.

Custom locations (used by tests and advanced setups):

```sh
DOCSBASE_CACHE_DIR=/tmp/dbc-cache \
DOCSBASE_DATA_DIR=/tmp/dbc-data \
DOCSBASE_CONFIG_DIR=/tmp/dbc-config \
docsbase install
```

## Updating

```sh
docsbase update              # latest release; no-op when already current
docsbase update --check      # only report whether an update exists
docsbase update --force      # reinstall the current version
docsbase update --version v0.1.0-alpha.2   # pin a release tag
```

`docsbase update` downloads the release asset for your platform, verifies
sha256 (integrity), then replaces both the owned copy (data dir) and the
binary that invoked the command. It reuses the same daemon coordination as
`docsbase install`: the daemon is stopped, the admission lease is taken, and
the binary is swapped atomically; the daemon restarts on next use.

Requirements: `curl`, `sha256sum` and `tar` on Linux; PowerShell on Windows.
`--from <PATH>` installs a local binary instead, skipping download and
verification (used by tests and air-gapped setups).

Environment:

- `DOCSBASE_REPO=owner/name` — use a fork's releases.

## Uninstall

```sh
docsbase uninstall        # dry-run: lists owned artifacts + indexed projects
docsbase uninstall --yes  # deletes the binary, manifest and cache root
```

Only paths recorded in the owned manifest (plus the wholly owned cache root)
are ever deleted. Config files are kept.

## Troubleshooting

- `daemon did not release the admission lease`: a blocking job is winding
  down — wait and retry; `docsbase daemon stop` first.
- `manifest … is not the configured cache`: the manifest was tampered with
  or copied from another machine — reinstall from the real binary.
- Windows: if `docsbase` is not found after install, reopen the terminal
  (installer updates the user `PATH`).
