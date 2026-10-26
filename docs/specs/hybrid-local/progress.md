# Progress: hybrid-local (итерация 4)

Ledger: одна запись на таск (что сделано, чем проверено, какие minor отложены).

- Spec opened (2026-10-23). ADR-17 записан в product design: провайдер
  `fastembed` за фичей `hybrid-local`; ORT ставится `docsbase connect fastembed`
  (runtime download, `ort-load-dynamic`), модели — только явным
  `docsbase models fetch` с pinned revision; `[providers.*]` — только в
  глобальном конфиге; `models use` пишет проект по умолчанию, `--global` —
  дефолты для новых проектов; дефолтных моделей и весов в бинаре нет.
  Version pins под 14-дневным cooldown: `fastembed = "=7.1.1"`,
  `ort = "=2.0.0-rc.13"` (пересмотреть после 2026-11-06).

- Spec rewritten to the project-design skill format (2026-10-23): user stories
  P1–P5 with Given/When/Then, data model, interfaces, assumptions/challenges,
  risk/alternatives, task plan header and per-task Consumes/Produces/Verify.
  Red-team pass at this gate: (1) unstated — offline failure mode for a
  half-fetched model is now explicit (`.complete` last, admission error);
  (2) adversarial — runtime is executable code, so only explicit connect with
  pinned sha256; (3) scale — model cache is per-user and can hold several GB,
  documented in NFR-8/CONFIG; (4) sunset — pinned `fastembed`/`ort` have a
  revisit date (2026-11-06) and `ORT_DYLIB_PATH` is an escape hatch;
  (5) incentive — no defaults, so nobody can claim "it just works" without
  provisioning. Status: awaiting approval before T1.

- Web verification (2026-10-23, exa/docs/HF API) corrected the 0.2 draft:
  - ORT 1.24.4 publishes five assets (linux-x64/aarch64, osx-arm64, win-x64/arm64);
    macOS x86_64 was dropped and minimum macOS is 14.0. `ort v2` maps to ORT 1.24.4
    (`api-24`), so the pin is a version, not a range.
  - Model sizes: bge-small-en-v1.5 onnx 133 MB; multilingual-e5-small fp32 470 MB /
    int8 118 MB; bge-reranker-base 1.11 GB; jina-reranker-v1-turbo-en int8 38 MB.
    The default reranker is now jina v1 turbo int8 (Apache-2.0, 8K); jina v2 is
    CC-BY-NC-4.0 (verified) and stays excluded.
  - E5 requires `query: `/`passage: ` prefixes and BGE v1.5 a query instruction; a single
    `embed` call silently degrades retrieval. FR-13 adds `embed_query`, registry
    prefixes and `prompt_version` in `model_id`; SC-9 pins it with a test.
  - fastembed 7.1.1 loads embedding ONNX as `Vec<u8>` (no file source); 7.5 adds
    `OnnxSource` for embeddings and is cooldown-blocked until 2026-11-06. The int8
    model choice keeps the 7.1.1 peak acceptable; rerank already supports files.
    (Corrected 2026-10-23 after source review: 7.5.0 still uses `Vec<u8>` for
    embeddings; `OnnxSource` is rerank-only and 7.5 pins ort rc.15, whose MSRV is 1.92;
    7.5.0 itself declares 1.88.)
  - HF pins are `resolve/<commit-sha>/<path>`; the tree API exposes LFS `sha256` + size,
    which the registry will store (regular files: size + computed sha256).

- Correction (2026-10-23, review question): "default reranker" was a spec bug.
  There is no default model for either stage: the registry lists candidates and
  `models use embed …` / `models use rerank …` is the only way to
  select them, with the same scope/validation rules. Config with
  `provider = "fastembed"` and no `model` is an admission error for both stages;
  `connect` never selects a model.

- Spec review (2026-10-23, independent reviewer) → revision 0.4. Blockers fixed:
  1. `fastembed` must be `default-features = false` and `ort` must be a direct
     `load-dynamic` dependency; otherwise the build downloads/links ORT and pulls
     hf-hub/image/native-tls (contradicting the offline default). Exact manifest is now in
     design §2 and T1/T7, and the tree gate covers those crates.
  2. The claim "7.5 adds file-based embedding loading" was false: 7.5.0 still stores
     `onnx_file: Vec<u8>`, and it pins ort rc.15 (MSRV 1.92). The spec now budgets one
     model copy in RSS instead of promising a future API.
  3. `models use` was gated on `hybrid-remote` and hardcoded OpenRouter; a local-only build
     could not write config. T6 now requires a per-provider probe (fastembed offline).
  4. FR-10's `embed_query` had no call site; T4 now changes
     `src/daemon/tools.rs::dense_candidates` and adds a spy test.
  5. Two sources of the cache key (`cache_model_id` vs `Embedder::model_id`) would break
     coverage/ANN warm; FR-9 now requires one canonical `embedding_model_id`, tested in T3.
  Majors fixed: structural `ProjectFileConfig` (no `[providers]`), runtime resolution order
  and `init_from` semantics, per-stage provider flags (`--embedding-provider`/
  `--rerank-provider`, later superseded by purpose-first `models use embed|rerank`),
  non-openrouter `model` required (OpenRouter defaults unchanged),
  windows/macos feature checks + required-check note + static binary check, exact cooldown
  pins, NFR-4 thread/budget wording. Added T9 (remove/uninstall/re-pin/observability) and
  SC-10/SC-11; dropped `connect --models`, `DOCSBASE_MODEL_CACHE`, OQ-2/4/6.

- CLI UX change (2026-10-23, operator request): `models use` becomes purpose-first —
  `models use embed <model>` / `models use rerank <model>`. One stage per invocation,
  options scoped to the purpose (`--dims` only for embed, `--window` only for rerank),
  only that stage is written, and setting both stages at once is impossible by CLI shape.
  `--provider` is inferable from a registry name and required otherwise; mixed providers
  are expressed by two separate invocations. FR-8/P3/SC-5, design data-flow, T6 and ADR-17
  updated.

