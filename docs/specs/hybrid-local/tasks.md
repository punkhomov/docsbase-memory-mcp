# hybrid-local Implementation Plan

**Goal:** add an offline local `fastembed` provider (embedding + rerank) behind feature
`hybrid-local`, with explicit `connect` (ORT) and `models fetch` provisioning, a
machine-local provider registry, and no default models.
**Architecture:** provisioning (`cli/connect`, `cli/models`, `hybrid/runtime`,
`hybrid/models`) is separate from the runtime path (`hybrid/local` implements `Embedder`/
`Reranker` with role-aware prefixes); the default build links none of it.
**Spec:** `docs/specs/hybrid-local/requirements.md` (rev 0.4)
**Global constraints:**
- Rust edition 2024, MSRV 1.88; fmt + clippy `--all-targets -D warnings`; no unwrap/expect/panic in `src/**`.
- Default `cargo tree` has no `fastembed`, `ort`, `tokenizers`, `hf-hub`, `image`, `native-tls`, `ureq`, `reqwest` (NFR-1).
- Exact pins: `fastembed =7.1.1` (default-features=false), `ort =2.0.0-rc.13` (load-dynamic), `ureq =3.4.2`, `sha2 =0.10.9`, `zip =9.0.0`, `flate2`, `tar`.
- No weights/runtime in the binary; ORT 1.24.4; five targets (no macOS x64, macOS >= 14).
- Network only in `connect`/`fetch`; `[providers]` only in the global config; canonical `model_id` includes revision + prompt version.
**Review focus:**
1. Silent network on search/index → T3/T4 (admission, no fallback).
2. Partial install used after a crash → T1/T2 (`*.part` + `.complete` + rename + lock).
3. Missing query/passage prefixes silently degrading retrieval → T4 (FR-10).
4. Write-A/read-B cache key (coverage/ANN vs embedder) → T1/T3 (single canonical function).
5. Provider scope leaks (project `[providers]`, mixed providers, local-only `models use`) → T3/T6.

---

## T1 — Model registry, cache, fetch, canonical id

**Goal:** a closed registry and an explicit, verifiable, idempotent fetch.
**Files:**
- Create: `src/hybrid/models.rs`
- Modify: `Cargo.toml` (feature `hybrid-local`, optional deps with exact pins)
- Test: `src/hybrid/models.rs::tests`, `tests/hybrid_models.rs`
**Interfaces:**
- Produces: `ModelEntry`, `FileSpec`, `Task`, `Pooling`, `ModelSource`, `entry`, `fetch`,
  `cached`, `cache_dir`, `embedding_model_id(cfg)`.
- Consumes: `Error`, `paths`, `fd-lock`, `sha2`.
**Steps:**
1. RED: registry well-formedness — unique names, dim > 0, `onnx` + 4 tokenizer files with
   size/sha256, pooling, licenses, E5 `query: `/`passage: ` prefixes, BGE instruction,
   `prompt_version`.
2. RED: `fetch_installs_all_files_and_completes_last` (fake source) — `.complete` last,
   second call downloads nothing.
3. RED: `fetch_rejects_bad_hash_and_size` — nothing usable remains.
4. RED: `cached_reverifies_hashes` — tampered file after install → not cached.
5. RED: `model_id_includes_revision_and_prompt`.
6. Run: `cargo test --locked --features hybrid-local --lib hybrid::models` (fails).
7. GREEN: implement; manifest: `fastembed` default-features=false, direct `ort` load-dynamic.
8. Verify: same command; `cargo tree --locked` default unchanged.
**Acceptance:** FR-5, FR-6, FR-9, NFR-3, NFR-5; SC-4, SC-10.
**Verify:** `cargo test --locked --features hybrid-local --lib hybrid::models` → ok.
**Depends:** —

## T2 — ONNX Runtime installer and `connect`

**Goal:** install/check ORT 1.24.4 and register providers; never select a model.
**Files:**
- Create: `src/hybrid/runtime.rs`, `src/cli/connect.rs`
- Modify: `src/cli/mod.rs`, `Cargo.toml` (`ureq`, `sha2`, `flate2`, `tar`, `zip`)
- Test: `src/hybrid/runtime.rs::tests`, `tests/connect.rs`
**Interfaces:**
- Produces: `Target`, `RuntimeStatus`, `platform_target()`, `resolve`, `install`, `init`,
  `connect::run(args)`.
- Consumes: global config, `providers`, `models::fetch` (not via connect).
**Steps:**
1. RED: `targets_cover_five_platforms` (linux x64/aarch64, osx-arm64, win-x64/arm64;
   unknown → admission; macOS floor 14). ORT version is `1.24.4`.
2. RED: `install_verifies_hash_and_is_atomic`; bad hash leaves nothing; `--force` replaces.
3. RED: `resolve_requires_a_connected_provider_or_env_override` — data-dir runtime without
   `[providers.fastembed]` is not used; `ORT_DYLIB_PATH` wins; `init_from` is first.
4. RED: `connect_openrouter_reads_env_probes_and_never_persists_the_key`; offline probe →
   admission.
