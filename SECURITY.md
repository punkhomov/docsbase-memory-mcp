# Security Policy

## Supported versions

| Version | Support |
|---|---|
| `0.1.0-alpha.5` (prerelease) | Best-effort fixes; breaking changes expected before `0.1.0` |
| `main` branch | Security fixes land here first |

This is a local-first tool by default: no network calls at runtime, no
accounts, no telemetry. The attack surface that matters is local: daemon
socket/pipe, file containment, install manifest handling.

Optional hybrid search stages (per-project/per-request opt-in):

- `hybrid-remote` (OpenRouter) sends document chunks/queries to the
  configured embedding provider and requires API-key handling. The key is
  read from the environment only, never stored or logged.
- `hybrid-local` (fastembed) **downloads external artifacts** during
  provisioning only: the ONNX Runtime shared library is fetched from the
  pinned GitHub release with a code-pinned `sha256` and size, and registry
  models are fetched from pinned Hugging Face revisions with per-file
  `sha256` verification. After provisioning, searches run fully offline;
  the runtime is an external executable artifact (native code), so treat
  the install directory with the same care as any downloaded binary —
  verify the release checksums (we do, and we fail closed on mismatch).

Both stages are covered by a dedicated security review before shipping.

## Reporting a vulnerability

Prefer **GitHub Private vulnerability reporting**
(Security tab → Report a vulnerability) so details stay private until a fix
ships. Include: version/tag, OS/arch, `docsbase status` output, repro steps,
and impact assessment.

We aim to acknowledge within 72 hours and to ship a fix or mitigation plan
within 14 days for confirmed High/Critical issues.

## Scope notes

- Out of scope for v1: macOS x86_64 (Intel) and universal binaries, ARM64
  Linux and ARM64 Windows, distro packages, SDDL-ACL hardening on Windows
  (tracked as backlog after ADR-10), DoS via pathological local inputs beyond
  documented limits.
- Supply chain: releases carry sha256 + SBOM + SLSA provenance — verify with
  `gh attestation verify` before installing from an asset. Full policy
  (threat model, quarantine layers, Dependabot scope, deferred items):
  [`docs/supply-chain.md`](docs/supply-chain.md).
