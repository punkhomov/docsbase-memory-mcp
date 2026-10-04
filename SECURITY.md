# Security Policy

## Supported versions

| Version | Support |
|---|---|
| `0.1.0-alpha.1` (prerelease) | Best-effort fixes; breaking changes expected before `0.1.0` |
| `main` branch | Security fixes land here first |

This is a local-first tool: no network calls at runtime, no accounts, no
telemetry. The attack surface that matters is local: daemon socket/pipe,
file containment, install manifest handling.

## Reporting a vulnerability

Prefer **GitHub Private vulnerability reporting**
(Security tab → Report a vulnerability) so details stay private until a fix
ships. Include: version/tag, OS/arch, `docsbase status` output, repro steps,
and impact assessment.

We aim to acknowledge within 72 hours and to ship a fix or mitigation plan
within 14 days for confirmed High/Critical issues.

## Scope notes

- Out of scope for v1: macOS, ARM64, distro packages, SDDL-ACL hardening on
  Windows (tracked as backlog after ADR-10), DoS via pathological local
  inputs beyond documented limits.
- Supply chain: releases carry sha256 + SBOM + SLSA provenance — verify with
  `gh attestation verify` before installing from an asset. Full policy
  (threat model, quarantine layers, Dependabot scope, deferred items):
  [`docs/supply-chain.md`](docs/supply-chain.md).
