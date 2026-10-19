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
- Task T4: complete (2026-10-19). Content B: 16 документов
  (`phrases/` 6: exact/bag-of-words пары + 3 pure-trap; `cjk/` 5: зеркальные
  пары 新年/新聞, 地下鉄/地下室 + 北京; `ar/` 5: same-root пары
  مكتبة/مكتب, مدرسة/درس + بوابة), 30 кейсов (phrase/cjk/arabic по 10,
  quoted phrase queries, vocalized مَكتَبة — харакат сносит нормализация).
  Verify: `query_group_b_is_valid` (счётчики + полная валидация 6 срезов),
  сьют 21 passed / 1 ignored; fmt/clippy clean. Ruling: adjudication
  11 ловушек отдельным агентом до коммита — 11/11 CONFIRM. Reviewer:
  PASS/APPROVED + 1 Important (ar-06 спан без второго терма → [5,5])
  + 3 minor (cjk-02 произвольный спан → точный compound-запрос;
  group A тест возвращён к slice-standalone валидации; 7/10 unquoted —
  оставлено: 3 quoted + 3 quoted-trap уже тестируют порядок, варианты —
  робастность). T6 наследует чистые группы A+B.
- Task T5: complete (2026-10-19). Content C: 24 документа
  (`mixed/` 4: RU/EN/CJK миксы; `semantic/` 7: парафразы; `traps/` 13:
  зеркальные near-duplicate ловушки), 38 кейсов (mixed 10 / semantic 14 /
  hard 14). Verify: `query_group_c_is_valid` (счётчики + полная валидация
  9 срезов, Jaccard-гейт), сьют 22 passed / 1 ignored; fmt/clippy clean.
  Ruling: adjudication 27 кейсов (13 trap + 14 semantic): 9 REJECT —
  4 парафраза оказались BM25-тривиальны (переписаны в настоящие
  парафразы: sem-03/05/09/11), 5 причин неточны (перепроверены грепом
  по текстам ловушек, включая невыявленные тем же классом ошибки
  в hard-07/12). Второй круг ревью: reasons сверены дословно с текстами
  (подстрока ≠ токен: `config` в `configurable` не считается; `trace`
  отсутствует в debug-доке).   Minor: notes sem-03/05 уточнены.
- Task T8: complete (2026-10-19). Метрики и агрегаты: checked-in
  `NDCG_DISCOUNT[20]` (литералы спеки дословно, Decimal-аудит: в пределах
  1 ulp от истинных; `LOG10_2` бит-в-бит), `evaluate_case`/`evaluate_at`,
  `EarlyMetrics` (= spec: только @3), `OutcomeMatrix` by-slice,
  `aggregate_slices`, `bootstrap_ci` (nearest-rank 24/974, канонический
  xorshift64*), `delta_report`, `classify_case`. Verify: 10 unit-тестов
  с ручными примерами + aggregate/delta тесты, сьют 40 passed;
  fmt/clippy clean; `cargo tree` без reqwest. Ruling: единый предикат
  `matches_hit` (path + lines-if-given) для gain/recall/MRR/overlap/
  forbidden — подтверждён focused-ревью как буква спеки (§5.6 generic
  sentence + `grade` clause + ADR-B2); path-only gain отвергнут.
  Review: PASS/APPROVED → fix round (9 minor: таблица→20 по спеке,
  canonical PRNG, имена/структуры по спеке, nearest-rank, untested
  forbidden-ветка покрыта, лишние allows убраны) → re-review: всё
  addressed, новой поломки нет. Minor: T6 RED-игнор остаётся до DELTA-2.
- Task T7: complete (2026-10-19). Replay fixtures + config runner:
  `VectorFixture` (manifest + `.bin` raw LE f32, atomic write, offset/len/
  finite/dim checks), `RerankFixture` (exact-set pools, top_n/index/usage
  validation, atomic write), `ReplayEmbedder`/`ReplayReranker` (fail-closed),
  `stages_for` (4 configs), `run_config` (warm-up, per-case limit,
  citations parse), `preload_vectors`. Verify: 5 новых тестов
  (missing hash, exact-set incl. order/top_n splits, out-of-range index,
  fetch window 50 на 60-doc probe, 4-config citations с record-then-replay
  reversed rerank — доказывает что стадия применяется), сьют 27 passed
  / 1 ignored (+T6 RED как planned); fmt/clippy clean; `cargo tree`
  default без `reqwest`. Ruling: record-then-replay в тесте — скаффолд
  для T9 `CollectingReranker`, не циркулярность (пулы строятся из
  наблюдаемых candidate set'ов, перевёрнутый порядок доказывает применение
  rerank); dense-вектора — детерминированные фейки (только для тестов).
  Reviewer: PASS/APPROVED + 9 minor, все закрыты кодом в таске (empty-guard,
  ссылка §5.1→§5.5, alignment-чек, load-time top_n/dup-пулы, chunk_id
  fail-closed, split set-vs-top_n атрибуции, Arc-фикстуры по дизайну,
  ConfigId Ord, T6-тест вне скоупа T7).
- Task T9: complete (2026-10-19). Record mode: `record_fixtures_with`
  (embed chunk_texts + queries → `write_vector_fixture` + `preload_vectors`,
  rerun Bm25Rerank/RrfRerank под `CollectingReranker` → exact-set pools →
  atomic `write_rerank_fixture`), `CollectingReranker` (records post-truncation
  presented docs + query-keyed top_n), `atomic_replace` (fsync+rename, no
  temp files left), `FmtUsage` Display, `VectorFixture::get`. Verify: 3 новых
  теста (record→replay byte-identical citations на reversed rerank —
  доказывает что replay идёт с диска а не из in-memory; atomicity на
  embed-fail и rerank-fail — файлы untouched; pin-mismatch rejected by
  `parse_model_pin`), сьют 43 passed / 2 ignored (T6 RED до DELTA-2 +
  corpus-inventory print); fmt/clippy clean incl. `--features hybrid-remote`;
  `tests/search_bench_live.rs` (ignored smoke: live record 5 docs/2 cases +
  offline replay 4 configs, только с экспортированным ключом).
  Ruling: self-review (stop-инструкция, fresh-reviewer отложен); два
  по-ходу фикса — `corpus_hash` контракт унифицирован на сам корпус
  (было двойное `corpus/corpus`), `record_*` берёт `&Arc<dyn Embedder>`.
