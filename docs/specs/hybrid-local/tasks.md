# hybrid-local Implementation Plan

**Goal:** add an offline local `fastembed` provider (embedding + rerank) behind feature
`hybrid-local`, with explicit `connect` (ORT) and `models fetch` provisioning and a
machine-local provider registry.
**Architecture:** provisioning (`cli/connect`, `cli/models`, `hybrid/runtime`,
`hybrid/models`) is separate from the runtime path (`hybrid/local` implements `Embedder`/
`Reranker`); the default build links none of it.
**Spec:** `docs/specs/hybrid-local/requirements.md`
**Global constraints:**
- Rust edition 2024, MSRV 1.88; fmt + clippy `--all-targets -D warnings`; no unwrap/expect/panic in `src/**`.
- Default `cargo tree` has no `fastembed`, `ort`, `tokenizers`, `hf-hub`, `ureq`, `reqwest` (NFR-1).
- New crates only per ADR-17, all optional behind `hybrid-local`.
- No weights/runtime in the crate or binary; network only in `connect`/`fetch`.
- `[providers]` only in the global config; `model_id` includes the pinned revision.
**Review focus:**
1. Silent network on search/index (must fail with an admission hint) — T3/T4.
2. Partial model/runtime install used after a crash — T1/T2 (`*.part` + `.complete` + rename).
3. Scope mistakes in `models use` (writing global when project was meant) — T6.
4. `[providers]` leaking into a committed project config — T3.
5. Default build growth (feature leakage/deps) — T7 tree gate.

---

## T1 — Model registry, cache layout, and fetch

**Goal:** a closed model registry and an explicit, verifiable, idempotent model fetch.
**Files:**
- Create: `src/hybrid/models.rs`
- Modify: `Cargo.toml` (feature `hybrid-local`, optional `ureq`, `sha2`, `fd-lock` reuse)
- Test: `src/hybrid/models.rs::tests`, `tests/hybrid_models.rs`
**Interfaces:**
- Produces: `ModelEntry`, `FileSpec`, `Task`, `Pooling`, `ModelSource`, `entry(&str)`,
  `fetch(&ModelEntry, &dyn ModelSource) -> Result<PathBuf>`, `cached(&ModelEntry) -> bool`,
  `cache_dir(name, revision) -> PathBuf`, `model_id(entry, dim, normalize) -> String`.
- Consumes: `Error`, `crate::paths` (cache dir), `fd-lock`.
**Steps:**
1. RED: `registry_entries_are_well_formed` — unique names, dim > 0, `onnx` + tokenizer
   files listed with size/sha256, embedding entries have pooling, licenses non-empty.
2. RED: `fetch_installs_all_files_and_completes_last` with a fake `ModelSource` — every
   file written, `.complete` exists, second call downloads zero files.
3. RED: `fetch_rejects_a_bad_hash` — fake source returns wrong bytes; assert error and no
   directory usable (`cached == false`, no `.complete`).
4. RED: `model_id_includes_revision` — `fastembed/bge-small-en-v1.5@d384@l2@r<rev8>`.
5. Run: `cargo test --locked --features hybrid-local --lib hybrid::models` (fails).
6. GREEN: implement registry type, cache layout, atomic write (`*.part` → fsync → rename),
   `.complete` last, lock file, hash/size checks, `list_state`.
7. Verify: same command passes; `cargo tree --locked` default unchanged.
**Acceptance:** FR-5, FR-6, FR-7, NFR-3, NFR-5; SC-4.
**Verify:** `cargo test --locked --features hybrid-local --lib hybrid::models` → ok.
**Depends:** —

## T2 — ONNX Runtime installer and `connect`

**Goal:** `docsbase connect` installs/checks the pinned runtime and registers providers.
**Files:**
- Create: `src/hybrid/runtime.rs`, `src/cli/connect.rs`
- Modify: `src/cli/mod.rs` (subcommand), `Cargo.toml` (`ureq`, `sha2`, `flate2`, `tar`, `zip`)
- Test: `src/hybrid/runtime.rs::tests`, `tests/connect.rs`
**Interfaces:**
- Produces: `Target`, `RuntimeStatus`, `platform_target()`, `resolve(&ProvidersCfg)`,
  `install(Target, &dyn RuntimeSource, force)`, `init(path)`, `connect::run(args)`.
- Consumes: `Config::global*`, `providers` config, `models::fetch`.
**Steps:**
1. RED: `targets_cover_six_platforms` — exactly linux-x64/aarch64, osx-arm64/x64,
   win-x64/arm64; unknown target → admission.
