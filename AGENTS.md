# AGENTS.md — working guide for coding agents

One page of hard rules, verification commands, process and pointers.
Authoritative sources: `docs/constitution.md`, `docs/specs/`, the product docs
linked below. This file only indexes them — do not duplicate spec content here.

## Repo map

- `src/` — Rust (edition 2024, MSRV 1.88): CLI (`src/cli`), daemon
  (`src/daemon`), MCP frontend (`src/mcp`), index/tokenizer/chunker
  (`src/index`), store (`src/store`), OS seam (`src/platform`).
- `tests/` — functional suites per area; `tests/fixtures/search-quality/`
  holds the golden corpus (old harness retired, see search-benchmark),
  `tests/fixtures/search-bench/` the benchmark corpus + `baseline.json`.
- `docs/` — product docs and `docs/specs/<name>/{requirements,design,tasks,progress}.md`.
- `.opencode/` — commands and the project-design skill (machine-mode protocol).

## Commands (verification)

Sandboxed agents: point `CARGO_HOME`/`RUSTUP_HOME` at a writable directory
first. CI pins the MSRV toolchain explicitly (the repo file pins nightly for
local development), so validate with `RUSTUP_TOOLCHAIN=1.88.0`.

Memory: 32-core hosts run 32 parallel `rustc` by default — full builds
(`test --all-targets`, `clippy --all-targets`, release) spike to ~20 GB RSS
plus page cache (`target/` grows to ~19 GB). On constrained hosts cap it:
`CARGO_BUILD_JOBS=8`; prefer `cargo test --lib` / `--test <name>` for
iteration.

```sh
RUSTUP_TOOLCHAIN=1.88.0 cargo test --locked
RUSTUP_TOOLCHAIN=1.88.0 cargo clippy --locked --all-targets -- -D warnings
cargo fmt --check
RUSTUP_TOOLCHAIN=1.88.0 cargo test --locked --release --test perf_budget
RUSTUP_TOOLCHAIN=1.88.0 cargo test --locked --test offline            # Linux only
INSTA_UPDATE=always RUSTUP_TOOLCHAIN=1.88.0 cargo test --locked --test search_golden
RUSTUP_TOOLCHAIN=1.88.0 cargo test --locked --test search_bench
RUSTUP_TOOLCHAIN=1.88.0 cargo test --locked --features hybrid-remote --test search_bench_live   # unit only, no key
OPENROUTER_API_KEY=... RUSTUP_TOOLCHAIN=1.88.0 cargo test --locked --features hybrid-remote --test search_bench_live -- --ignored live_measurement   # manual, cost-capped
```

Timing budgets are host-relative, not host-absolute: CI runners are shared VMs
and consistently **2–2.5× slower** than a developer machine (measured
2026-10-29: the same `search_bench` gate test takes 6.5 s locally and 12.5–13 s
on `ubuntu-latest`; Rrf p95 13.16 ms in `baseline.json` vs 26.7–33.0 ms on CI).
Never "fix" a CI timing failure by concluding the code got slower, and never
tighten a latency budget from a local run. The offline latency gate therefore
applies `baseline_p95 × 1.5 × DOCSBASE_BENCH_LATENCY_SLACK + 5 ms` (CI sets the
factor to 3, locally it is 1.0) with the absolute 500 ms ceiling unscaled —
see `docs/specs/search-benchmark/requirements.md` FR-22, Delta 2026-10-29.

Search-benchmark gate policy (DELTA-3): the gate test
(`metrics_match_baseline`) runs inside the required `linux` CI job — a red
gate is a red `linux` check, which branch protection blocks from merging.
Overriding a red gate needs an explicit human override recorded in the PR
(merging stays a human act in any case). The gate answers "worse or better",
not "perfect": trap at rank 1 or missing expected = fail; everything else
is diagnostics. Live runs are advisory (weekly + manual dispatch only),
never per-PR CI.

Accept snapshots/baseline only as a deliberate, reviewed delta (see below).

## Hard rules

- No `unwrap`/`expect`/`panic!` in `src/**` (tests may use them).
- No new crates without an ADR; the default build stays offline (no network
  crates).
- `main` is protected: changes land via PR only. Required checks: `docs`,
  `linux`, `macos`, `windows`.
- Commit messages and PR titles: English prose. Literal examples in other
  scripts are fine (`ё/Ё→е/Е`, `замена/замены`). Conventional commits.
- Refactors must not change behavior. If the tokenizer/normalization pipeline
  changes, bump `TOKENIZER_VERSION` — the index rebuilds from SQLite.
- The golden baseline is a no-regression gate: improvements are allowed,
  silent regressions are not. Never regenerate it as a side effect.

## Process (machine mode)

- Spec-driven work lives in `docs/specs/<name>/` (requirements, design, tasks,
  progress ledger). Amend specs with dated delta markers instead of silently
  editing requirements.
- One task at a time, TDD where possible (RED -> GREEN). Fresh reviewer per
  task; prompt pattern: `.opencode/skills/project-design/references/reviewer.md`.
  Fix rounds for Critical/Important; minors are fixed in-task or recorded as
  `minor (deferred)` in the ledger.
- Ledger: append `Task <ID>: complete (...)` plus `Ruling:` / `minor` lines to
  `progress.md` with each task commit.
- **Merging is a human action.** Agents prepare a PR (ready for review, green
  checks, reviewed) and stop; never run `gh pr merge` or any other merge
  mechanism, even when the work looks finished. The human merges explicitly.
- Exemplar to copy: `docs/specs/search-quality/` (SQ1–SQ18).

## Gotchas (paid for in review cycles)

- Windows: drop the tantivy `IndexHandle` before `open_or_create` — its
  recreate wipes the index dir and an open lockfile blocks it.
- tantivy `PhraseQuery` same-offset terms are a conjunction: for standalone
  words the stem must *replace* the surface form, not be added next to it.
- Snowball Russian (RV region) splits illustrative trios (`замена`/`замены`
  -> one stem, `заменой` -> another); pin measured behavior, not assumptions.
- BM25 length is counted in tokens: an oversized chunk whose long blob is
  dropped stays "short", so `LONG_CHUNK_PENALTY` is the only brake (its
  soundness requires penalty <= 1; enforced by a const assert).
- The long-chunk penalty is applied while widening the raw fetch window: the
  top-k must be settled by final score, not by the initial window.
- `textnorm` offsets are relative to the normalized text (width folding and
  Arabic removals shift bytes).
- Chunker: fences/tables are atomic and may exceed cap+overlap (documented
  SC-5 exception); prose is capped and split at token boundaries.
- Boost values are pinned by `field_boosts_order_and_sensitivity` and the
  penalty end-to-end; do not tune them without golden evidence (NFR-6).
- macOS is `cfg(unix)`: keep platform branches exclusive
  (`cfg(all(unix, not(target_os = "macos")))` for the generic Unix variant)
  or the crate fails to compile on macOS only.
- macOS liveness must treat zombies as dead (`proc_pidinfo` + `SZOMB`):
  `kill(pid, 0)` alone reports a killed-but-unreaped child as alive.
- BSD/macOS `accept()` inherits `O_NONBLOCK` from the listener (Linux does
  not), and XNU rejects `setsockopt` with EINVAL on shut-down sockets: clear
  non-blocking before setting timeouts and tolerate dead connections.
- macOS releases: ad-hoc `codesign -s - --force` after build plus a
  `codesign -v` gate (Apple Silicon kills unsigned binaries); sign after any
  `lipo` merge.
