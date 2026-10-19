# Search Benchmark — Implementation Plan

**Goal:** дать проекту детерминированный, дискриминирующий offline-гейт для
BM25/rerank/RRF и отдельный live-отчёт качества, latency и стоимости.
**Architecture:** test-only harness в `tests/support/bench.rs`; корпус и запросы
в `tests/fixtures/search-bench/`; offline-стадии — replay-реализации
`Embedder`/`Reranker`, поиск — через `daemon::tools::search_docs_with`.
Production `src/**` не меняется.
**Spec:** `docs/specs/search-benchmark/requirements.md`
**Design:** `docs/specs/search-benchmark/design.md`

**DELTA-1 (2026-10-19):** adjudication `absent`, рубрикатор + второе ревью,
`lexical_jaccard` (expected-span, токенизация индекса, ≤0.5),
direction-agnostic disagreements (FR-26…FR-28; SC-11/SC-12) и should-слой:
pooling review, early cutoffs 3/10, outcome matrix, live Pareto (FR-29…FR-32;
SC-13). Стартовые пороги калибруются только отдельным ruling. Обновлены
T1/T2/T3–T6/T8/T10/T11; добавлена T12; live и CI сдвинуты в T13/T14.

**Global constraints (из design §2):**

- Rust edition 2024, MSRV 1.88; `cargo fmt`; clippy `--all-targets -- -D warnings`.
- Никаких новых крейтов; используются `blake3`, `serde`, `serde_json`,
  `tempfile`, `rusqlite`, `anyhow` (dev), уже присутствующие в дереве.
- Offline-гейт работает в дефолтной сборке: без `hybrid-remote`, без
  `reqwest`, без сети и без `OPENROUTER_API_KEY` (FR-21, SC-10).
- Live-тесты — только `#[ignore]` + feature `hybrid-remote`; ключ только из
  `OPENROUTER_API_KEY`, никогда не пишется в отчёт (NFR-5).
- Фикстуры — доверенные repo-controlled файлы; пути валидируются внутри
  `tests/fixtures/search-bench/` (NFR-5).
- Векторы — raw little-endian `f32`; поддерживаемые платформы little-endian
  (Linux x86_64, Windows x64, macOS aarch64) (NFR-6).
- Offline-гейт: ≤30 с debug на CI-хосте, жёсткий предел 60 с; фикстуры ≤12 MiB
  (NFR-1, NFR-2).
- Production-контракты MCP/CLI и формат `search_docs` не меняются.
- В `src/**` не добавляем `unwrap`/`expect`/`panic!`; harness живёт в
  `tests/**`, где это допустимо, но предпочитает `anyhow::Result` с контекстом.

**Review focus:**

1. **Fail-closed replay.** Missing vector или rerank-пул обязан валить тест с
   ключом, а не пропускать кейс (`T7`).
2. **Per-slice gate.** Регрессия одного среза не должна прятаться за средним;
   проверяется на синтетических baseline/current (`T10`).
3. **Adjudication, а не «нет в expected».** Каждый `absent` имеет `reason`,
   critical-кейсы — ≥2 reviewer'ов; grade 1 имеет `notes` (`T1`, `T3–T6`).
4. **Semantic не лексический.** `lexical_jaccard ≤ 0.5` (expected-span,
   токенизация индекса); иначе кейс переписывается/переносится (`T1`, `T5`,
   `T6`).
5. **Бенчмарк различает.** Saturation ≤70% и межконфигурационные
   disagreements (любое направление); пороги не понижаются молча (`T10–T12`).
6. **Legacy parity на точной копии.** 10 документов/12 кейсов, а не
   расширенный корпус; IDF не должен менять старые top-3 (`T2`, `T11`).
7. **Атомарность record mode.** Сбой на середине не оставляет валидным
   манифест, указывающий на битый `.bin` (`T9`, `T11`).
8. **Offline без сети.** Дефолтная сборка не тянет `reqwest`, гейт проходит
   без ключа (`T7`, `T14`).

Ledger `docs/specs/search-benchmark/progress.md` создаётся в T1 и
дополняется строкой `Task <ID>: complete (...)` после каждой задачи.

---

