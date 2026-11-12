# Supply chain

## Rust policy — 2026-11-13

Rust/Cargo, rustfmt, Clippy, MSRV and CI are pinned to **1.100.0**.
Nightly is no longer needed for dependency resolution. Registry dependencies
retain a **14-day** quarantine, with `resolver.incompatible-publish-age = "deny"`.

Cargo 1.100 stabilizes the existing global/per-registry configuration keys.
Its important limits are unchanged: versions already in Cargo.lock are exempt,
records without pubtime cannot establish age, and unsupported registries may
ignore the setting. `--locked` is not an age audit. Git/path dependencies are
outside the registry publication-age mechanism. Environment/config overrides
can also weaken resolver settings.

To regenerate: preserve the old lock for review, remove Cargo.lock, then run
`cargo +1.100.0 generate-lockfile`. Review version/source changes and run
`.github/scripts/check-cargo-age.py`; CI independently audits every registry
entry in the submitted lock, including transitives, rejects missing dates and
unsupported external sources. It does not regenerate the lock in CI.

Cooldown reduces exposure to fresh releases; it does not establish code trust
or replace advisory monitoring. Dependabot version PRs remain disabled;
security changes still require review and pass the same age gate unless a
specific exception is explicitly approved. No Cargo exception is added here.
`cargo-vet` remains future work in #12. Historical exceptions remain recorded
in the corresponding spec ledgers.

Source: the Cargo 1.100 bundled reference, `registry.global-min-publish-age`
and `resolver.incompatible-publish-age` (stable since 1.100).

## Frontend policy (2026-11-11)

The frontend uses the exact pnpm version and SHA-512 in root `package.json`.
Node is fixed by `.node-version` and exact engine declarations. Node 24.22.3
is an explicitly approved security-update exception to the 14-day rule;
JavaScript packages have no age or trust exceptions.

Root `pnpm-workspace.yaml` enforces a 20,160-minute minimum release age, fails
on missing publication dates or an unsatisfied age constraint, rechecks frozen
lockfiles, rejects trust downgrades and exotic transitive sources, validates
store integrity and peers, and rejects unexpected Node/pnpm versions.
New dependency install scripts fail until reviewed; esbuild and Tailwind's
native helper scripts are explicitly denied, using their registry binaries.
Automatic dependency-provided agent skills are disabled.

Use `corepack enable`, `corepack install`, then `pnpm install --frozen-lockfile`
from the repository root. Updating dependencies must retain these policies.
Corepack verifies the package-manager hash; Node distribution checksums are
verified during local setup. CI reads the same exact Node pin.

`pnpm audit` reports all advisories; `pnpm run audit` fails on moderate or higher
severity. As of 2026-11-11 there is one known LOW development-only advisory:
[deepmerge prototype poisoning](https://github.com/advisories/GHSA-gcqx-w48m-w2h4),
through the Svelte Vite plugin. No patched version is published. It merges
trusted build configuration and is not part of the embedded runtime; it is
not hidden with an audit exclusion. Recheck when updating the plugin.

Biome is pinned and used for frontend formatting, including its explicitly
enabled experimental Svelte 5 support. Changes must pass formatting,
`svelte-check`, build and real browser tests; generated Rust contracts are
excluded from formatting. No formatter runs automatically during Cargo builds.