2. RED: `install_verifies_hash_and_is_atomic` — fake source with bad sha leaves nothing;
   good source installs under `<data>/runtimes/onnxruntime/<ver>/<target>/`.
3. RED: `connect_check_reports_missing_without_network` — `--check` never calls the source.
4. RED: `connect_openrouter_reads_env_and_never_persists_the_key` — fake probe succeeds,
   global config gets `[providers.openrouter]`, key absent from file.
5. Run: `cargo test --locked --features hybrid-local --lib hybrid::runtime` (fails).
6. GREEN: implement download/extract (tar.gz; zip on Windows), sha256, atomic install,
   global config write, status table; `--force`/`--models`; `--local` rejected.
7. Verify: `cargo test --locked --features hybrid-local --lib hybrid::runtime --test connect`.
**Acceptance:** FR-2, FR-3, FR-4, FR-10, NFR-3, SC-2, SC-3.
**Verify:** `cargo test --locked --features hybrid-local --lib hybrid::runtime --test connect` → ok.
**Depends:** T1 (for `--models`).

## T3 — Provider registry in config and validation

**Goal:** parse `[providers.*]` (global only), validate provider/model/dims before use.
**Files:**
- Modify: `src/config/mod.rs` (parse/validate), `src/hybrid/config.rs` (`from_config`),
  `src/hybrid/mod.rs` (feature gating)
- Test: `tests/config.rs`, `src/hybrid/config.rs::tests`
**Interfaces:**
- Produces: `ProvidersCfg`, `providers::status(&Config)`, updated `from_config(cfg, caps)`
  where `caps` carries `remote_on`/`local_on` instead of a single bool.
- Consumes: registry (`models::entry`), feature flags.
**Steps:**
1. RED: `providers_in_project_config_are_rejected`.
2. RED: `fastembed_without_registry_entry_is_an_admission_error` (with hint text).
3. RED: `fastembed_requires_model_and_matching_dimensions_and_l2`.
4. RED: `global_defaults_are_overridden_by_project` (provider+model).
5. GREEN: add config structs, precedence, validation; keep OpenRouter tests green by
   migrating them to `[providers.openrouter]`.
6. Verify: `cargo test --locked --test config --lib hybrid::config`.
**Acceptance:** FR-1, FR-9, FR-12, NFR-6, SC-3, SC-5.
**Verify:** `cargo test --locked --test config --lib hybrid::config` → ok.
**Depends:** T1 (registry lookup).

## T4 — `FastEmbedEmbedder`

**Goal:** local embedding from fetched files, no network, one model per process.
**Files:**
- Create: `src/hybrid/local.rs`
- Modify: `src/hybrid/mod.rs` (`#[cfg(feature = "hybrid-local")] pub mod local;`)
- Test: `src/hybrid/local.rs::tests`, `tests/hybrid_local.rs`
**Interfaces:**
- Produces: `local::embedder(&EmbeddingCfg, &ProvidersCfg) -> Result<Arc<dyn Embedder>>`.
- Consumes: `runtime::resolve/init`, `models::cached`, `fastembed` user-defined API.
**Steps:**
1. RED: `embedder_without_runtime_fails_with_connect_hint` (no ORT needed).
2. RED: `embedder_without_model_fails_with_fetch_hint`.
3. RED: `embedder_model_id_matches_registry` (construct with a stub model dir and a fake
   runtime path? if ORT absent, assert the error precedes construction).
4. GREEN: build `UserDefinedEmbeddingModel` (onnx bytes, tokenizer files) with pooling and
   quantization; `TextEmbedding::try_new_from_user_defined`; `OnceLock` per key; batch/thread
   options; `blocking::enter` at call sites.
5. Verify: `cargo test --locked --features hybrid-local --lib hybrid::local`.
**Acceptance:** FR-9, NFR-4, NFR-7, SC-6.
**Verify:** `cargo test --locked --features hybrid-local --lib hybrid::local` → ok.
**Depends:** T1, T3.

## T5 — `FastEmbedReranker`

**Goal:** local cross-encoder rerank on the same provisioning path.
**Files:**
- Modify: `src/hybrid/local.rs`
- Test: `src/hybrid/local.rs::tests`
**Interfaces:**
- Produces: `local::reranker(&RerankerCfg, &ProvidersCfg) -> Result<Arc<dyn Reranker>>`.
- Consumes: `runtime`, `models`, `fastembed::TextRerank`.
**Steps:**
1. RED: `reranker_without_model_fails_with_fetch_hint`.
2. RED: `rerank_contract_returns_index_and_score_for_every_doc` (with a stub model, if ORT
   is unavailable assert the contract test is `#[ignore]` and add a unit test of the
   mapping/`top_n` logic).