5. GREEN: download (size cap), extract, atomic install, global config write, status table;
   `--check`, `--force`; `--local` rejected; no `--models` flag.
6. Verify: `cargo test --locked --features hybrid-local --lib hybrid::runtime --test connect`.
**Acceptance:** FR-2, FR-3, FR-4, FR-11, NFR-3; SC-2, SC-3.
**Verify:** same command → ok.
**Depends:** —

## T3 — Config: providers, project type, canonical id

**Goal:** parse `[providers.*]` (global only) and validate stages before use.
**Files:**
- Modify: `src/config/mod.rs` (`GlobalFileConfig`, `ProjectFileConfig`), `src/hybrid/config.rs`,
  `src/hybrid/mod.rs`, callers of `cache_model_id` (`src/hybrid/jobs.rs`,
  `src/daemon/server.rs`)
- Test: `tests/config.rs`, `src/hybrid/config.rs::tests`
**Interfaces:**
- Produces: `ProvidersCfg`, `providers::status`, `embedding_model_id`, updated
  `from_config(cfg, caps)` with `remote_on`/`local_on`.
- Consumes: registry, feature flags.
**Steps:**
1. RED: `providers_in_project_config_are_a_parse_error` (separate type, not post-merge).
2. RED: non-openrouter provider without `model` is admission (no inherited OpenRouter
   default); `dimensions` must match the registry, `normalize=true`.
3. RED: `coverage_and_ann_warm_use_the_canonical_id` — a fake fastembed stage reports 100%
   coverage and the same id the embedder produces.
4. RED: global defaults → project override; OpenRouter behaviour unchanged.
5. GREEN: implement; replace all `cache_model_id` call sites with `embedding_model_id`.
6. Verify: `cargo test --locked --test config --lib hybrid::config`.
**Acceptance:** FR-1, FR-5, FR-9, FR-12, NFR-6; SC-3, SC-10.
**Verify:** same command → ok.
**Depends:** T1.

## T4 — `FastEmbedEmbedder` and role-aware query embedding

**Goal:** local embedding from fetched files, no network, correct prefixes.
**Files:**
- Create: `src/hybrid/local.rs`
- Modify: `src/hybrid/mod.rs`, **`src/daemon/tools.rs` (search calls `embed_query`)**,
  `src/vector_cache.rs` (trait default)
- Test: `src/hybrid/local.rs::tests`, `tests/hybrid_local.rs`
**Interfaces:**
- Produces: `local::embedder`, `Embedder::embed_query` (default = `embed`).
- Consumes: `runtime::resolve/init`, `models::cached`, `fastembed`.
**Steps:**
1. RED: `embedder_without_runtime_fails_with_connect_hint`.
2. RED: `embedder_without_model_fails_with_fetch_hint`.
3. RED: `search_path_uses_embed_query` — a spy `Embedder` in `dense_candidates` records
   `embed_query`; `embed_query_applies_registry_query_prefix` and
   `embed_appends_passage_prefix` for BGE/E5; models without prefixes unchanged.
4. GREEN: build `UserDefinedEmbeddingModel` (bytes + tokenizer files) with pooling,
   `TextEmbedding::try_new_from_user_defined`, `OnceLock`, batch/thread options, prefixing.
5. Verify: `cargo test --locked --features hybrid-local --lib hybrid::local --test hybrid_local`.
**Acceptance:** FR-9, FR-10, NFR-4, NFR-7; SC-6, SC-9.
**Verify:** same command → ok.
**Depends:** T1, T3.

## T5 — `FastEmbedReranker`

**Goal:** local cross-encoder rerank on the selected model (no default; `models use --rerank`).
**Files:**
- Modify: `src/hybrid/local.rs`
- Test: `src/hybrid/local.rs::tests`
**Interfaces:**
- Produces: `local::reranker`; `Reranker` impl.
- Consumes: runtime, models, `fastembed::TextRerank`.
**Steps:**
1. RED: `reranker_without_model_fails_with_fetch_hint`.
2. RED: `reranker_config_requires_an_explicit_model` (no implicit reranker, no default).
3. RED: `rerank_contract_returns_index_and_score_for_every_doc` and `top_n` clamp (mapping
   logic unit-tested; live inference ignored).
4. GREEN: `UserDefinedRerankingModel` (`OnnxSource::File`), `RerankInitOptionsUserDefined`;
   shared model cache; errors → `Internal`.
5. Verify: `cargo test --locked --features hybrid-local --lib hybrid::local`.
**Acceptance:** FR-5, FR-9, FR-12, NFR-7; SC-6.
**Verify:** same command → ok.
**Depends:** T4.

## T6 — `models use` scope, per-stage providers, probe gating

**Goal:** select models per project or as global defaults; work in a local-only build.
**Files:**
- Modify: `src/cli/models.rs` (scope, per-stage provider flags, probe dispatch, global write)
- Test: `tests/cli_models.rs`
**Interfaces:**
- Produces: `--local/--global`, `--embedding-provider`/`--rerank-provider`, `--provider`
  shorthand; `resolve_scope`.
