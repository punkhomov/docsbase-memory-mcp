# Search Telemetry — Progress Ledger

Ledger исполнения `docs/specs/search-telemetry/tasks.md`. Одна задача — одна
запись при коммите. Ruling фиксируют принятые решения; minor — что отложено.

- Spec rev 0.2 (2026-10-31): первый драфт заблокирован ревью (3×P1) и исправлен:
  конверт заменён на opt-in `explain` (конституция: ломающее — только в мажоре),
  `cost_usd` вынесен в `search-cost` (нет канала usage в проде), `status.ann`
  получил хранилище `AnnOutcome` (у `status` нет доступа к `DenseCache`).
  Ruling: форма ответа по умолчанию не меняется ни в одном из вариантов спека.


- Task T0: complete (2026-10-31). Design + characterization test.
  `docs/specs/search-telemetry/design.md`; `tests/hybrid_ranking.rs` gained
  `default_response_shape_is_the_bare_citation_array` (all four configs: bare
  array, exact key set, `lines` is `[start, end]`, no envelope) and
  `limit_zero_and_errors_carry_no_metadata` (FR-8 edges).
  Verify: `cargo test --locked --test hybrid_ranking` → 12 passed; fmt/clippy clean.
  Ruling: the outcome record lives in `DenseCache`, not in `Shared` — the cache
  already owns the ANN tier state and is already threaded into `search_docs_with`,
  so `status` gains `Option<&DenseCache>` instead of `search_docs_with` changing
  its public signature for a field the cache owns.
  Ruling: `serde_json::Value` keeps object keys sorted, so the contract is the key
  *set*, not insertion order; the test sorts before comparing.
