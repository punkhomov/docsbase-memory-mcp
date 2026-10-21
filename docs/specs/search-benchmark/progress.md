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
- Task T6: complete (2026-10-19). DELTA-2 принята решением заказчика:
  FR-1 500–1000 → 200–400 чанков (замер 62 дока → 132 чанка при
  MAX_CHUNK_CHARS=1500; масштаб отдаём внешним бенчам как primary,
  внутренний корпус — на глубину). Контент: +16 доков (78 всего),
  +22 кейса (122 всего): `multi/` 8 доков в 4 топик-кластера × 12 кейсов
  mr-01…mr-12 (по 2–3 expected, grade-1 с notes про разрыв), `boundary/`
  8 доков × 10 кейсов lb-01…lb-10. Размещение ответов измерено чанкером:
  8/8 primary-спанов boundary straddle разрез или лежат в оверлапе
  (видны из двух чанков), lb-09/lb-10 — глубокие ответы (честно указано
  в notes). Pooling (FR-29, BM25 top-5): все 22 grade-2 primary в top-5
  (в основном rank 1–2); confusables (traps/config-json, log-trace,
  frame-budget) всплывают рядом с expected — задокументированы, не
  благословлены. MANIFEST: +16 записей с blake3. Verify: `fixtures_are_valid`
  GREEN (78 docs, 122 cases, 300 chunks, sliced minimums, DELTA-1 gates);
  сьют 44 passed / 1 ignored; fmt/clippy clean.
  Ruling: fresh-reviewer adjudication (22/22 CONFIRM с правками): B1 —
  второй reviewer на все кейсы; B2 — mr-05 += heartbeat g2, mr-10 +=
  retention g2 (оба primary не покрывали вторую половину запроса);
  ACCEPT A1/A2/A6/A7/A8/A9/A10(g2)/A11 + notes-предложения; REJECT
  A3/A4/A5/A12 как absent — gate-break (ловушки в топе над expected
  покраснили бы baseline с первого дня; FR-10 + FR-14(a)). Отклонённые
  ловушки (frame-budget vs 64MB/контракт, backup-journal vs миграция,
  index-rebuild vs проверка бэкапа) записаны как кандидаты в будущие
  hard_negative, когда ранжирование разведёт их с expected. Reviewer minors:
  слабейшие grade-1 (mr-02 http-сторона, mr-08 levels-сторона) и
  single-expected mr-07 — на пересмотр в T7+ pooling, сейчас честные notes
  спасают. A1-спан уточнён [84,87]→[85,87] (84-я строка пустая).
- Task T10: complete (2026-10-20). Baseline/gate/report: `Baseline`,
  `Report` (+`deltas` via paired nDCG, `warnings`), `Failure`/`FailureKind`
  (12 kinds), `gate_failures` (FR-14 a–ж: versions/hashes/pins, forbidden,
  hit/nDCG drops, slice regressions, missing/extra config, case/slice drift,
  legacy parity), `latency_failures` (pure math, Linux-gating at call site),
  `discrimination_failures` (FR-28 six rules), `quality_json` (deterministic,
  no latency/timestamps), `write_baseline` (canonical skip-if-identical +
  atomic), `replay_stages` (fail-closed pins), `regenerate_from_fixtures`,
  `run_config_timed` (per-call latency; `run_config` delegates).
  Verify: 15 новых тестов (7 FR-14 failure kinds по отдельности, improvement
  pass, 2 sabotage end-to-end, determinism, latency math, baseline IO incl.
  SC-9 restamp, discrimination 6+1, missing-file hard fails); сьют 59 passed
  / 3 ignored; fmt/clippy clean; полный default-сбор зелёный.
  Ruling: fresh review NEEDS CHANGES → все 8 принято и закрыто кодом:
  (1) skip-compare по canonical без generated_at/index_build_ms (SC-9);
  (2) legacy nDCG через NDCG_DISCOUNT вместо libm log2 (к Baseline bytes
  кроссплатформенны; parity не затронут — сравнивает только hit/absent);
  (3) quality_json покрывает legacy_corpus_hash + legacy_parity;
  (4) replay_stages bail на дублирующем пине (FR-25 fail-closed);
  (5) paired_ndcg-None → warnings, missing/extra slice → CaseDrift
  (переименование класса больше не молчит); (6)(7) добавлены тесты
  LegacyMismatch, exact-0.02, generated_at, bench_version, хэши/пины,
  extra-config, zero-entry coverage; (8) compute сидит все классы в 0
  (документировано: полнота из compute, не из сигнатуры). Отклонений нет.
  Sabotage-механика зафиксирована: rerank-тест пинит b.md (length-norm бьёт
  5×TF — измерено), shuffle-тест на BM25-tied тройке с self-calibration
  top-2 (2-доковый вариант давал точную RRF-ничью 1/61+1/62 — тоже измерено).
  minor: `Baseline.index_build_ms` (требует FR-22, в дизайне §5.8 поля не
  было — расхождение зафиксировано здесь, гейт поле игнорирует).