## T1 — Fixture schema + validator + harness skeleton

**Goal:** типизированные фикстуры и валидатор, которые ловят битые пути,
lines, grades, model pin и недостающие срезы до запуска поиска.
**Files:**
- Create: `tests/support/mod.rs` (`pub mod bench;`)
- Create: `tests/support/bench.rs`
- Create: `tests/search_bench.rs`
- Create: `docs/specs/search-benchmark/progress.md` (ledger; append after every
  subsequent task)
**Interfaces:**
- Produces: `QueryFile`, `QueryCase`, `ExpectedHit`, `AbsentHit`, `LineRange`,
  `CorpusManifest`, `ManifestDoc`, `ModelPin`, `Fixtures::load(root)`,
  `validate_queries(cases, corpus, required_classes)`,
  `lexical_jaccard(query, text)`, `parse_model_pin(&str)`,
  `corpus_hash(root)`, `blake3_file(path)`.
**Steps (TDD):**
1. В `tests/search_bench.rs` подключить harness: `mod support;` — Rust
   резолвит его в `tests/support/mod.rs`; `#[path = "support/mod.rs"]`
   допустим, но не обязателен.
2. RED: в `tests/search_bench.rs` добавить тесты на временных каталогах:
   - `validator_accepts_minimal_fixture`
   - `validator_rejects_expected_absent_overlap`
   - `validator_rejects_missing_path`
   - `validator_rejects_bad_lines`
   - `validator_rejects_bad_grade_and_k`
   - `validator_rejects_too_few_slice_cases`
   - `validator_rejects_duplicate_ids`
   - `validator_rejects_unknown_class`
   - `validator_rejects_mixed_model_pins`
   - `validator_rejects_path_escape`
   - `validator_rejects_hash_mismatch`
   - `validator_rejects_absent_without_reason`
   - `validator_rejects_missing_second_reviewer`
   - `validator_rejects_missing_reviewer`
   - `validator_rejects_missing_notes` (grade 1 и critical-классы)
   - `validator_rejects_semantic_lexical_jaccard`
   - `validator_rejects_semantic_without_lines`
   - `lexical_jaccard_matches_hand_computed`
3. Run: `cargo test --locked --test search_bench` — падение на отсутствующих
   типах/функциях.
4. GREEN: реализовать serde-типы, загрузку `MANIFEST.json`/`queries.json`,
   проверки из design §5.1 (включая FR-26/FR-27), парсер
   `provider/model@d<dim>@<l2|raw>`.
5. Run: `cargo test --locked --test search_bench` — зелёный.
6. Ledger: `Task T1: complete (…tests…, clippy/fmt clean)`; далее каждая
   задача дописывает свою строку в `progress.md`.
**Acceptance:** FR-24, FR-25, FR-26, FR-27; NFR-5, NFR-6; SC-2, SC-12
(частично).
**Verify:** `cargo test --locked --test search_bench` → 18 passed.
**Depends:** —.

---

## T2 — Legacy corpus + parity

**Goal:** доказать, что новый harness воспроизводит старый golden на точной
копии корпуса, до удаления старого гейта.
**Files:**
- Create: `tests/fixtures/search-bench/legacy-corpus/` — 10 `.md` файлов,
  побайтовые копии `tests/fixtures/search-quality/*.md` (без `baseline.json`).
- Create: `tests/fixtures/search-bench/legacy.json` — 12 кейсов, `k = 3`.
- Create: `tests/fixtures/search-bench/legacy-baseline.json` — копия
  `tests/fixtures/search-quality/baseline.json`.
- Modify: `tests/support/bench.rs`, `tests/search_bench.rs`.
**Interfaces:**
- Produces: `BenchEnv`, `build_env(corpus_dir, name, expected_docs)`,
  `run_legacy_parity()`, `LegacyParity`, `LegacyCaseReport`.
**Steps (TDD):**
1. RED: тест `legacy_parity_matches_old_baseline` — падает на отсутствующем
   `run_legacy_parity`.
2. Скопировать 10 документов рекурсивно с сохранением относительных путей;
   проверить `diff -r` против `tests/fixtures/search-quality` (только `.md`).