- Spec review 2 (2026-10-23, `reviewer-muse`) → **NEEDS-REVISION**, 5 blockers + 5 majors
  + 5 minors; revision 0.5. Blockers fixed: P4 model_id gained `@p<prompt_version>`;
  provider-aware `model`/`dimensions` resolution (OpenRouter defaults only; fastembed
  missing model = Admission, dims from registry); FR-1 scoped so only the global
  `runtime_path` reaches FR-11; fetch failures (offline/transport/hash/size/corrupted
  cache) are Admission with one action, inference stays Internal; execution order
  T1→T3→T2 and explicit CLI wiring for connect/fetch/list/remove. Majors fixed:
  `hybrid/openrouter.rs` added to T3 with a byte-identical legacy OpenRouter key test;
  one model instance per `ModelCacheKey` (model id + max_length + intra_threads + runtime);
  `connect openrouter` probes `GET /api/v1/key` (0 tokens) and `models use` probes one
  embed + one rerank with a fake `Transport`; numeric caps (2 GiB/file, 3 GiB total,
  256 MiB archive, 512 MiB extracted) and 1.5x+256 MiB free-space preflight;
  `models remove` lock/`--force` semantics. Minors fixed: exact `flate2=1.1.10`,
  `tar=0.4.46`, `zip=8.6.0` pins (9.0.0 is exactly on the 14-day boundary); stale Delta
  0.3 CLI line; registry file set instead of a fixed count; extra connect RED tests;
  ProviderState/RuntimeStatus mapping and the corrected intra_threads note.

- Delta review (2026-10-23, `reviewer-muse`, `019ccc5..4bf8048`): **READY** — all 15 prior
  findings confirmed FIXED, T1 unblocked. Three new P1s + four P2s fixed in rev 0.6:
  explicit feature matrix (`connect openrouter` under `hybrid-remote`; `connect fastembed`/
  status and `models fetch/list/remove` under `hybrid-local`; T2 verifies local-only
  compile separately); `embedding_model_id`'s fastembed arm delegates to
  `models::canonical_id` (one formatter) with a cross-test over all registry entries;
  embedder/reranker retain the shared `.lock` guard for their lifetime (T4/T5), and T9
  depends on them; §6.4 no longer says bare `OnceLock`; the probe is a single-shot path
  (no retry wrapper; 401/403 → Admission check-key, transport → Admission retry); CLI
  table lists `fetch --force`/`remove --force`; "expected bytes" defined (Σ files[].size
  for fetch, archive + extracted for connect); `files[]` is authoritative; `models use`
  requires `--dims` for OpenRouter.

- Full review 3 (2026-10-23, Mercury 3 via `inception/mercury-3:high`) → **NEEDS-REVISION**
  (2 majors, 6 minors; T1 still startable) → rev 0.7. Majors fixed: connect preflight now
  uses `target.archive_size + target.library_size` from the five-target table (OQ-1 is a T2
  deliverable; tests use a fake Target), and macOS install strips quarantine if present,
  verifies `codesign --verify --strict`, ad-hoc signs only on failure. Minors: uninstall
  scope aligned (runtime + model caches), `--path` defined (error with `--global`), T2
  Consumes clause cleaned, per-platform library file named, four tokenizer files pinned as
  a registry invariant, OQ-1 marked as T2 deliverable, `models use` added to the feature
  matrix, complete-fetch no-op lock rule, SC-5 probe gating. Mercury confirmed the BYO
  fastembed/ort facts locally and that T1 has no blockers.

- Delta review 4 (2026-10-23, muse, `5e7c73e..7f792e7`): **NEEDS-REVISION** — all Mercury
  findings FIXED; one new major + five minors fixed in rev 0.8. Major: a verified complete
  `fetch` now holds the **shared** `.lock` while verifying (never the exclusive lock), so it
  serializes with `remove`/`--force`; test renamed `fetch_complete_takes_shared_lock`.
  Minors: product-design feature list includes `models use`; tokenizer rows require at
  least the four files (presence, not count); FR-13 parenthetical separates connect vs
  fetch size sources; T6 verifies both feature combinations; T8 records macOS signature
  state and confirms `init_from`. T1 was and remains startable.

- Task 1: complete (commits `3e53690..`→ head of this session, tests: `cargo test --locked
  --features hybrid-local --lib hybrid::models` → 15 passed; `--test cli_models` → 5 passed;
  `--test hybrid_models` → 4 passed; `cargo test --locked` default → green, 0 of fastembed/
  ort/tokenizers/hf-hub/image/native-tls/ureq/reqwest in the default tree).
  Реализовано: `src/hybrid/models.rs` (registry: bge-small-en-v1.5, multilingual-e5-small,
  jina-reranker-v1-turbo-en, bge-reranker-base с реальными pinned revisions/size/sha256 из
  HF tree API; `entry`, `fetch`, `fetch_one`, `cached`, `cache_dir`, `canonical_id`,
  `expected_bytes`, `HttpModelSource`; `.complete` last; shared-lock verify path vs exclusive
  install lock; `--force`; caps 2 GiB/file + 3 GiB/model; preflight 1.5x+256 MiB с именами
  required/available); `src/cli/models.rs` (+`Fetch`/`List`, fetch_cmd/list_cmd);
  `src/cli/mod.rs` dispatch; `src/platform/{mod,unix,windows}.rs` (+`fs::available_space`);
  `Cargo.toml` (feature `hybrid-local`, exact pins per design §2; `libc` direct для unix).
  Cargo.lock сгенерирован под nightly с `min-publish-age=14d` (resolver не нашёл нарушений).
  Ruling: `cache_dir` возвращает `PathBuf` (fallback на cwd при ошибке XDG), а не `Result` —
  как в design §5; ошибка всё равно видна на первой FS-операции. Ruling: preflight считается
  целочисленно (`expected + expected/2 + 256MiB`), эквивалентно 1.5x с truncation, без
  float-кастов, clippy-pedantic clean. Minor (deferred): manifest warns `unused dependency`
  fastembed/tar/zip (используются в T2/T4/T5); `cargo fmt` применён; `lista` CLI output
  пока без колонки selected (T6). Управляющие константы
  `PREFLIGHT_MULTIPLIER_NUMERATOR/DENOMINATOR` (3/2, целочисленно),
  `PREFLIGHT_HEADROOM_BYTES`/`MAX_FILE_BYTES`/`MAX_TOTAL_BYTES` публичны для T2/T9.
  Feature-тесты покрыты `#![cfg(feature = "hybrid-local")]` (как hybrid_openrouter.rs).