- Task T11: complete (2026-10-21). Record mode enforces determinism:
  warm-up presentations polluted pools AND the provider is not
  bit-deterministic across calls (same query embedded twice → different
  RRF order), so observation now runs through the just-recorded fixture
  (`observe_rerank_pools` + `observe_pools_is_deterministic` test).
  Recorded under production pins: 520 vectors (300 chunks + 193 queries …
  exact counts in manifest), 364 rerank pools (warm-up dedups by key).
  NFR-2 guard: `write_vector_fixture` sweeps orphan `.bin`s after the
  manifest commit (re-records used to pile orphans: 4 bins found and
  removed) + `vector_rewrite_sweeps_orphan_bins` test. Fixtures now
  5.5 MiB total (was 11.4 MiB with orphans).
- Task T12: complete (2026-10-21). +54 cases, +3 en docs (irregular
  morphology), bounds 60–85 docs / 120–200 cases (DELTA-2 amendment).
  Fresh review: 70 CONFIRM, 1 notes fix (enm-15 `written` occurs in 2
  non-relevant docs), 0 REJECT; singleton-query design ACCEPTED;
  mix-14..16 stay under boundary. Pooling (FR-29): every new primary
  verified top-5 + confusables documented (A3–A5/A12 rejected as absent —
  gate-break, recorded as future hard negatives).
