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
