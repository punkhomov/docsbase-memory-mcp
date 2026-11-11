# #68 bounded dedupe refill implementation plan — 2026-11-11

> **For agentic workers:** REQUIRED SUB-SKILL: superpowers:executing-plans.
> Inline execution with fresh review per task, per AGENTS.md.

**Goal:** Добрать chunks разных документов без потери best chunks после rerank.
**Architecture:** Request-local geometric widening/capacity, one frozen bounded
dense source, base pool preservation, one captured payload and one rerank.
**Tech Stack:** Rust2024/MSRV1.88, SQLite/Tantivy; no new crates.
**Spec:** requirements.md Delta #68 FR-21..25 / SC-13..14, design.md Delta #68.

## Global Constraints

- No `unwrap`/`expect`/`panic!` in `src/**` (tests may use them).
- No new crates without an ADR; the default build stays offline (no network crates).
- The golden baseline is a no-regression gate: improvements are allowed, silent regressions are not. Never regenerate it as a side effect.
- Rust2024/MSRV1.88; official cli-full; no paid/live calls.
- No tokenizer/index/schema change, ACL/MMR/pagination work, #12/#47/#48 edits.
- User explicitly authorized squash merge after green native checks, overriding the historical human-only merge process for this session.

## R1 — runtime and regressions

Files: search/settings.rs; daemon/tools.rs; new daemon/search_refill.rs
(request-local pool logic); tests/dedupe_refill.rs; tests/search_payload_budget.rs;
search/operation_tests.rs and public docs/spec deltas.

Interfaces: SearchSettings/SearchPatch.dedupe_max_candidates; private Refill
state consumes typed policy, ordinary base, args; capacity over chunk_id doc bits.
No new public request args or SearchMeta struct field. Explain-only refill object
is attached by PreparedSearch.finish, so normal response remains an array.

- [x] RED actual40+10 corpus lexical limit10 currently returns2. Also cap/no-cap,
  only-one-doc/exhaustion, >1000 dominant/budget, configured small ceiling,
  RRF score, best late rerank chunk, scopes, generation/cancel/byte cleanup.
- [x] Run `cargo test --locked --features cli-full --test dedupe_refill` and
  record actual expected assertion failure before changing runtime.
- [x] Add owner default1000, Count provenance, 1..=1000 validation; file/typed
  overrides and project rejection, not consumer fallback.
- [x] Implement local pool selection and loop:
  ```text
  fetch=base; ceiling=max(base,policy.dedupe_max_candidates)
  dense=one_lookup(ceiling) if cap+RRF
  loop:
      ordered=preserve_base(rrf(lexical,dense.prefix(fetch)),fetch)
      if capacity>=limit || exhausted || fetch==ceiling: break
      fetch=min(fetch*SEARCH_UX_OVERSCAN,ceiling)
      lexical=read_phase(expected_generation,scope,fetch)
  drop(scope); capture(payload); rerank_once; cap; limit
  ```
- [x] GREEN focused tests, lib/CLI/IPC/hybrid regression suites, clippy/fmt.
- [x] Fresh R1 review via immutable diff package, fix findings, ledger/commit.

## R2 — measurement and local acceptance

Files: product docs/CHANGELOG, reports/dedupe-refill-2026-11-11.md, ledger.

- [x] Release diagnostic no-refill/refill/budget with real index and offline
  deterministic vectors. Record time, source widths, retained pool, rounds.
  Compare lexical-only group collection alternative as an algorithm tradeoff,
  not a performance claim about an unimplemented equivalent production path.
- [x] ru_morph dedupe_slice_diagnostics and unchanged metrics_match_baseline;
  full cli-full tests, clippy all-targets + opt-in targets, fmt, relevant release
  resource/static/offline checks. No baseline updates.
- [x] Fresh R2 review, ledger/commit.

## R3 — reviewed native delivery

Process clarification 2026-11-11: native CI requires a published commit; R2
local evidence and R3 forge delivery have distinct completion records.

- [ ] Fresh final whole-branch review, fix findings, commit final ledger.
- [ ] Push PR closes #68 only; docs/linux/macos/windows green on final HEAD.
  Squash merge exact checked HEAD, verify issue closure and main commit.
  Final forge outcomes go in the PR evidence comment without a new CI-only
  documentation commit after each run.