3. GREEN: `UserDefinedRerankingModel` + `RerankInitOptionsUserDefined`; reuse the model cache;
   `top_n` clamp; errors → `Internal`.
4. Verify: `cargo test --locked --features hybrid-local --lib hybrid::local`.
**Acceptance:** FR-7, NFR-7, SC-6.
**Verify:** same command → ok.
**Depends:** T4 (shared runtime init/cache).

## T6 — `models use` scope and provider-aware validation

**Goal:** select models per project or as global defaults; never write `[providers]`.
**Files:**
- Modify: `src/cli/models.rs` (scope flags, provider-aware validation, global write)
- Test: `tests/cli_models.rs`
**Interfaces:**
- Produces: `models use --provider … --local|--global`; `resolve_scope(root, flags)`.
- Consumes: `Config::global`, project root discovery, registry.
**Steps:**
1. RED: `inside_project_writes_local_by_default`; `--local` accepted; `--path` honoured.
2. RED: `outside_project_requires_global`; `both_scope_flags_are_rejected`.
3. RED: `fastembed_model_without_fetch_fails_with_hint`; dims inferred from registry;
   wrong `--dims` rejected.
4. RED: `global_write_preserves_providers_and_other_sections`.
5. GREEN: implement scope resolution, validation, atomic write; keep the comment-loss note.
6. Verify: `cargo test --locked --test cli_models --lib cli::models`.
**Acceptance:** FR-8, SC-5.
**Verify:** `cargo test --locked --test cli_models` → ok.
**Depends:** T1, T3.

## T7 — Feature wiring, CI gates, docs

**Goal:** the feature builds on 3 OS, the default build stays offline, docs complete.
**Files:**
- Modify: `Cargo.toml`, `.github/workflows/ci.yml`, `docs/CONFIG.md`, `README.md`,
  `SECURITY.md`, `CHANGELOG.md`
**Steps:**
1. Add `hybrid-local` feature with all optional deps.
2. CI: `linux-local` job (clippy+test with the feature, no network for tests); default
   `linux` job adds the `cargo tree` gate for the forbidden crates.
3. Docs: CONFIG (`[providers]`, scope, connect/fetch), README quickstart, SECURITY
   (runtime as an external artifact), CHANGELOG.
4. Verify: `cargo tree --locked | grep -E 'fastembed|ort|tokenizers|hf-hub|ureq|reqwest'`
   is empty; `cargo check --features hybrid-local` on the three OS jobs.
**Acceptance:** NFR-1, SC-1, SC-8.
**Verify:** tree gate empty; CI jobs green.
**Depends:** T1–T6.

## T8 — Live verification and ledger

**Goal:** prove the real path once, record numbers.
**Files:**
- Create: `tests/hybrid_local_live.rs` (`#[ignore]`)
- Modify: `docs/specs/hybrid-local/progress.md`
**Steps:**
1. Ignored test: `connect --check` (or `ORT_DYLIB_PATH`) → `models fetch` → embed 3 texts →
   rerank 3 docs; assert dimensions/score ordering.
2. Run manually: `cargo test --release --features hybrid-local --test hybrid_local_live -- --ignored --nocapture`.
3. Ledger: ORT version/sha, model sizes, fetch time, embed/rerank latency, RSS.
**Acceptance:** SC-7.
**Verify:** the command above passes and the ledger has the numbers.
**Depends:** T2–T5.

---

## Task-list self-review

- **Spec coverage:** FR-1→T3; FR-2/3/4/10→T2; FR-5/6/7→T1; FR-8→T6; FR-9→T3/T4;
  FR-11→T2; FR-12→T3/T4/T5. NFR-1→T7; NFR-2→all; NFR-3→T1/T2; NFR-4/7→T4/T5;
  NFR-5/6→T1/T2/T3; NFR-8→T7. SC-1→T7; SC-2/3→T2/T3; SC-4→T1; SC-5→T6; SC-6→T4/T5;
  SC-7→T8; SC-8→T7. No uncovered requirement.
- **Type consistency:** `ModelEntry`/`FileSpec`/`ModelSource` (T1) are consumed by T2/T3/T4;
  `ProvidersCfg` (T3) is consumed by T2/T4/T5; `Target`/`RuntimeSource` (T2) by T8.
- **Review focus:** network-on-search → T3/T4 tests; partial install → T1/T2; scope →
  T6; providers-in-project → T3; default-build growth → T7.
- **Proportion:** 8 tasks for ~6 modules; no task exceeds one session.