- Task 1 review (2026-10-23, `reviewer-muse`, fresh): spec compliance PASS; quality
  CHANGES — 1 Important + 3 Minor. Fix round (commit `f6c390c`): Important → новый RED-тест
  `fetch_rejects_total_over_cap` (total-ветка `install_all`, два файла по 1.5 GiB+, ни
  одного вызова source; `FakeSource` стал ленивым — не аллоцирует заявленные размеры);
  Minor #1 → убран неиспользуемый `PREFLIGHT_MULTIPLIER: f64`, порог теперь целочисленный
  `expected*3/2 + 256MiB` через публичные константы (один источник истины); Minor #2 →
  удалён `impl Default for HttpModelSource` (таймауты только через `new()`); Minor #3
  (streaming fetch вместо буфера в `Vec<u8>`) — отложен: `ModelSource::fetch` возвращает
  `Vec<u8>` по design §5, буфер транзиентный (только CLI-фаза fetch), бюджеты NFR-4
  касаются embed/rerank, а не fetch; пересмотреть при T8 live-замере RSS на
  `bge-reranker-base` (1.11 GB). Re-review: 3/3 ADDRESSED, new breakage none. T1 closed.

- Task 3: complete (commits `f6c390c..`→ head of this session, tests: `cargo test
  --locked --features hybrid-local --lib hybrid::config` → 8 passed; `--test config` →
  25 passed; `cargo test --locked` default → green).
  Реализовано: `ProvidersCfg`/`FastembedProvider`/`OpenrouterProvider`/`ProviderState`/
  `provider_status`; structural `GlobalFileConfig` vs `ProjectFileConfig` (проект без
  `providers` — `[providers]` в `.docsbase.toml` → Admission с подсказкой «move to
  config.toml», не post-merge check); provider-aware resolution `resolve_embedding`/
  `resolve_reranker` (FR-5): OpenRouter дефолты как раньше, fastembed требует `model`,
  dims из registry, mismatch → Admission, `normalize=true` обязателен; `embedding_model_id`
  — единый канонический ключ (FR-9/SC-10): OpenRouter = legacy формат byte-for-byte
  (тест `embedding_model_id_openrouter_is_byte_identical_to_legacy`), fastembed =
  `models::canonical_id` через новый `canonical_id_for` (один форматтер, cross-test по всем
  registry-строкам); все call-site'ы `cache_model_id` заменены (jobs::coverage,
  daemon/server ANN-warm, openrouter, cli/models). `Config.providers` добавлен.
  Ruling: fallback в `embedding_model_id` на legacy-формат при невозможном lookup registry
  недостижим по построению (resolution валидировал до), возврат `String` сохранён, чтобы не
  менять контракты openrouter/coverage; в fallback нет паники (no expect). Ruling: тест
  `coverage_and_ann_warm_use_the_canonical_id` использует fake-embedder с model_id из
  `embedding_model_id` и доказывает 100% coverage под каноническим ключом; ANN-warm
  принимает тот же ключ (равенство строк), полный прогон warm — T4.

- Task 3 review round 1 (2026-10-23, `reviewer-muse`): spec compliance FAIL (2 Critical: from_config
  не обновлён до `remote_on/local_on` и fastembed отвергался generic-сообщением; provider_status без
  вызова → SC-3 hint не enforced. 3 Important: coverage-тест не звал ANN warm; byte-identical-тест
  сравнивал с литералами, а не с legacy-функцией; read_project sniff `[providers` мимо
  `[ providers.* ]`/inline-таблиц. 3 Minor: re-export фасада; silent fallback в embedding_model_id;
  Broken никогда не возвращался; + reranker missing-model e2e).
  Fix round (commit `…`): from_config(cfg, remote_on, local_on) с per-provider gating
  (`feature_missing_remote/local`) и `validate_stage_shapes`; добавлен `src/hybrid/local.rs` (seam-стаб
  T4/T5, `local::embedder/reranker` возвращают Internal до T4/T5); provisioning-гейт
  `ensure_fastembed_provisioned` в `Config::validate_hybrid` (connect hint при отсутствии/runtime
  Broken, fetch hint при нескачанном model — FR-12/SC-3/NFR-6); `provider_status` теперь возвращает
  Broken для записанного, но отсутствующего runtime_path; embedding_model_id fallback логирует warning
  (не молчит); read_project определяет `providers` через `toml::Value`-parse (любые написания);
  byte-identical-тест сравнивает с `legacy_cache_model_id`; coverage-тест теперь реально зовёт
  `DenseCache::warm` + `get` под каноническим ключом; e2e-тесты config: fastembed без provider →
  connect hint, без fetch → fetch hint, reranker без model → Admission; убран дублирующий e2e
  `fastembed_without_dims_uses_registry_dim` (покрыт unit «fastembed_resolution_requires_model_and_validates_dims»;
  e2e требовал бы реального fetch multi-hundred-MB кэша для прохождения provisioning-гейта).
  Ruling: `from_config` не принимает `ProvidersCfg` (brief: `from_config(cfg, caps)`), provisioning
  проверяется на уровне `Config::validate_hybrid` — это точка открытия проекта (NFR-6); stage-конструкция
  fastembed идёт через seam-стаб, который T4/T5 заменяют реальными embedder/reranker (с пробросом
  providers при необходимости). Verify после фикса: lib hybrid::config 8 passed; test config 27 passed;
  default suite green; hybrid-remote check чист; clippy -D warnings чист.

- Task 3 re-review (2026-10-23, `reviewer-muse`): verdict OK WITH NOTES — все 5 prior findings +
  4 minors ADDRESSED; один новый P1 (fastembed-only конфиг на hybrid-remote-сборке требовал
  `OPENROUTER_API_KEY`, т.к. `OpenRouterClient::from_env()` строился безусловно). Fix
  (commit `e01608c`): client строится лениво только при `has_openrouter`; regression-тест
  `fastembed_only_config_does_not_demand_openrouter_key` (hybrid-local,hybrid-remote). Re-review:
  ADDRESSED, new breakage none. T3 closed.

