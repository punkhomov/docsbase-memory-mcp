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

- Spec re-review (2026-10-31): **ACCEPT WITH NOTES** (1×P1, 2×P2, 2×P3), все
  исправлены в rev 0.3.
  P1: запись исхода живёт в памяти процесса, поэтому одноразовый `docsbase search`
  её не переживает, а `docsbase status` без демона её не увидит — запись объявлена
  daemon-only, CLI отвечает `unknown` + `daemon not running`; в enum добавлен
  `unknown`, `disabled` зарезервирован за конфигом.
  P2: `Verify:` ссылались на несуществующие цели (`search_response_shape`,
  `status`) — T0–T2 переведены на `--test hybrid_ranking`, T5 создаёт
  `tests/status.rs`; SC-1 переформулирован: форма проверяется на двух швах
  (`search_docs_with` и MCP-граница), плюс побайтовое сравнение одной пары
  (запрос, конфигурация), потому что набор ключей не ловит дрейф значений.
  P3: в `design.md` поправлен список вызовов `status` (реально `tests/registry.rs`
  и `tests/vcs.rs` ×2, не `cleanup.rs`) и смысл `None` (`unknown`, не `disabled`).
  Ruling: состояние, которое живёт в процессе, не может обещаться CLI-процессу;
  лучше честный `unknown`, чем `disabled`, который врёт о причине.

- Task T1+T2: complete (2026-10-31). `explain` on the boundary + `SearchMeta`.
  `src/hybrid/meta.rs` (ScoreScale/Stage/Degraded/SearchMeta/envelope + 3 unit
  tests: OTel pairs unique, token set pinned, JSON field names); `search_docs_with`
  gains `explain` (bool, wrong type = protocol error), the envelope is built by
  `build_meta` from what actually ran, and argument parsing moved to
  `parse_search_args` (clippy line budget). MCP schema + `docsbase search --explain`.
  Tests: `explain_reports_the_scale_and_the_stages_that_ran` (3 configs, scale +
  stages + window + generation + degraded), `explain_follows_the_effective_path_not_the_config`
  (`rerank: false` answers `rrf`, provider not called), `explain_wraps_the_empty_result_and_rejects_a_bad_flag`.
  Verify: `--test hybrid_ranking` 15 passed; full default suite 63 `test result: ok`;
  clippy 0; fmt clean.
  Ruling: `window` is the fetch that actually happened — `limit` for a lexical
  search, `max(limit, window)` when fused/reranked; the first version of the test
  asserted `50` for the lexical case and was wrong, not the code.
  Ruling: T1 and T2 landed together — an envelope without the fields is not
  observable, and splitting them would have added a throwaway shape.

- Task T3: complete (2026-10-31). Stage timings. `stage_ms` covers bm25, dense,
  rrf, rerank and `total`, each from an `Instant` span nested inside `started`;
  `elapsed_ms` saturates instead of wrapping. Tests:
  `stage_timings_are_nested_and_cover_only_what_ran` (a reranker that measures how
  long it actually held the stage; `stage_ms.rerank >= held` and
  `sum(stages) <= total`) and `a_stage_that_did_not_run_has_no_key`.
  Verify: `--test hybrid_ranking` 17 passed.
  Ruling: the test's workload is a *measured* hold (the reranker records its own
  span), not a `sleep` used to pin "in flight" — the Flake #9 lesson applies to
  synchronisation, and a measured lower bound needs no tolerance.
- Task T4: complete (2026-10-31). `degraded` tokens. `dense_candidates` records
  the reason at the point of the decision (`ann-building` for `Building`,
  `ann-insufficient` for a short ANN answer, `ann-error` for a failed lookup or a
  `Failed` entry, `exact-fallback` for `TooLarge`), `degrade()` deduplicates and
  preserves order; `search_docs_with` records `no-rerank-stage` when a project has
  a reranker and the request opts out. Tests: `opting_out_of_an_available_rerank_stage_is_reported`
  and, in `hybrid_rank_ann`, the during-build search now asserts
  `degraded == ["ann-building"]`.
  Verify: `--test hybrid_ranking` 18 passed, `--test hybrid_rank_ann` 1 passed;
  full default suite 63 `test result: ok`; clippy 0; fmt clean.

- Task T5: complete (2026-10-31). `AnnOutcome` + `status.ann` + `index_generation`.
  `DenseCache` gains `outcomes` (one record per project, capped at
  `limits.max_projects`, oldest dropped) with `record_outcome`/`outcome`/
  `ann_snapshot`; `dense_candidates` records at each decision point; `status`
  takes `Option<&DenseCache>`, emits `index_generation` and `ann`, and
  `ann_state()` maps absence → `unknown` (no daemon), no record + hybrid off →
  `disabled`, no record + hybrid on → `loading`, stale record → `stale`.
  `tests/status.rs` (new target): unknown without a daemon, ready after a
  published ANN search (with `vectors > 0`), disabled, loading, and a recorded
  fallback naming its reason. Call sites updated: `daemon/server.rs`,
  `cli/status.rs`, `tests/registry.rs`, `tests/vcs.rs` ×2.
  Verify: `--test status` 5 passed; full default suite 64 `test result: ok`;
  combo 64; clippy 0; fmt clean.
  Ruling: `status` reads the record, never the tier — no build, no disk read, and
  `vectors`/`estimated_bytes` are `null` when the graph is not resident.
  Ruling: the 2-chunk fixture can only satisfy `fetch = 2`; a `10` window made the
  ANN answer "insufficient" and the test reported `fallback` — the state was
  correct and the test's premise was not.
- Task T6: complete (2026-10-31). OTel name table in `meta.rs` (`Stage::otel()`),
  unit test `stage_names_have_unique_otel_pairs`; no OTel dependency.
- Task T7: complete (2026-10-31). `docs/MCP.md` (explain + Telemetry section +
  status fields) and `CHANGELOG` [Unreleased]/Added.
- Task T8: complete (2026-10-31). `search_bench` 68 passed with
  `metrics_match_baseline` green and `baseline.json` untouched (`git diff` empty);
  `perf_budget` release 6 passed; full default suite 64; combo 64; clippy 0; fmt
  clean.
  Ruling: the baseline was not regenerated — `hits[]` is byte-identical on the
  default path, which is what SC-1/FR-7 require.
