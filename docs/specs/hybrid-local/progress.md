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
