# ADR-RUST100 — stable resolver and independent lock audit

2026-11-13. Replace nightly resolution with exact stable 1.100.0, preserving
the global 14-day deny settings. Cargo.toml remains the authoritative manifest;
only its MSRV changes. Delete/regenerate Cargo.lock to avoid grandfathering
old lock entries during this refresh. Required docs CI audits submitted locks
without rewriting them, using sparse crates.io publication dates and checksums.
Fail closed on missing dates, unknown versions, unsupported external sources
and network/parse errors. Local/workspace packages have no registry pubtime.

Regeneration updates 89 registry version entries in Rig and 90 in Web. Three
new transitive crate names are approved by this ADR: objc2-core-foundation and
objc2-core-services through notify 8.2.1 (macOS events; replacing fsevent-sys),
and utf_types through utf8_iter 1.1.0. No direct dependency is added. All pass
the unchanged quarantine. Native CI must check the macOS backend as well as
Linux/Windows. New Clippy changes are mechanical and must preserve behavior.

Both feature PRs remain distinct. Rebase each onto current main; inspect their
combined merge separately and regenerate the combined lock under the same
policy, rather than choose one branch lock and lose the other feature.