- Task 2: complete (commits `e7a5d1f..`→ head of this session, tests: `cargo test --locked
  --features hybrid-local --lib hybrid::runtime` → 6 passed; `--test connect` → 4 passed;
  `--features hybrid-local,hybrid-remote --test connect` → 4 passed; default suite green;
  clippy -D warnings чист на hybrid-local и hybrid-local,hybrid-remote; default tree gate: 0
  запрещённых крейтов).
  Реализовано: `src/hybrid/runtime.rs` — OQ-1 таблица заполнена реальными релизами ORT 1.24.4
  (5 таргетов, archive_url/archive_sha256/archive_size/library_path/library_size из официальных
  архивов; скачаны и проверены при spec-work), `platform_target`, `resolve` (FR-11:
  ORT_DYLIB_PATH → provider runtime_path → admission; Broken-state), `install` (caps 256 MiB/
  512 MiB, preflight 1.5x+256 MiB, download+sha256+size, extract библиотеки из tgz/zip,
  staging+rename, exclusive `.lock`, `--force`), `init` (OnceLock, init_from+commit, Admission,
  никогда паника); macOS quarantine/codesign-последовательность (`macos_fixup_commands`)
  конструируется и применяется только на macOS (на Linux CI тестируется состав). `src/cli/connect.rs`
  — `connect` status-table (4 ProviderState), `connect fastembed --check/--force`, `connect
  openrouter` (GET /key, 0 токенов, 401/403→Admission check-key, ключ не пишется) под
  hybrid-remote; глобальный write `[providers.*]` сохраняет остальные секции. `Transport` gain
  `get_json` (wire.rs, ReqwestTransport, Mock/Failing/Recording/Scripted имплементиции).
  Ruling: `init` использует `init_from`+`commit()` (ort rc.13 возвращает EnvironmentBuilder; commit
  регистрирует глобальный env). Ruling: валидация размера извлечённой библиотеки по таблице —
  stale-таблица падает на install, а не молчит. Ruling: тесты runtime изолированы `Mutex`-guard'ом
  (общий real runtime_root, параллельные тесты конфликтовали). Minor (deferred): streaming download
  (T1 deferred) касается и runtime-архива (74MB win-x64) — пересмотреть при T8.

- Task 2 review rounds (2026-10-23, `reviewer-muse`): вердикт FAIL → fix `fccf2e0`
  (RuntimeStatus; data-dir fallback; openrouter probe на инжектируемом Transport + тесты
  200/401/transport; --check статус + macOS signature state; lock-ошибки; macOS floor; caps/zip/
  unknown/floor тесты; ENV_LOCK) → re-review: 1 Important NOT ADDRESSED (dead data-dir fallback —
  status-гейт убивал arm) + minor sysctl/weak asserts/guards → fix `5bd9058` (resolve без
  provider_status-гейта, живые data-dir fallback + тест; `kern.osproductversion` + fail-closed;
  runtime_status Connected при data-dir; точные state_labels) → re-review: новый P1 — runtime_status
  маскировал Broken как Connected (ранний return без existence-проверки) → fix `059aad1`
  (existence-гейт + 2 regression-теста) → final re-review: ADDRESSED, new breakage none.
  Minor (deferred): тесты runtime используют реальный `runtime_root()` (только
  DOCSBASE_DATA_DIR-изоляция, без общего guard'а с ENV_LOCK domain'ом); macOS-функции
  (macos_fixup_commands, macos_major_version) неисполнимы на Linux CI по природе (покрыт состав/
  разбор). T2 closed.

- Task 4: complete (commits `89de00a..`→ head, tests: `cargo test --locked --features
  hybrid-local --lib hybrid::local` → 4 passed; `--test hybrid_local` → 3 passed; default suite
  green; clippy -D warnings чист; default tree gate 0).
  Реализовано: `Embedder::embed_query` default = embed([text]) (vector_cache.rs); real
  `FastEmbedEmbedder` (local.rs): provisioning order runtime→model с connect/fetch hints,
  `ModelCacheKey` process-wide map (canonical id + max_length 512 + intra_threads min(4,cores) +
  runtime path), `runtime::init` перед fastembed-конструктором, BYO загрузка
  (`UserDefinedEmbeddingModel` + `TextEmbedding::try_new_from_user_defined` + pooling/quantization/
  output_key из registry), shared `.lock` guard на lifetime инстанса (fd-lock try_read),
  prefix-роли (embed_query=query_prefix, embed=passage_prefix); `dense_candidates` теперь зовёт
  `embed_query` (spy-тест `dense_search_path_uses_embed_query` доказывает роль). Ruling: fallback
  между реальным inference и тестами невозможен offline — prefix-контракт тестируется на registry-
  строках; live-inference в T8. Ruling: `FastEmbedEmbedder` хранит `Mutex<TextEmbedding>` (fastembed
  embed требует &mut self); процесс-wide кэш по ключу гарантирует один инстанс на ключ.
  Отложено: T5 (reranker stub остаётся).

- Task 5: complete (commits `2d0c8c5..`→ head, tests: `cargo test --locked --features
  hybrid-local --lib hybrid::local` → 11 passed (вкл. reranker_without_runtime/without_model/
  rejects_embedding_model_rows/rerank_contract); `--test hybrid_local` → 3 passed; default suite
  green; clippy -D warnings чист).
  Реализовано: `FastEmbedReranker` (local.rs): `reranker(cfg, providers)` с provisioning order
  runtime→model (connect/fetch hints), task-валидация (embedding-модель для rerank → Admission с
  action), `UserDefinedRerankingModel` + `OnnxSource::File` (без полного буфера модели) +
  `RerankInitOptionsUserDefined`; process-wide кэш по ModelCacheKey (`get_or_load_reranker`,
  mirror embedder); shared `.lock` guard на lifetime; `rerank_scores` — чистый mapping
  (index,score) best-first + top_n clamp (unit-покрыт). Ruling: key reranker'а — `fastembed/<name>`
  (без @d/@r/@p, т.к. scoring не кэшируется — YAGNI, трайт Reranker без model_id). Ruling:
  fastembed возвращает все scores отсортированными; clamp у нас, чтобы контракт `top_n` был у
  вызывающего.