- Consumes: `embedding_model_id`, registry, global config.
**Steps:**
1. RED: scope matrix (project default, `--local`, outside-project requires `--global`, both
   flags error); global write preserves `[providers]` and unrelated sections.
2. RED: `--provider` applies only to stages named in the invocation; untouched stages keep
   their provider; mixed providers written per stage.
3. RED: fastembed probe is offline and runs under `hybrid-local` only; OpenRouter probe runs
   under `hybrid-remote`; `--features hybrid-local` alone can write config.
4. RED: unfetched model → admission with `docsbase models fetch`; dims inferred for
   fastembed, explicit for OpenRouter; wrong dims rejected.
5. GREEN: implement; `--dry-run` prints the diff.
6. Verify: `cargo test --locked --test cli_models --features hybrid-local`.
**Acceptance:** FR-7, FR-8, FR-12; SC-5, SC-11.
**Verify:** `cargo test --locked --test cli_models --features hybrid-local` → ok.
**Depends:** T1, T3.

## T7 — Feature wiring, CI gates, docs

**Goal:** feature builds on 3 OS, default stays offline, docs and release story complete.
**Files:**
- Modify: `Cargo.toml`, `.github/workflows/ci.yml`, `docs/CONFIG.md`, `README.md`,
  `SECURITY.md`, `CHANGELOG.md`
**Steps:**
1. Add `hybrid-local` with the exact manifest from design §2.
2. CI: `linux-local` job (clippy+test, no downloads); add
   `cargo check --features hybrid-local --all-targets` to the existing `windows` and `macos`
   jobs; default tree gate includes `fastembed|ort|tokenizers|hf-hub|image|native-tls|ureq|reqwest`.
3. Docs: CONFIG (providers, scope, connect/fetch), README quickstart, SECURITY (runtime as
   external executable artifact), CHANGELOG.
4. Record in the ledger that `linux-local` must become a required check (human action) and
   verify `release-static` + `dlopen` on Linux.
**Acceptance:** NFR-1, SC-1, SC-8.
**Verify:** tree gate empty; all jobs green.
**Depends:** T1–T6.

## T8 — Live verification, budgets, ledger

**Goal:** prove the real path once and measure.
**Files:**
- Create: `tests/hybrid_local_live.rs` (`#[ignore]`)
- Modify: `docs/specs/hybrid-local/progress.md`
**Steps:**
1. Ignored test: `connect --check` (or `ORT_DYLIB_PATH`) → `models fetch bge-small-en-v1.5`
   → embed 3 texts (query prefix asserted) → `models fetch jina-reranker-v1-turbo-en` →
   rerank 3 docs.
2. Run release live test manually; record ORT version/sha, model sizes, fetch time,
   embed/rerank latency, RSS against NFR-4 budgets (<=700 MB, p95 thresholds).
3. Ledger entry.
**Acceptance:** SC-7, NFR-4.
**Verify:** the ignored command passes and the ledger has numbers.
**Depends:** T2–T5.

## T9 — Lifecycle: remove, uninstall, re-pin, observability

**Goal:** no orphaned runtime/cache, no silent stale state.
**Files:**
- Modify: `src/cli/install.rs`, `src/cli/models.rs`, `src/hybrid/models.rs`,
  `src/hybrid/runtime.rs`, `docs/CONFIG.md`
**Steps:**
1. `models remove` removes all cached revisions; refuses/warns when a daemon may hold the
   model; `--force` overrides.
2. `uninstall` removes `<data>/runtimes/onnxruntime` and the model cache entries owned by
   docsbase.
3. Re-pin: a revision change reports "vectors will be re-embedded, ANN invalidated"; `fetch`
   does not silently switch revisions.
4. `models list` shows provider state, selected `model_id`, cache size; free-space preflight
   before fetch.
5. Verify: unit tests for remove/uninstall manifest; `docsbase uninstall --dry-run` output.
**Acceptance:** FR-7, FR-13; SC-11.
**Verify:** `cargo test --locked --test cli_models --lib hybrid::models` → ok.
**Depends:** T1, T2, T6.

---

## Task-list self-review

- **Spec coverage:** FR-1→T3; FR-2/3/4/11→T2; FR-5/6/9→T1/T3; FR-7/8→T6/T9;
  FR-10→T4; FR-12→T3/T4/T5; FR-13→T9. NFR-1→T7; NFR-2→all; NFR-3→T1/T2; NFR-4→T4/T5/T8;
  NFR-5/6→T1/T2/T3; NFR-8→T7. SC-1→T7; SC-2/3→T2/T3; SC-4→T1; SC-5→T6; SC-6→T4/T5;
  SC-7→T8; SC-8→T7; SC-9→T4; SC-10→T1/T3; SC-11→T6/T9. No uncovered requirement.
- **Type consistency:** `ModelEntry`/`FileSpec`/`ModelSource` (T1) feed T3/T4/T5;
  `ProvidersCfg`/`embedding_model_id` (T3) feed T2/T4/T5/T6; `Target`/`RuntimeSource` (T2)
  feed T8.
- **Review focus:** see header; each failure mode is pinned to a test in T1–T6.
- **Proportion:** 9 tasks, ~6 modules; T9 is lifecycle, not gold-plating.
