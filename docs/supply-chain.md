# Supply chain

Threat model, what each mechanism covers, and what is deliberately deferred.
Short version: Dependabot is our freshness watchman, **not** our supply-chain
defense. Trust is enforced by quarantine (time) today and by audit (`cargo-vet`)
in stage 2.

## Problem

A malicious or compromised crate version can enter the build through:

1. **Direct dependencies** — versions we declare in `Cargo.toml` / workflows.
2. **Transitive dependencies** — versions the resolver picks into `Cargo.lock`.
3. **Build actions** — third-party GitHub Actions pinned in workflows.
4. **Stale tooling** — a resolver that silently ignores our policy keys.

`Cargo.lock` records versions and checksums but **no publish dates**, so the
lock file alone proves nothing about age. Any age check must happen at version
*selection* time (resolver) or by re-checking dates out of band.

## Layers (defense in depth)

| Layer | Catches | Scope | Status |
|---|---|---|---|
| Dependabot security updates | known CVEs, immediately (bypass all delays) | direct + transitive (via lock) | active |
| `cooldown: default-days 14` (dependabot) | fresh releases: PR opens only 14+ days after publish | **direct only** — transitives are resolved by cargo with no age check (confirmed upstream, dependabot-core#14683) | active |
| `min-publish-age = 14 days` (`.cargo/config.toml`, nightly-only) | fresh releases at resolve time | direct + transitive, **but enforced only under nightly**; stable cargo prints `unused config key` and ignores it, and `--locked` builds skip resolution entirely | partial (see gaps) |
| SLSA provenance + sha256 (releases) | tampered artifacts after build | release assets | attestations published per release; installers/`docsbase update` check sha256 (integrity) and point to `gh attestation verify` for authenticity |
| `cargo-vet` audit gate | untrusted code, including transitives, regardless of age | everything in the build graph | **stage 2** |
| CI age gate over the lock (all entries < 14d → red) | fresh transitives smuggled via lock | `Cargo.lock` fully | **TODO, blocked on cargo ~1.100** |

## Solo-maintainer policy (why the gaps are acceptable for now)

- There is a single contributor. Every `Cargo.lock` mutation is produced
  locally under **nightly** (`cargo update` is never run on stable — stable
  would silently ignore the age key and poison the lock).
- CI builds with `--locked` on stable 1.88 (MSRV proof). It does not
  re-resolve, so it cannot violate the age rule by itself.
- Dependabot version-update PRs are **disabled**
  (`open-pull-requests-limit: 0`); only security updates flow automatically.
  Version bumps are done manually under nightly, where the 14-day rule bites.
- External PRs touching `Cargo.toml`/`Cargo.lock` must be reviewed by hand
  (no such contributors today — revisit when that changes).
- `dtolnay/rust-toolchain` is ignored by Dependabot: that action uses the git
  ref as the toolchain version, so its "updates" (e.g. `@1.120.0`) are bogus
  and can never work.

## Known holes (accepted, tracked)

1. **Fresh transitives via Dependabot security/lock updates.** Cooldown does
   not apply to transitives; a security PR may pull a 1-day-old transitive
   into the lock and stable CI stays green. Caught later, at the next local
   nightly `cargo update` (resolver refuses it) — late but loud.
2. **No CI enforcement of age.** Until the mechanism stabilizes (~cargo
   1.100), CI cannot check ages: stable ignores the key, `--locked` skips
   resolution. Tracked in the supply-chain issue (see TODO below).
3. **`cargo-vet` not yet adopted.** The only layer that judges *trust* rather
   than *age*. Stage 2.

## TODO (when cargo ~1.100 stabilizes `min-publish-age`)

- Add a CI `supply-chain` job that fails on any locked version younger than
  14 days (natively if stable cargo learns the key, else via `created_at`
  from the crates.io API) — must cover **transitives**.
- Adopt `cargo-vet`: `cargo vet init`, import Mozilla/Google audits, `vet`
  gate in `ci.yml`.
- Reconsider Dependabot version updates (currently off): with vet + age gate
  in CI, bot PRs become safe to consume again.

Tracking issue: [#12](https://github.com/punkhomov/docsbase-memory-mcp/issues/12).
