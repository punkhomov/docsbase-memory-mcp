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
  - HF pins are `resolve/<commit-sha>/<path>`; the tree API exposes LFS `sha256` + size,
    which the registry will store (regular files: size + computed sha256).