- Task 6: complete (commits `570051b..`→ head, tests: `cargo test --locked --test
  cli_models --features hybrid-local` → 15 passed; `--features hybrid-local,hybrid-remote` →
  15 passed; lib `cli::models` unit → 20 (hybrid-local) / 26 (both); default suite green;
  clippy -D warnings чист (default + features); fmt --check).
  Реализовано: purpose-first `models use embed|rerank <model>` (clap subcommands, общие
  `--local/--global/--dry-run/--path` как global args); scope matrix (project = наличие
  `.docsbase.toml`; вне проекта --global обязателен; both flags error; --path+--global error);
  provider inference из registry (task-match, иначе --provider); dims из registry для fastembed
  (--dims optional, mismatch → Admission), --dims обязателен для OpenRouter; window bounds;
  merge только выбранной стадии (другая не трогается, ranking сохраняется); probe gating:
  fastembed offline (runtime::resolve → connect hint; models::cached → fetch hint), OpenRouter
  single-shot через injected Transport (401/403 → check key, transport → retry, wrong shape →
  Internal); global write сохраняет [providers] и прочие секции (atomically, create_dir_all);
  print_summary provider-aware. Ruling: "project" определён как директория с
  `.docsbase.toml` (детерминированно тестируется; bootstrap нового проекта — через --global).
  Ruling: probe не требует реальной сети в тестах — fake Transport на уровне unit (как
  probe_openrouter_key в T2); CLI integration тесты используют --dry-run и error-пути.

  T6 review fix (commit `0306ab4`): re-review ADDRESSED ×3 (hints → new CLI;
  project-write roundtrip test; probe payloads == design strings `docsbase probe` /
  (`docsbase`,`probe`), asserted via body-recording fake transport) + minors (trim,
  provider whitelist upfront, without_env, shared `cli::test_env::ENV_LOCK` across
  connect/models). Tests: lib 27 (both features) / 21 (hybrid-local), cli_models 15/15,
  default green, clippy clean default+features. Ruling: env mutation во всех CLI unit
  тестах сериализуется общим lock'ом (иначе параллельные tests race'ят).

- Task 7: complete (commits `b786557..`→ head, Verify: tree gate default empty для всех 8
  запрещённых crates (fastembed|ort|tokenizers|hf-hub|image|native-tls|ureq|reqwest); `cargo build --release --locked --features hybrid-local` ok (бинарник
  ~17 MiB < 40 MiB бюджета, NFR-8), без missing link-time libs (ORT грузится через dlopen).
  CI: добавлен `linux-local` job (clippy -D warnings + `cargo test --features hybrid-local`,
  полностью offline, без download'ов); в `windows`/`macos` добавлен `cargo check --features
  hybrid-local --all-targets`; default tree gate расширен до списка `fastembed|ort|tokenizers|
  hf-hub|image|native-tls|ureq|reqwest` (NFR-1). Docs: CONFIG.md (providers только в global,
  scope, connect/fetch, models use embed|rerank, канонические id), README quickstart (local
  embeddings ветка), SECURITY.md (ORT как внешний executable artifact + pinned sha256, ключ
  never stored), CHANGELOG (Added: hybrid-local). Ruling: `linux-local` должен стать required
  check — действие человека (записано; merging остаётся человеческим). Ruling: release-static +
  dlopen проверено компиляцией release с features (запуск live нагрузки — T8).

- Task 8: complete (test `tests/hybrid_local_live.rs`, `#[ignore]`; live run PASSED on
  2026-10-24). Numbers: ORT 1.24.4 tgz 8_155_822 B sha256 `3a211fbea252c1e66290658f1b735b772056149f28321e71c308942cdb54b747` (table confirmed), lib so.1.24.4 22_159_232 B. Fetch: jina-reranker-v1-turbo-en fresh 38_295_104 B в ~3.0s; bge cached-check 62ms (совежий fetch не замерен — provisioning, не бюджет). Embed (release, N=3): 10.3ms суммарно, ~3.4ms/text, query 2.3ms (бюджет 250ms/512-token chunk — ok). Rerank 3 docs: 3.4ms (< 200ms/50 docs — ok). Peak RSS 329 MiB (< 700 MiB — ok). Примечание: connect download из GitHub в этой песочнице блокируется TLS-прокси (UnknownIssuer для rustls) — ORT установлен вручную (curl + sha256-проверка совпала с таблицей); сам download-путь connect покрыт offline-тестами T2 на fake source. macOS signature state не применимо (Linux); `init_from` успешен (inference работал). Ruling: live-прогон — ручной/ночной; test не входит в CI.

- Task 9: complete (commits → head, Verify: `cargo test --locked --test cli_models
  --features hybrid-local` → 18 passed; `--test install` → 15 passed (вкл.
  uninstall_dry_run_lists_hybrid_artifacts_without_deleting); lib cli::models 31 (both) /
  25 (hybrid-local); hybrid::local 12 + hybrid_local 3; default suite green; clippy -D warnings
  чист (default/hybrid-local/both); fmt --check).
  Реализовано: `models remove <name>` (FR-7): exclusive non-blocking `.lock` через
  fd-lock try_write; shared guard from T4/T5 (take_shared_lock → pub(crate)) → Admission
  "in use"; `--force` удаляет с warning'ом; единицы-тесты с реальным shared guard'ом
  (remove_refuses_when_locked_then_succeeds_after_drop / remove_force_when_locked /
  remove_unlocked_succeeds / remove_not_fetched_is_admission). `uninstall --dry-run` + `--yes`
  удаляет `<data>/runtimes/onnxruntime` и `<cache>/models` (FR-13) даже без launcher manifest.
  Re-pin: `models use embed` печатает note при смене pin'а (re-embed + ANN invalidated) и при
  первом pin'е. `models list`: provider states (fastembed/openrouter), selected pins (canonical
  id), per-model cache size. Fetch/connect preflight уже существовал (1.5x+256MiB, fail
  before download с проверенными числами). Ruling: fetch не молча меняет revision — revision
  build-static в registry; смена ревизии = код-изменение registry (перезапрос явный). Ruling:
  uninstall без manifest'а всё равно чистит hybrid-local artifacts (они owned независимо от
  launcher).

  T9 review fix (commit → head): Important — remove_cmd теперь удаляет ВСЕ cached revision'ы
  модели (`<cache>/models/<name>` целиком), gate на model_root (а не на pinned revision dir);
  тест remove_deletes_all_cached_revisions_including_stale. Minors — repin_embedding_note
  возвращает Option<String> (pure) + unit-тесты (change/first/silent); no-manifest uninstall
  теперь берёт admission lease (acquire_after_stop) перед удалением runtime/models
  (remove_hybrid_artifacts helper, без дублирования); tests/config.rs + hybrid_local.rs
  изолируют DOCSBASE_CACHE_DIR (реальный кэш после T8 ломал fetch-hint тесты). Verify: default
  suite green; both-features suite green; clippy -D warnings чист default/hybrid-local/both;
  fmt --check; tree gate pass.

  T9 residual fix (commit `38fbf8b`): re-review ADDRESSED — stale-only model root
  (pinned revision absent) теперь удаляется без lock acquisition (невозможно держать lock на
  отсутствующей revision dir; `Box::leak`-паттерн как в take_shared_lock); тест
  remove_stale_only_revisions_succeeds. Verify: cli::models 28 (hybrid-local) / 33 (both);
  cli_models 18; config 27; hybrid_local 3; install 15; default suite green; clippy чист
  default/hybrid-local/both; fmt --check.

