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
