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
    embeddings; `OnnxSource` is rerank-only and 7.5 needs ort rc.15/MSRV 1.92.)
  - HF pins are `resolve/<commit-sha>/<path>`; the tree API exposes LFS `sha256` + size,
    which the registry will store (regular files: size + computed sha256).

- Correction (2026-10-23, review question): "default reranker" was a spec bug.
  There is no default model for either stage: the registry lists candidates and
  `models use --provider fastembed --embedding …` / `--rerank …` is the only way to
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
  `--rerank-provider`), non-openrouter `model` required (OpenRouter defaults unchanged),
  windows/macos feature checks + required-check note + static binary check, exact cooldown
  pins, NFR-4 thread/budget wording. Added T9 (remove/uninstall/re-pin/observability) and
  SC-10/SC-11; dropped `connect --models`, `DOCSBASE_MODEL_CACHE`, OQ-2/4/6.