- Final convergence review (P0 fix): `from_config` теперь принимает `&ProvidersCfg` и передаёт
  его обоим fastembed arms (раньше `ProvidersCfg::default()` — connect-provisioned runtime был
  недостижим во всех 4 продуктовых путях: cli/index, cli/search, daemon/server, hybrid/jobs;
  P4/SC-6/FR-11). Threading: jobs::embed_texts/embed_project_chunks получают providers;
  callers (index/job.rs, daemon/tools.rs, tests) обновлены. Regression-тест
  `from_config_fastembed_uses_real_provider_config` (runtime через runtime_path, cache isolated
  → fetch hint, НЕ "not connected"). P2s: CHANGELOG += `models remove`/uninstall --dry-run;
  README.ru зеркалит EN quickstart (connect/fetch/use); embedding_model_id fallback больше не
  шумит в stderr (doc: unreachable через validated config, FR-9). Revisit open: streaming
  fetch (bge-reranker-base 1.11 GB buffered, unmeasured); runtime::init OnceLock first-load
  poison (принято, T2 ruling). Verify: default suite green; both-features suite green; clippy
  чист default/hybrid-local/both; fmt --check; tree gate pass.

- Converged: final review re-check ADDRESSED (P0 from_config threading verified on all 4
  production paths + jobs helpers; P2s done; no new breakage). Вердикт: READY FOR HUMAN
  MERGE. Остаётся человеческими действиями: merge PR (main protected), добавить `linux-local`
  как required GitHub check, live-cadence OQ-3, при желании — ревизия streaming-fetch на
  bge-reranker-base (1.11 GB). Общий diff: 47 файлов, +10.5k/−0.6k, 43 коммита.

- Review panel (3-model + verifier, target ec9452e..48831b5): verdict BLOCK. Panel: mimo BLOCK (1 P0), muse OK WITH NOTES (2 P1), deepseek OK WITH NOTES (4 P1). Aggregated → verifier: 11 VERIFIED, 2 PARTIAL. Blocking: P0 runtime tests env/root isolation (intermittent red; deletes real data-dir runtime when env unset); verified P1s: models remove probes only pinned-revision lock while deleting model root; provisioning gate in Config::validate breaks connect/models list/unrelated projects; gate ignores ORT_DYLIB_PATH; AC3 product-path wiring has no integration guard; failed fetch/--force wipes verified cache (cleanup_dir). PARTIAL: C6 conflicting --provider (P1→P2, contract ambiguity), C11 duplicate concurrent loads (P2, leak nuance corrected). Parent-verified P2s: connect --check internal error on Broken path + help mismatch; double full-hash per load; per-text embed (batch unused); dry-run preview omits non-hybrid keys; global config writers RMW without inter-process lock; stale docs (provider allowlist comments, install.rs doc duplicate, T6 note). Fixes pending (human/implementer).