3. Перенести 12 кейсов из `tests/search_quality.rs::CASES` в `legacy.json`
   (`id`, `query`, `class`, `k=3`, `expected`, `absent`, `source="legacy"`,
   `notes`, `reviewed_by`). Для единственного `absent`-кейса (`"plan id"`)
   заполнить `reason` («must not match assessment_plan_id») и ≥2 reviewer'ов;
   остальным — ≥1 reviewer.
4. Реализовать `build_env`: copy → register project (`SCHEMA_VERSION`) →
   `run_full(Config::default())` → выставить `project.status = Indexed` →
   `repo::{chunk_hashes,chunk_texts}`.
5. Реализовать `run_legacy_parity` на `Bm25`-стадиях; сравнить `hit` и
   `absent_hit`; `nDCG` — только в отчёт.
6. GREEN: `cargo test --locked --test search_bench legacy_parity_matches_old_baseline`.
**Acceptance:** FR-16, FR-26 (legacy metadata); SC-6, SC-12 (legacy).
**Verify:** `cargo test --locked --test search_bench legacy_parity_matches_old_baseline`
→ 12/12 parity.
**Depends:** T1.

---

## T3 — Content A: ru_morph, en_morph, identifier

**Goal:** добавить документы и размеченные кейсы для морфологии и точных
идентификаторов, включая near-duplicate ловушки.
**Files:**
- Create: `tests/fixtures/search-bench/corpus/{ru,en,api}/…`
- Modify: `tests/fixtures/search-bench/corpus/MANIFEST.json`
- Modify: `tests/fixtures/search-bench/queries.json`
- Modify: `tests/search_bench.rs` (тест `query_group_a_is_valid`)
**Interfaces:** использует `Fixtures::load`, `validate_queries`.
**Steps (TDD):**
1. RED: `query_group_a_is_valid` требует `ru_morph >= 12`, `en_morph >= 10`,
   `identifier >= 10`; падает на пустом `queries.json`.
2. Авторство: ≥18 документов; синтетические near-duplicates с изменённым
   идентификатором; frozen-real копии с `origin/license/frozen_at/blake3`.
3. Разметка: 32 кейса суммарно; `expected` с `lines` и `grade`; `absent` для
   ловушек — с непустым `reason`; `source`/`notes` заполнены; `reviewed_by`
   по FR-2. Для grade 1 `notes` объясняет, чего не хватает.
4. Добавить ignored-тест `print_corpus_hashes`, который печатает
   `path blake3` для всех `.md`; заполнить `MANIFEST.json`
   (`origin/license/frozen_at/blake3`).
5. GREEN: `cargo test --locked --test search_bench query_group_a_is_valid`.
**Acceptance:** FR-1, FR-2, FR-3, FR-26 (частично); SC-4, SC-12 (частично).
**Verify:** `cargo test --locked --test search_bench query_group_a_is_valid`.
**Depends:** T1.

---

## T4 — Content B: phrase, cjk, arabic

**Goal:** покрыть фразовые запросы, CJK-биграммы и арабскую нормализацию,
включая ложные фразовые совпадения.
**Files:**
- Create: `tests/fixtures/search-bench/corpus/{cjk,ar,phrases}/…`
- Modify: `corpus/MANIFEST.json`, `queries.json`
- Modify: `tests/search_bench.rs` (тест `query_group_b_is_valid`)
**Interfaces:** использует `Fixtures::load`, `validate_queries`.
**Steps (TDD):**
1. RED: `query_group_b_is_valid` требует `phrase >= 10`, `cjk >= 10`,
   `arabic >= 10`.
2. Авторство: ≥14 документов; фразовые ловушки (`"plan id"` vs
   `assessment_plan_id`), CJK-substring, арабский без harakat/с harakat.
