# Search Benchmark — Progress Ledger

Ledger исполнения `docs/specs/search-benchmark/tasks.md`. Одна задача — одна
запись при коммите. Ruling фиксируют принятые решения; minor — что отложено.

- Task T1: complete (2026-10-19). Fixture schema + validator + harness
  skeleton: `tests/support/{mod,bench}.rs`, `tests/search_bench.rs`.
  Verify: `cargo test --locked --test search_bench` → 18 passed;
  `cargo fmt --check` clean;
  `cargo clippy --locked --all-targets -- -D warnings` clean.
  Ruling: wire-форма `lines` — явный `Deserialize` из `[start, end]`
  (FR-2), без опоры на неявное поведение serde; `validate_queries` именован
  и сигнатурен по брифу (manifest грузится из `corpus/MANIFEST.json`
  внутри); `lexical_jaccard` — множества токенов `IdentifierTokenizer`;
  corpus/query size bounds — в T6 (`fixtures_are_valid`, чанки требуют
  T2-env); `queries_hash` — в T10 (baseline); absent lines валидируются
  наравне с expected (FR-10). Review: fresh-reviewer FAIL → fix round
  (12 findings) → re-review: 10 addressed, 2 закрыты кодом
  (manifest escape-gate, rerank pin string coverage); minor (deferred): нет.
- Task T2: complete (2026-10-19). Legacy corpus + parity: побайтовая копия
  10 `.md` (без `baseline.json`, `diff -r` чистый кроме него), `legacy.json`
  (12 кейсов, k=3, `source="legacy"`, absent trap с `reason` + 2 reviewers),
  `legacy-baseline.json` (копия старого baseline), `BenchEnv`/`build_env`
  (copy tree, `ensure_project`, `run_full(Config::default())`, 10..=10,
  chunk join invariant), `run_legacy_parity` (Bm25, hit/absent_hit vs
  legacy-baseline, nDCG diagnostic). Verify:
  `cargo test --locked --test search_bench legacy_parity_matches_old_baseline`
  → 12/12 parity, полный сьют 19 passed; fmt/clippy clean. Ruling: reviewer
  FAIL про `reviewed_by` у всех 12 legacy-кейсов отклонён — T2 Step 3
  перечисляет поле, но ≥2 требует только FR-26 (critical/absent);
  legacy-классы некритичны, absent только у legacy-10 (2 reviewers есть).
  Minor закрыты кодом: per-case dump при mismatch, join по (query, class),
  verbatim reason из брифа.
- Task T3: complete (2026-10-19). Content A: 22 документа
  (`ru/`, `en/`, `api/` + 2 frozen-real: `docs/MCP.md`, `docs/QUICKSTART.md`,
  license MIT, `blake3`/`frozen_at` в `MANIFEST.json`), 32 кейса
  (ru 12 / en 10 / id 10, все с `lines`+grade+notes, ловушки с `reason` +
  2 reviewers). Verify: `query_group_a_is_valid` (счётчики + полная
  валидация) зелёный, полный сьют 20 passed / 1 ignored; fmt/clippy clean.
  Ruling: adjudication 8 ловушек отдельным агентом до коммита — 8/8
  CONFIRM (релевантность absent, лексическая правдоподобность, точность
  reason и строк). Reviewer: PASS/APPROVED + 4 minor (слабые
  флективные дистанции ru-morph-05, en-morph-09/10, частичный спан
  ru-morph-09) — исправлены в таске. T6 наследует чистую группу A.