- Fix C1 (P0 runtime tests env/root isolation): runtime tests now serialize all env mutation under `runtime_test_guard()` (poison-tolerant) and sandbox `DOCSBASE_DATA_DIR` via RAII `TestDataDir` (restores on drop); `resolve_requires_a_connected_provider_or_env_override` and `runtime_status_reports_connected_when_data_dir_install_exists` use it instead of manual env dances. Runtime root touches can never hit the real data directory. Full lib suite green (174 passed); runtime group green 3/3.
- Fix C3+C4 (P1 provisioning gate): `ensure_fastembed_provisioned` removed from `Config::validate_hybrid` — config load never refuses on missing runtime/model, so `connect`/`models list`/`config` and unrelated projects load even with a stale pin. Provisioning surfaces with identical single-action hints (connect vs fetch) at stage construction (`hybrid::local::embedder`/`reranker` already resolve runtime first via `runtime::resolve` — honoring ORT_DYLIB_PATH — then check `models::cached`). Tests updated: fastembed_without_provider_fails_with_connect_hint / fastembed_unfetched_model_fails_with_fetch_hint now assert load-ok + construction admission; new fastembed_env_runtime_override_reaches_model_check (C4 discriminator: fetch hint, never "is not ready"). Reproduced: models list exit 0 with dangling runtime_path; config exit 0 with env runtime, no provider record. lib/config suites green; clippy clean.
- Fix C5 (P1 AC3 wiring regression guard): new integration test `index_admits_unfetched_fastembed_with_real_provider_record` (tests/cli_models.rs) drives the real `docsbase index` product entry point with a project fastembed pin + global `[providers.fastembed]` record with dangling runtime_path; asserts the resolve-stage admission names the runtime + `--force` and never "is not connected" — i.e. the call site threads the real `ProvidersCfg` (a `default()` regression would flip the message). cli_models suite green; clippy clean.
- Fix C5 extended (muse residual: single-path guard): added sibling guards — `search_admits_unfetched_fastembed_with_real_provider_record` (E2E: register project via index w/ lexical config, then pin fastembed + dangling global provider; search fails with --force hint, never "is not connected") and `jobs_embed_texts_threads_real_providers` (unit-level `jobs::embed_texts` with real ProvidersCfg, cache-isolated). Remaining unwired: daemon/server.rs (stage construction inside server startup) — covered by the same from_config contract; noted as deferred guard. cli_models (20) + hybrid_config suites green; clippy clean.
- Fix C6 (P1 fetch wipes verified cache): `fetch_into` now installs into a unique sibling `.staging-<rev>-<seq>` dir (per-call serial tag; parallel tests/fetches can't collide) and promotes it by rename only after every file verified; the previous cache is stashed to `.old-<seq>` so a crash between the two renames restores it. A failed fetch (incl. failed --force refetch) leaves the pre-existing verified cache intact and drops only staging. Failure-path tests now allow only the `.lock` control file; new `force_refetch_failure_preserves_verified_cache` regression. `cleanup_dir` removed (dead). lib (177), hybrid_models (4), cli_models suites green; clippy clean.
- Fix C7 (P2 conflicting --provider): `resolve_provider` now errors with an Admission when the model is a registry row and `--provider` explicitly conflicts (fastembed implied); matching `--provider fastembed` still allowed (dry-run test at tests/cli_models.rs:107-127). New test models_use_conflicting_provider_for_registry_model_errors. cli_models (21) + lib cli::models (30) green; clippy clean.
- Fix C8 (P2 connect --check): help no longer promises install; a vanished runtime path is now reported as a Broken-state line (exit 0, "is missing") instead of an internal stat error. New test connect_fastembed_check_with_vanished_runtime_path_is_broken_not_internal. connect suite green; clippy clean.
- Fix C9 (P2 double full-hash): `verify_hashes` wrapped with a process-wide memo keyed by file signature (per-file size+mtime, .complete mtime) — repeated cached checks per open do one hash pass; same-size tampering still detected via signature invalidation. Test-only hash-pass counter + `cached_memoizes_verified_hashes_within_process` regression. lib (177) + models (48) green; clippy clean.
- Fix C11 (P2 duplicate concurrent loads): `get_or_load_by_key` now serializes construction under `LOAD_LOCK` (cold path), re-checking the map after acquiring so waiters reuse the winner's instance; no lock-order cycle with instance maps. New test `concurrent_same_key_cold_loads_construct_once` (8 threads, barrier, exactly one loader call, all share instance). local (15) + lib green; clippy clean.
- Fix C13 (P2 global config RMW): new `config::paths::with_config_write_lock` (fd-lock `<path>.lock`, bounded retry 50x20ms, error on exhaustion); `connect::write_global_provider` and `models::write_global_toml` (the `--global` writer) now run their read-modify-write bodies under it — two concurrent processes cannot drop each other's sections. Unit test config_write_lock_serializes_and_releases. lib (181), connect (5), cli_models (22) green; clippy clean.
- Fix stale docs (panel parent-verified P2): EmbeddingCfg/RerankerCfg "only openrouter accepted" comments updated to both providers; install.rs duplicated doc block above remove_hybrid_artifacts removed; models list stale "selected-model column arrives in T6" note removed. C13 residual recorded: deterministic two-process concurrent RMW preservation test deferred (minor; lock semantics unit-covered).
- Fix R2-P1a (models use RMW outside lock): `apply_stage_locked` extracted — read-merge-write re-runs inside `with_config_write_lock` for BOTH project and global scopes (probe stays outside; dry-run preview advisory). `write_global_toml` is now test-only (#[cfg(test)]). Regression `concurrent_global_writers_both_survive` (slow writer A holds lock, B starts after A's snapshot; both stages survive). lib (182) + cli_models green; clippy clean.
- Fix R2-P1b (fetch install lock hang): `acquire_exclusive_lock` — bounded try-retry (50×20ms, fresh leaked 'static lock per attempt, remove_cmd/take_shared_lock pattern) then Admission "is loaded/in use; stop the daemon (or retry)". Never blocks silently on a process-lifetime shared holder. New test fetch_force_refuses_boundedly_when_shared_lock_held (refused <2s). models (50) + lib (183) green; clippy clean.
- Fix R2-D2/D3 (P2 lock identity + in-flight fetch): lock moved to the MODEL ROOT (`<model_root>/.lock`, `open_lock`/`take_shared_lock`/`remove_cmd` all use it) — the lock file is never renamed/unlinked by a promote, so a loaded instance's shared guard keeps blocking the remove probe across refetches (D2), and an in-flight first fetch (`.staging-*` phase) is protected from remove (D3). remove_cmd simplified to one root-lock probe. Unit tests moved to realistic nested cache layout. New tests: loaded_instance_blocked_remove_probe_across_refetch, remove_refuses_when_fetch_inflight_on_staging. models (52), local (15), lib (185), cli_models green; clippy clean.
- Fix R2-D4 (P2 LOAD_LOCK poison): `get_or_load_by_key` now recovers a poisoned construction lock (`PoisonError::into_inner` — guards no data), so a panicking loader can't permanently break later stage construction. New test poisoned_load_lock_recovers_for_later_keys. local (16) + lib green; clippy clean.
- Fix R2-U2 (P2 runtime install no backup): `promote_runtime` extracted — stashes the previous runtime to `.old-<triple>`, promotes staging, restores on publish failure, removes backup on success (mirrors the model-cache C6 pattern). Unit test promote_runtime_restores_previous_on_publish_failure (restore + success paths). runtime (18) + lib (187) green; clippy clean.
- Fix R2-M3 (P2 env test race): `tests/hybrid_config.rs` now has one module-level `ENV_LOCK` shared by `jobs_embed_texts_threads_real_providers` and `from_config_fastembed_uses_real_provider_config` (was function-local, leaving the jobs test unserialized). R2-M4 (P2 CI): linux-local job now also runs `cargo test --locked --features hybrid-local,hybrid-remote` + clippy combo (760 tests pass locally). R2-M5 (P2 TOCTOU): take_shared_lock NotFound → fetch-hint Admission (test take_shared_lock_on_vanished_root_is_fetch_hint). R2-U4 (P2): --check --force mutually exclusive (test added). hybrid_config suites green both features; clippy clean; combo clippy clean.
- Fix R3-P0 (P0 default clippy red): `ENV_LOCK` static + `use std::sync::Mutex` in tests/hybrid_config.rs gated `#[cfg(feature = "hybrid-local")]` (both users are local-only; the round-2 M3 hoist broke default/remote clippy). All four clippy matrices now green: default, hybrid-remote, hybrid-local, combo. Default & hybrid-local suites green.
- Fix R3 docs/API P2s: print_status "four providers" comment corrected; README.md "Modern"→"Default" + restored training/rotation disclosure (matches RU/CONFIG); `take_shared_lock` returns the plain guard (dead Option variant removed from embedder/reranker `_lock_guard` and call sites); contention wording made neutral ("busy (fetch in progress or removal)"). lib (188) + local (17) + cli::models (32) green; combo clippy clean.
- Fix R3 deepseek-5 (P2 preserved-stage validation): validate_hybrid_table split into validate_embedding_stage + validate_reranker_stage — preserved stages now fail at `models use` on unknown provider or fastembed normalize=false. New test models_use_preserved_stage_provider_and_normalize_are_validated. R3 deepseek-4 (P2 staging inside root): staging/backup moved to siblings under `<cache>/models` (`.staging-<name>-<tag>`) — immune to a concurrent remove of the root subtree; promote into a vanished root fails cleanly. R3 deepseek-7 (P2 list rehash): `cached_shallow` (`.complete`+sizes, no sha256) used by models list; full hash stays on load/trust paths. lib (188), models (52), cli_models (23), default suite green; combo + default clippy clean.
- Fix R3 deepseek-7 wiring (muse residual): list_cmd now actually calls `cached_shallow` (verify pass caught zero callers after a checkout reverted the edit); `force_refetch_failure_preserves_verified_cache` leftover scan moved to `<cache>/models` (sibling layout); open_lock doc updated for sibling staging. All suites + both clippy matrices green.
- Fix R3 muse-2 (P2 init OnceLock failure stickiness): `runtime::init` memoizes only SUCCESS (OnceLock<()> set after commit); a transient first-load failure is re-attempted on every call, no daemon restart needed. R3 muse-3 (P2 fetch restart note): `models fetch --force` prints "restart a running detached daemon to pick up the new revision". R3 deepseek-2 (P2 OpenRouter embed_query): OpenRouterEmbedder now overrides `embed_query` with `input_type: "search_query"` (INPUT_TYPE_QUERY) via `embed_texts_with_type`; stale "iteration 2" comment removed; new test embed_query_uses_search_query_input_type (fake transport asserts payload). combo (762), hybrid_openrouter (20) green; combo + default clippy clean.
- Fix R3 mimo-4/muse-1 (P2 streaming fetch): ModelSource gains `fetch_to` (streaming; default materializes via fetch); HttpModelSource streams 64KiB chunks with in-flight per-file cap; `fetch_one` now downloads straight into `*.part` through HashCountSink (running sha256 + byte counter), verifies size/sha, fsync, rename — no whole-file RAM buffer (bge-reranker-base 1.1 GiB peak eliminated). Also R3 mimo-3 (P2 lossy suffixes): `.part` built via OsString::push (non-UTF-8 cache roots). R3 muse-3 residual: restart note now prints whenever the revision changed (not only --force). All suites + both clippy matrices green.
- Fix R3 mimo-3 residual (muse): write_complete `.tmp` and with_config_write_lock `.lock` now also use OsString::push (non-UTF-8 paths). All suites + both clippy matrices green.
- Review panel round-4 (target ec9452e..88d2067): mimo OK WITH NOTES (1 P2), muse OK WITH NOTES (4 P2), deepseek aborted by user (no report). Verifier (review-verifier) on 5 aggregated claims: C1 VERIFIED (stray .docsbase.toml.lock in project root), C2 VERIFIED (exclusive-lock retry leaks fd+Box per attempt, up to 50), C3 REFUTED (concurrent-fetch staging collision impossible — root exclusive lock serializes fetch), C4 VERIFIED (load path trusts size+mtime memo; no hash of loaded bytes), C5 PARTIAL→P3 (restart-note omission possible but stale-daemon impact blocked by shared lock). Surviving open items: 3 P2 (C1, C2, C4) + 1 P3 (C5). Fixes pending human/implementer decision.
- Fix round-4 C1 (P2, VERIFIED): config-write locks relocated from `<target>.lock` sibling to `<data>/locks/<siphash>.lock` (with_config_write_lock_in + config_lock_path; never unlinked → no inode race). Project-scoped `docsbase models use` no longer leaves a stray .docsbase.toml.lock in the user's repo. Unit test updated + asserts no sibling lock. All suites + clippy green.
- Fix round-4 C1 gap (deepseek P2): added `config_write_lock_wrapper_routes_locks_to_data_dir` — re-exec child test sandboxing DOCSBASE_DATA_DIR exercises the real `with_config_write_lock` wrapper (no env race), asserts no sibling lock next to project target and exactly one lock under <data>/locks.
- Fix round-4 C2 (P2, VERIFIED): `acquire_exclusive_lock` now opens/leaks ONE lock per call and retries `try_write` on it (was: fresh fd+Box per attempt, up to 50 leaked on contention). All suites + clippy green.
- Fix round-4 C2 (P2, VERIFIED): `acquire_exclusive_lock` now opens/leaks ONE lock per call and retries `try_write` on it (was: fresh fd+Box per attempt, up to 50 leaked on contention). Return type relaxed to anonymous lifetime ('static was forcing a perpetual borrow region → E0499). All suites + clippy green.
- Fix round-4 C2 (P2, VERIFIED): exclusive install lock now opened/leaked exactly once per fetch — `acquire_exclusive_lock` replaced by `with_exclusive_lock(dir, body)` (guard stays inside the success arm of the retry loop; safe code only — a returned 'static guard cannot be reborrowed across iterations, E0499). `fetch_into`'s install body moved into `install_staged`. All suites + clippy green.
