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
- Task T5: complete (2026-10-19). Content C: 24 документа
  (`mixed/` 4: RU/EN/CJK миксы; `semantic/` 7: парафразы; `traps/` 13:
  зеркальные near-duplicate ловушки), 38 кейсов (mixed 10 / semantic 14 /
  hard 14). Verify: `query_group_c_is_valid` (счётчики + полная валидация
  9 срезов, Jaccard-гейт), сьют 22 passed / 1 ignored; fmt/clippy clean.
  Ruling: adjudication 27 кейсов (13 trap + 14 semantic): 9 REJECT —
  4 парафраза оказались BM25-тривиальны (переписаны в настоящие
  парафразы: sem-03/05/09/11), 5 причин неточны (перепроверены грепом
  по текстам ловушек, включая невыявленные тем же классом ошибки
  в hard-07/12). Второй круг ревью: reasons сверены с текстами
  дословно (подстрока ≠ токен: `config` в `configurable` не считается);
  sem-11 переписан в zero-overlap. Minor: notes sem-03/05 уточнены.
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