3. Разметка: 30 кейсов; `absent` для ложных совпадений — с `reason`;
   `lines` точные; `reviewed_by`/`notes` по FR-2 (для absent — ≥2
   reviewer'ов).
4. Обновить manifest/hashes.
5. GREEN: `cargo test --locked --test search_bench query_group_b_is_valid`.
**Acceptance:** FR-1, FR-2, FR-3, FR-26 (частично); SC-4, SC-12
(частично).
**Verify:** `cargo test --locked --test search_bench query_group_b_is_valid`.
**Depends:** T1, T3 (общий `queries.json`).

---

## T5 — Content C: mixed_lang, semantic, hard_negative

**Goal:** добавить кейсы, которые различают lexical и dense-канал, и
ловушки, которые обязаны оставаться запрещёнными.
**Files:**
- Create: `tests/fixtures/search-bench/corpus/{mixed,semantic,traps}/…`
- Modify: `corpus/MANIFEST.json`, `queries.json`
- Modify: `tests/search_bench.rs` (тест `query_group_c_is_valid`)
**Interfaces:** использует `Fixtures::load`, `validate_queries`.
**Steps (TDD):**
1. RED: `query_group_c_is_valid` требует `mixed_lang >= 10`,
   `semantic >= 14`, `hard_negative >= 14`.
2. Авторство: ≥20 документов; semantic-перефразировки на EN/RU с
   `lexical_jaccard <= 0.5` (expected-span); near-duplicate hard negatives;
   mixed RU/EN/CJK.
3. Разметка: 38 кейсов; для semantic указать, что `bm25` — negative-control
   (кейс может не находиться без `rrf`, но `absent`/grade должны быть
   корректны); `reviewed_by` ≥2 для semantic/hard_negative/absent; у
   semantic `expected[].lines` обязательны; `notes` для grade 1. Свежий
   reviewer проверяет рубрикатор и `reason`.
4. Обновить manifest/hashes.
5. GREEN: `cargo test --locked --test search_bench query_group_c_is_valid`
   (включая lexical overlap).
**Acceptance:** FR-1, FR-2, FR-3, FR-26, FR-27 (частично); SC-4, SC-5,
SC-12 (частично).
**Verify:** `cargo test --locked --test search_bench query_group_c_is_valid`.
**Depends:** T1, T3–T4.

---

## T6 — Content D: multi_relevant, long_doc_boundary + full validation

**Goal:** закрыть корпус до целевого размера и включить полный
`fixtures_are_valid` со всеми обязательными срезами.
**Files:**
- Create: `tests/fixtures/search-bench/corpus/{multi,long}/…`
- Modify: `corpus/MANIFEST.json`, `queries.json`
- Modify: `tests/search_bench.rs` (тест `fixtures_are_valid`)
**Interfaces:** использует `Fixtures::load`, `validate_queries`.
**Steps (TDD):**
1. RED: `fixtures_are_valid` требует 60–80 документов, 200–400 чанков,
   120–150 кейсов, ≥8 на каждый из 11 срезов, все hashes/lines/grades,
   `reviewed_by`/`reason`/`notes` по FR-26 и lexical Jaccard по FR-27.
2. Добавить ≥15 документов: несколько релевантных документов на запрос;
   длинные секции и ответ на границе чанка.
3. Добавить 22 кейса: `multi_relevant >= 12`, `long_doc_boundary >= 10`;
   для multi-relevant указать все grade 1/2 и `notes`; второй reviewer для
   ambiguous-кейсов.
4. Pooling cross-check (FR-29): вручную сверить новые expected/absent с
   соседними документами; спорные — в ledger. Автоматический pooling —
   после T7/T11.
5. Обновить manifest/hashes.
6. GREEN: `cargo test --locked --test search_bench fixtures_are_valid`.
**Acceptance:** FR-1, FR-2, FR-3, FR-24, FR-26, FR-27, FR-29 (частично);
SC-4, SC-12, SC-13 (частично).
**Verify:** `cargo test --locked --test search_bench fixtures_are_valid`.
**Depends:** T3–T5.

---

## T7 — Replay fixtures + environment + config runner

**Goal:** offline-прогон четырёх конфигураций через replay-векторы и
replay-rerank, fail-closed при промахе.
**Files:**
- Modify: `tests/support/bench.rs`
- Modify: `tests/search_bench.rs`
**Interfaces:**
- Produces: `VectorFixture::{load,get,write}`, `RerankFixture::{load,pool,write}`,
  `preload_vectors`, `ReplayEmbedder`, `ReplayReranker`, `ConfigId`,
  `stages_for`, `run_config`, `ConfigRun`, `Citation`.
- Preload: `preload_vectors(&env, &fixture)` заполняет `VectorCache`.
**Steps (TDD):**
1. RED: тесты:
   - `replay_embedder_missing_hash_is_error`
   - `replay_reranker_requires_exact_candidate_set`
   - `replay_reranker_rejects_out_of_range_index`
   - `run_config_uses_fetch_window_fifty_for_hybrid`
   - `run_config_returns_citations_for_four_configs`
   Синтетические 3–5 документов, векторы и rerank-пулы пишутся во временный
   каталог; `f32` little-endian.
2. GREEN: реализовать загрузку/валидацию manifest/bin, replay-стадии,
   `DenseCache` на конфигурацию, warm-up, `run_config`.
3. Проверить, что `cargo tree` дефолтной сборки не содержит `reqwest`.
4. Run: `cargo test --locked --test search_bench` — зелёный.
**Acceptance:** FR-4, FR-5, FR-6, FR-7, FR-21; NFR-6; SC-10 (частично).
**Verify:** `cargo test --locked --test search_bench` и
`cargo tree -e normal | rg reqwest` (пусто).
**Depends:** T1, T6.

---

## T8 — Metrics, slices, bootstrap

**Goal:** посчитать hit/recall/MRR/nDCG/citation/forbidden/duplicates и
агрегировать по срезам с детерминированным bootstrap.
**Files:**
- Modify: `tests/support/bench.rs`
- Modify: `tests/search_bench.rs`
**Interfaces:**
- Produces: `evaluate_case`, `CaseMetrics`, `EarlyMetrics`, `SliceReport`,
  `DeltaReport`, `OutcomeMatrix`, `aggregate_slices`, `bootstrap_ci`.
**Steps (TDD):**
1. RED: unit-тесты с ручными примерами:
   - `ndcg_matches_hand_computed_example`
   - `ndcg_suppresses_duplicate_gain`
   - `ndcg_discount_table_is_checked_in` (ранги 1, 2, 10, 20; без `log2`)
   - `recall_and_mrr_multi_relevant`
   - `citation_overlap_uses_lines`
   - `forbidden_hit_is_sticky`
   - `duplicate_path_ratio_counts_repeats`
   - `early_metrics_match_hand_computed` (cutoffs 3/10)
   - `outcome_matrix_counts_patterns` (all-hit/all-miss/mixed)
   - `bootstrap_is_deterministic`
2. GREEN: реализовать метрики и агрегаты по design §5.6–5.7; дисконт — только
   из `NDCG_DISCOUNT`, не из `f64::log2`.
3. Run: `cargo test --locked --test search_bench` — зелёный.
**Acceptance:** FR-8, FR-9, FR-10, FR-11, FR-30, FR-31 (частично);
NFR-3 (частично).
**Verify:** `cargo test --locked --test search_bench`.
**Depends:** T7.

---

## T9 — Record mode (code + fake-stage tests)

**Goal:** атомарно записывать векторы и rerank-скоры; запись должна быть
воспроизводимой на fake-стадиях без сети.
**Files:**
- Modify: `tests/support/bench.rs` (writers + `CollectingReranker`)
- Modify: `tests/search_bench.rs` (fake-stage `record_mode_*` тесты)
- Create: `tests/search_bench_live.rs` (`#![cfg(feature = "hybrid-remote")]`,
  `mod support;`, только `#[ignore] record_fixtures`)
**Interfaces:**
- Produces: `record_fixtures_with` (support, без сети), `CollectingReranker`,
  `write_vector_fixture`, `write_rerank_fixture`, `atomic_replace`.
- Live wrapper: `#[ignore] record_fixtures` вызывает `record_fixtures_with`
  с реальными стадиями (T11).
**Steps (TDD):**
1. Подключить harness в обоих test-крейтах: `mod support;` (резолвится в
   `tests/support/mod.rs`).
2. RED (дефолтная сборка, `tests/search_bench.rs`; тесты вызывают
   `record_fixtures_with`):
   - `record_mode_writes_replayable_fixtures` — fake deterministic
     embedder/reranker; replay даёт те же citations.
   - `record_mode_is_atomic_on_failure` — инжектированный сбой после записи
     `.bin`; старый manifest остаётся валидным.
   - `record_mode_does_not_mix_model_pins`.
3. GREEN: реализовать запись versioned `.bin` + manifest commit point
   (entries сортируются по `text_hash` перед конкатенацией, иначе SC-9
   ломается на порядке `HashMap`), rerank JSON, `CollectingReranker`.
4. Run: `cargo test --locked --test search_bench` — fake-stage record-тесты
   зелёные.
5. В `tests/search_bench_live.rs` добавить `#[ignore] record_fixtures`
   (реальная сеть); проверить компиляцию:
   `cargo test --locked --features hybrid-remote --test search_bench_live`
   (0 запущенных, 1 ignored).
**Acceptance:** FR-19; SC-9 (частично).
**Verify:** `cargo test --locked --test search_bench` и
`cargo test --locked --features hybrid-remote --test search_bench_live`.
**Depends:** T7, T8.

---

## T10 — Baseline, gate, determinism, sensitivity, latency, report

**Goal:** сравнение с baseline, hard-fail политика, deterministic report,
sensitivity-test и latency-гейт.
**Files:**
- Modify: `tests/support/bench.rs`
- Modify: `tests/search_bench.rs`
**Interfaces:**
- Produces: `Baseline`, `Report`, `GateFailure`, `gate_failures`,
  `latency_failures`, `DiscriminationReport`, `discrimination_failures`,
  `write_baseline`, `regenerate_baseline` (`#[ignore]`), report JSON.
**Steps (TDD):**
1. RED: тесты:
   - `gate_detects_each_failure_kind` (forbidden, hit drop, nDCG drop,
     slice drop, config/case drift)
   - `gate_allows_improvement_without_baseline_update`
   - `gate_rejects_sabotaged_reranker` (обратный порядок)
   - `gate_rejects_shuffled_embedder` (RRF ломается)
   - `evaluate_is_deterministic` (quality-часть без latency/timestamp)
   - `latency_budget_math` (unit: 1.5x + 5 ms, 500 ms ceiling)
   - `baseline_preserves_timestamp_when_unchanged`
   - `discrimination_detects_each_failure` (saturated ratio, slice,
     disagreement)
2. GREEN: реализовать baseline/report/gate/latency/report writer и
   `discrimination_failures`.
3. `regenerate_baseline` — только `#[ignore]`, пишет baseline атомарно после
   legacy parity и записывает `discrimination` для диагностики; принятие
   baseline проверяет `benchmark_is_discriminating` (T12).
4. Run: `cargo test --locked --test search_bench`.
**Acceptance:** FR-12, FR-13, FR-14, FR-15, FR-20, FR-22, FR-28, FR-30,
FR-31; NFR-1, NFR-3, NFR-8; SC-1, SC-3, SC-7 (частично), SC-9 (частично),
SC-11 (частично), SC-13 (частично).
**Verify:** `cargo test --locked --test search_bench` (fixture-dependent
  integration помечен `#[ignore]` до T11).
**Depends:** T8, T9.

---

## T11 — Record real fixtures + baseline + legacy parity

**Goal:** записать реальные векторы и rerank-скоры, сгенерировать baseline и
включить integration-гейт.
**Files:**
- Create: `tests/fixtures/search-bench/vectors/<pin_slug>/…`
- Create: `tests/fixtures/search-bench/rerank/<pin_slug>.json`
- Create: `tests/fixtures/search-bench/baseline.json`
- Modify: `tests/search_bench.rs` — снять `#[ignore]` с
  `metrics_match_baseline`, добавить `vectors_cover_all_chunks_and_queries`
  (FR-5/FR-7) и включить `fixtures_are_valid` в общий прогон.
**Interfaces:** использует `record_fixtures`, `regenerate_baseline`.
**Steps:**
1. Требование: `OPENROUTER_API_KEY` в env; иначе задача blocked (не
   подменять replay фейками).
2. Run:
   `cargo test --locked --features hybrid-remote --test search_bench_live -- --ignored record_fixtures`
   — записывает vectors/rerank.
3. Run:
   `cargo test --locked --test search_bench -- --ignored regenerate_baseline`
   — пишет baseline после зелёного legacy parity; `discrimination` пишется
   для диагностики, финальный gate — T12.
4. Run: `cargo test --locked --test search_bench` — все 4 конфигурации,
   `metrics_match_baseline`, `legacy_parity`, `fixtures_are_valid` зелёные.
   Если `benchmark_is_discriminating` красный — baseline записан, но T12
   обязана добавить кейсы; пороги не понижать.
5. Повторный прогон `record_fixtures` на неизменных входах не меняет baseline
   (SC-9).
**Acceptance:** FR-13, FR-15, FR-16, FR-19; SC-1, SC-6, SC-9.
**Verify:** `cargo test --locked --test search_bench` → 0 failed
(`benchmark_is_discriminating` может быть red до T12).
**Depends:** T10. **Blocked by:** `OPENROUTER_API_KEY` (operator).
Baseline из T11 не мержится отдельно: T11+T12 — одна цепочка, baseline
коммитится только после зелёного discrimination.

---

## T12 — Discrimination hardening (FR-28)

**Goal:** довести baseline до discriminating-порогов, добавляя сложные кейсы,
а не понижая пороги.
**Files:**
- Modify: `tests/fixtures/search-bench/queries.json`
- Modify: `tests/fixtures/search-bench/corpus/` + `MANIFEST.json`
- Modify (при новых кейсах): `vectors/<pin_slug>/…`, `rerank/<pin_slug>.json`
- Modify: `tests/fixtures/search-bench/baseline.json`
- Modify: `tests/search_bench.rs` (`benchmark_is_discriminating`)
**Interfaces:** использует `DiscriminationReport`, `record_fixtures_with`,
`regenerate_baseline`.
**Steps (TDD):**
1. RED: `benchmark_is_discriminating` падает и печатает
   `saturated_ratio`, `non_saturated_by_slice`, `semantic_disagreements`,
   `rerank_disagreements`, `semantic_max_lexical_jaccard`.
2. Добавить кейсы в слабые срезы: semantic-перефразировки (Jaccard ≤0.5),
   hard negatives, multi-relevant/boundary; `reviewed_by`/`reason`/`notes`
   по FR-26; pooling review (FR-29) для новых документов.
3. Перезаписать только затронутые векторы/rerank-скоры; перегенерировать
   baseline.
4. GREEN: `cargo test --locked --test search_bench`; ledger фиксирует
   saturation до/после.
5. Если пороги не достигаются — добавить ещё кейсы; изменение порогов —
   отдельный ruling, не тихий фикс.
**Acceptance:** FR-28, FR-29; SC-11.
**Verify:** `cargo test --locked --test search_bench benchmark_is_discriminating`
→ ok; baseline содержит `discrimination`.
**Depends:** T11. **Blocked by:** `OPENROUTER_API_KEY` при перезаписи
векторов/скоров.

---

## T13 — Live measurement + cost cap

**Goal:** реальный прогон четырёх конфигураций с quality/latency/usage/cost
отчётом и жёстким бюджетом.
**Files:**
- Modify: `tests/search_bench_live.rs` (live-клиент, `RecordingTransport`)
- Modify: `tests/support/bench.rs` (только cost/usage math без `reqwest`)
**Interfaces:**
- Produces: `live_measurement`, `RecordingTransport`, `LiveReport`,
  `cost_cap_from_env`, `estimate_cost`.
**Steps (TDD):**
1. RED: unit-тесты:
   - `cost_cap_stops_before_the_next_request`
   - `usage_is_parsed_from_recorded_response`
   - `live_report_contains_all_four_configs` (структура, без сети)
   - `pareto_marks_dominated` (quality/cost/latency)
2. GREEN: реализовать live-прогон на реальных стадиях, запись отчёта в
   `target/search-bench/live-report.json`.
3. Run (без ключа): `cargo test --locked --features hybrid-remote --test search_bench_live`
   — unit зелёные, live `#[ignore]`.
4. Manual: с ключом
   `cargo test --locked --features hybrid-remote --test search_bench_live -- --ignored live_measurement`.
**Acceptance:** FR-17, FR-18, FR-32; NFR-7; SC-8, SC-13 (Pareto).
**Verify:** unit-команда выше + manual live-отчёт с cost ≤ cap и `pareto`.
**Depends:** T8, T12.

---

## T14 — CI, docs, ledger, retire old harness, final review

**Goal:** включить гейт в CI, задокументировать команды и завершить миграцию
со старого golden-гейта.
**Files:**
- Modify: `.github/workflows/ci.yml`
- Create: `.github/workflows/bench-live.yml`
- Modify: `AGENTS.md` (команды верификации)
- Modify: `docs/specs/search-benchmark/progress.md` (создан в T1)
- Delete (после parity): `tests/search_quality.rs`,
  `tests/fixtures/search-quality/baseline.json`
- Modify: `docs/specs/search-quality/progress.md` (ссылка на преемника)
**Steps:**
1. CI: job `search-bench` (Linux) + шаг `search_bench` в Windows/macOS
   jobs; проверка `cargo tree` без `reqwest`.
2. Live workflow: weekly + dispatch, secret, upload отчёта; не required.
3. `AGENTS.md`: добавить
   `cargo test --locked --test search_bench` и live-команду.
4. Удалить старый harness отдельным коммитом после зелёного parity; если
   продукт решит оставить его как smoke — зафиксировать ruling в ledger.
5. Ledger: задача, ruling, minors, финальное ревью.
6. Включение `search-bench` в required checks — действие человека в branch
   protection; агент только оставляет job зелёным.
7. Прогнать полный набор:
   `cargo fmt --check`,
   `cargo clippy --locked --all-targets -- -D warnings`,
   `cargo test --locked`.
**Acceptance:** FR-21, FR-23; NFR-10; SC-2, SC-10, SC-11, SC-13.
**Verify:** все три команды выше зелёные; CI job `search-bench` зелёный
(в required checks его добавляет человек).
**Depends:** T12, T13.

---

## Traceability (requirements → tasks)

| Requirement | Tasks |
|---|---|
| FR-1, FR-2, FR-3 | T2, T3, T4, T5, T6 |
| FR-4 | T7 |
| FR-5, FR-6, FR-7 | T7 |
| FR-8, FR-9, FR-10, FR-11 | T8 |
| FR-12 | T10 |
| FR-13 | T10, T11 |
| FR-14 | T10 |
| FR-15 | T10, T11 |
| FR-16 | T2, T11 |
| FR-17, FR-18 | T13 |
| FR-19 | T9, T11 |
| FR-20 | T10, T13 |
| FR-21 | T7, T14 |
| FR-22 | T10, T14 |
| FR-23 | T14 |
| FR-24 | T1 |
| FR-25 | T1 |
| FR-26 | T1, T2, T3–T6, T12 |
| FR-27 | T1, T5, T6, T12 |
| FR-28 | T10, T11, T12 |
| FR-29 | T6, T12 |
| FR-30, FR-31 | T8, T10 |
| FR-32 | T13 |
| NFR-1 | T10, T14 |
| NFR-2 | T3–T6, T7 |
| NFR-3 | T8, T10 |
| NFR-4 | T3–T6, T9 |
| NFR-5 | T1, T14 |
| NFR-6 | T1, T7 |
| NFR-7 | T13 |
| NFR-8 | T10, T13 |
| NFR-9 | T3–T6, T14 |
| NFR-10 | T14 |
| SC-1 | T10, T11 |
| SC-2 | T14 |
| SC-3 | T10 |
| SC-4 | T3–T6, T7 |
| SC-5 | T5, T8 |
| SC-6 | T2, T11 |
| SC-7 | T10, T14 |
| SC-8 | T13 |
| SC-9 | T9, T11 |
| SC-10 | T7, T14 |
| SC-11 | T10, T12 |
| SC-12 | T1, T3–T6 |
| SC-13 | T8, T10, T13 |

**Self-review:** каждая FR/NFR/SC имеет владельца; задачи упорядочены по
зависимостям; T11 и T12 требуют внешнего ключа при записи фикстур и явно
помечены blocked; новые крейты не появляются; production `src/**` не
меняется.