- DELTA-3 (2026-10-21): FR-10 rank-1 trap rule + known-failures tolerance
  (gate fails only on NEW failures; fixed surface as improvements).
  Rank-1 traps: ar-01 (office ties library — genuine engine defect, tracked
  as issue #47), hard-07/08/10, hn-19/22 (6 cases, 9 known entries).
  Policy: verdict is a required check; merge on red needs human override
  in PR. Trap ranks below 1 → diagnostics, not failures.
- Free defaults (FR-33): `nemotron-3-embed-1b:free@d2048` +
  `nemotron-rerank-vl-1b:free`. Pilot: Rrf 18 vs 15 hits / 0.594 vs 0.512
  nDCG (20 queries); rerank top-1 6/6. Fixed dims (no MRL — HTTP 400
  otherwise, caught by probe). Record pacing for `:free` (20/min).
  Privacy note: free routes may train on prompts; paid stay available
  via explicit `models use`. Previous paid fixtures deleted (superseded).
- models-use CLI: `docsbase models use --embedding/--dims/--rerank/--window/--dry-run`
  (manual-only, no discovery); validates shapes, probes once per stage,
  merges into `.docsbase.toml` preserving other keys. Fresh review caught:
  resolve returned bare table (dry-run printed unpinnable fragment) —
  fixed + roundtrip regression test; spec/CI incoherence (advisory claim
  vs required job) — fixed by required-check-with-override policy.
- Verify: full suite 64 passed / 2 ignored; discrimination green on free
  fixtures (saturated 0.694, sem disagreements 10, rerank 6, all slices
  covered); fmt/clippy clean (default + hybrid-remote).
- Ruling: self-gate in regen can only yield ForbiddenHit (versions/hashes/
  pins/deltas identical by construction; latency/discrimination not in
  gate_failures) — verified: live baseline.json known_failures are 9/9
  ForbiddenHit. Fixed-known re-matching stale entries is prevented by
  the documented fix→regen requirement (DELTA-3/FR-15).
- Task T13: complete (code + units; live execution pending free-tier reset).
  `RecordingTransport` (usage parse + cost cap per request),
  `estimate_cost_usd` (free $0 / qwen $0.01+M / cohere $0.001/call, unknown
  fail-closed), `cost_cap_from_env` ($1 default), `check_cost_cap`,
  `pareto_frontier`, `live_measurement` (ignored, writes
  `target/search-bench/live-report.json` with quality/latency/usage/cost/
  pareto + baseline deltas), `PacingEmbedder`/`PacingReranker` for the
  free 20/min tier, index-time backfill in live runs (production parity).
  Verify: 5 unit tests (cap, usage parse both shapes, live report shape,
  pareto dominated/frontier); live_measurement ran 12 min then hit the
  free DAILY cap (1000/day, resets 00:00 UTC) — code path proven to the
  cap, full report pending reset. Free-tier lesson recorded: bulk jobs
  need pacing (done) + daily-cap chunking (record in slices if needed).
- Task T14: complete. `bench-live.yml` (weekly + dispatch, artifact upload,
  continue-on-error, advisory by design). Old harness retired
  (`tests/search_quality.rs` + `search-quality/baseline.json` deleted;
  corpus stays for `search_golden`; legacy parity was green 12/12).
  AGENTS.md: bench commands + DELTA-3 policy (required check, human
  override in PR; live advisory). Design §5.8 aligned (rank-1, known set).
- Ruling: fresh review NEEDS CHANGES → all accepted: Blocker 1 (models use
  wrote rootless fragment — wrapped in `hybrid` + roundtrip regression
  test); Blocker 2 (advisory-vs-required incoherence — required check +
  human override); M1 (dims without embedding rejected), M2 (window kept
  when unset), M3 (ends_with + skip-first pacing + dedup before scoring),
  M4 (known rank-1 traps in diagnostics), M5 (fix→regen documented),
  M6 (tolerance test from a real gate Failure; sticky test renamed),
  M7 (trimmed model ids). T13 live execution is the only open item
  (blocked on the free daily reset, not on code).
- Task T13: complete (code + units; live execution blocked on the free
  daily cap — 1000/day, resets 00:00 UTC; rerun after reset, report lands
  in target/search-bench/live-report.json). `RecordingTransport`
  (usage parse + per-call cost + worst-case preflight cap),
  `estimate_cost_usd` (free $0 / qwen $0.01+M / cohere $0.001/call,
  unknown fail-closed), `cost_cap_from_env` ($1 default, garbage counts
  as unset), `pareto_frontier`, `live_measurement` (ignored, paced,
  backfills dense cache like production, asserts cap). Free-tier pacing
  for both embed and rerank (`PacingEmbedder`/`PacingReranker`, 4 s).
  Verify: 5 unit tests (cap incl. exact-fit, usage shapes, cost table,
  report shape, pareto) green; live run started, hit the daily cap at
  backfill (no report yet — expected, not a code defect).
- Task T14: complete. `bench-live.yml` (weekly Monday 06:00 UTC +
  dispatch, continue-on-error, artifact upload; secret
  `OPENROUTER_API_KEY`). Old harness retired (test + old baseline.json
  deleted; corpus stays for search_golden; parity was green 12/12).
  AGENTS.md: bench commands + DELTA-3 gate policy (offline required with
  human override; live advisory). Design §5.8 aligned (rank-1,
  known-failures, fix→regen). Search-quality ledger closed with successor
  pointer. Full suite: default 516 passed, hybrid 546 passed, 0 failed.
- Reviewer (final): B1 double-count fixed via spent deltas; B2 paid-rerank
  substring fixed to require `:free`; B3 stale refs fixed
  (search_bench message, AGENTS map, tantivy comment, hybrid_live docs).
- Review fix round (2026-10-21, 7 findings, all accepted):
  R1 diagnostics persisted — `Report.trap_diagnostics` + `attach_trap_diagnostics`,
  in `quality_json`, `target/search-bench/gate-report.json` (offline gate),
  `trap_diagnostics` in `live-report.json` (`trap_diagnostics_persist_in_report`
  test); R2 `models use` writes atomically (temp + rename) and validates the
  merged table — stale window/zero dims/blank model/unknown ranking fail with
  fix hints (3 new unit tests); R3 docs unified — README links point at
  `legacy-baseline.json`, one bound set 60–85 docs / 120–200 cases (FR-1/FR-2,
  §8, layouts, tasks T6 + dated correction of the 120–180 amendment), ADR-B4
  1024→2048, report-shape example pins → free defaults, DELTA-3 carries the
  diagnostics implementation note; R4 cost cap realigned to the spec contract
  (`DOCSBASE_BENCH_COST_CAP_USD`, $2 default) in code + workflow, pinned by
  `cost_cap_default_is_two_dollars`; R5 `hybrid_live` smoke asserts against
  `EmbeddingCfg::default().dimensions`; R6 gate policy names the enforceable
  entity (required `linux` job, not the test name) in AGENTS/requirements/
  bench-live comment; R7 ar-01 tracked as issue #47 and linked in the ledger.
- Review fix round 2 (2026-10-21, 3 findings): R8 `bench-live.yml` gains
  top-level `permissions: contents: read` like the other workflows; R9
  `cost_cap_from_raw` rejects infinities (`is_finite` — a mandatory cap
  cannot be disabled by `inf`/`1e999`; NaN already fell through) + pin
  tests via the pure function; R10 `models use` gets CLI end-to-end coverage
  in `tests/smoke.rs` (dry-run prints a paste-ready `[hybrid]` table and
  writes nothing; bare invocation fails loudly) — units covered the merge,
  E2E covers the wiring.
- Blocker 1 resolved (2026-10-21, customer-approved): FR-10/FR-8/FR-14(a)/
  SC-5/P2 rewritten as the rank-1 rule (DELTA-3) — spec matches code and the
  `rank1_forbidden_only` test; design §5.6 rule updated; rollback rejected
  (corpus is built for top-10 traps, zero-tolerance could never go green).
- Blocker 2 noted minimally (2026-10-21, no inflation per customer):
  CONFIG.md carries the true free defaults + a short privacy note, README/
  README.ru one line, CHANGELOG entry, `models use` prints a one-line
  stderr note for `:free` routes (FR-33 condition now met in docs + CLI).
- Blocker 3 deferred (customer decision): FR-2 `reviewed_by` ≥1 for ordinary
  cases stays unenforced; risk-based validator (≥2 critical/absent) stands.
- Blocker 4 deferred (customer decision): regenerate may still launder new
  rank-1 traps into known-failures; technical guard (old known-set +
  accept flag + regression test) pending; FR-15 process guard is the only
  protection meanwhile — regenerate deliberately and diff known Failures.
