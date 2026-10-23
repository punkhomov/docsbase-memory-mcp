# SPECIFICATION: hybrid-local (итерация 4)

**Status:** Draft, ревизия 0.4 · **Дата:** 2026-10-23
**Constitution:** `docs/specs/constitution.md` (1.3.0)
**Related:** `docs/specs/hybrid-providers/` (dense/RRF), `docs/specs/ann-index/` (ANN),
ADR-12 (cache key), ADR-13 (local models behind a flag), ADR-14 (OpenRouter),
ADR-17 (this iteration).

**Delta 0.3 (2026-10-23, web verification):**
- **ORT matrix is 5, not 6.** ONNX Runtime 1.24.4 ships `linux-x64`, `linux-aarch64`,
  `osx-arm64`, `win-x64`, `win-arm64`; **macOS x86_64 was dropped** and minimum macOS is
  **14.0**. `ort v2.0.0+` maps to ORT **1.24.4** (`api-24`), so pin `1.24.4`, not a range.
- **Model sizes are decisive.** `bge-small-en-v1.5` onnx 133 MB (MIT, 384/Cls);
  `multilingual-e5-small` fp32 470 MB / int8 118 MB (MIT, 384/Mean);
  `bge-reranker-base` **1.11 GB** (MIT); `jina-reranker-v1-turbo-en` int8 **38 MB**
  (Apache-2.0, 37.8M params, 8K tokens). The registry lists jina v1 turbo int8 as the
  supported reranker (bge remains an option); **nothing is selected by default** — the
  reranker is chosen with `models use --provider fastembed --rerank <name>` exactly like
  the embedder. `jina-reranker-v2` stays excluded (CC-BY-NC-4.0, verified).
- **Role-aware prefixes are mandatory.** E5 requires `query: ` / `passage: ` prefixes;
  BGE v1.5 requires the query instruction `Represent this sentence for searching relevant
  passages: ` on queries only. Embedding a query without its prefix silently degrades
  retrieval. The `Embedder` trait gains `embed_query` (default = `embed`); the registry
  carries `query_prefix`/`passage_prefix` and `prompt_version`, and `model_id` includes
  `@p<version>` so a prefix change invalidates vectors.
- **fastembed 7.1.1 loads embedding ONNX via `Vec<u8>`** (no file-based source). This is not
  temporary: 7.5.0 still has `onnx_file: Vec<u8>` (only rerank has `OnnxSource`), and 7.5.0
  pins `ort = 2.0.0-rc.15`, whose MSRV is 1.92. With the chosen int8 models (118 MB, 38 MB)
  the extra copy is bounded; the budget is RSS, not disk. Revisit only with an MSRV bump.

**Delta 0.4 (2026-10-23, independent spec review):** five blockers fixed before T1:
`fastembed` must be `default-features = false` with a direct `ort` `load-dynamic`
dependency (otherwise the build links/downloads ORT and pulls hf-hub/image/native-tls);
the "7.5 adds file-based embedding loading" claim was false (7.5 still uses `Vec<u8>`,
`OnnxSource` is rerank-only, and 7.5 needs ort rc.15/MSRV 1.92); `models use` must work in
a `hybrid-local`-only build (per-provider probe); `embed_query` needs the
`daemon::tools::dense_candidates` call site; and one canonical `embedding_model_id` must
replace the `cache_model_id`/`Embedder::model_id` split. Also: structural
`ProjectFileConfig` (no `[providers]` in projects), provider-entry-aware runtime
resolution and `init_from` ordering, purpose-first `models use embed|rerank`,
non-openrouter `model` required (OpenRouter defaults unchanged), exact cooldown pins,
platform CI coverage, and a new lifecycle requirement (remove/uninstall/re-pin/
observability). FR-14 was renumbered to FR-13.

## 1. Problem

Hybrid retrieval supports one provider, OpenRouter (`hybrid-remote`), which needs an API
key and network access on index/backfill/search. Projects with privacy requirements or
without network cannot use dense retrieval or rerank at all, and there is no way to run the
whole hybrid pipeline offline. The build already has an extension point (`Embedder`,
`Reranker`) but no local implementation, no runtime/model lifecycle, and no configuration
model for "this machine has provider X".

## 2. Goals / Non-goals

Goals:
- **G1.** A local `fastembed` provider for embedding and rerank behind feature
  `hybrid-local`; existing `Embedder`/`Reranker` contracts unchanged.
- **G2.** The default build stays fully offline (no `fastembed`, `ort`, `tokenizers`,
  `hf-hub`, `ureq`, `reqwest` in `cargo tree`).
- **G3.** No network on index/search paths: runtime and models are prepared only by explicit
  `docsbase connect` / `docsbase models fetch`.
- **G4.** One configuration model for providers and models: `[providers.*]` is machine-local
  (global config only); model selection is project-local by default, or global defaults for
  new projects.
- **G5.** Reproducibility: pinned model revisions, sha256 for runtime and model files,
  atomic/idempotent installation.

Non-goals:
- GPU/CUDA/Metal/DirectML, image models, fine-tuning, model auto-update.
- MCP tools for connect/fetch (CLI only).
- Default models or any automatic download.
- Changes to `search_docs`, MCP schemas, ANN contracts, or ranking.

## 3. Users / Actors

- **Developer (local machine)** — wants semantic search offline, runs `connect` and
  `models fetch` once, then edits project config.
- **Operator/CI** — prepares a machine reproducibly (`connect --check`, `models fetch`),
  must be able to run without interactive prompts.
- **Coding agent (MCP client)** — consumes `search_docs`; never triggers downloads and must
  get an actionable error if the provider is not prepared.
- **Maintainer** — needs a dependency/artifact story that does not grow the default build.

## 4. Prioritized user stories

### P1 — Prepare the local provider on this machine
As a developer, I want `docsbase connect fastembed` to install the ONNX Runtime and
register the provider, so that local models can run without a preinstalled runtime.

**Why this priority:** nothing else works without the runtime; it is the setup gate.
**Independent test:** on a clean data dir, `connect fastembed` writes
`[providers.fastembed]` to the global config and the runtime file exists; a second run
changes nothing.
**Acceptance scenarios:**
- Given no runtime and network access, when `connect fastembed` runs, then the pinned
  runtime is downloaded, its sha256 matches, and the provider is `Connected`.
- Given an already installed runtime, when `connect fastembed` runs again, then it is a
  no-op (no download).
- Given a corrupted download, when sha256 does not match, then nothing is installed and the
  command fails with an actionable error.

### P2 — Fetch a model explicitly
As a developer, I want `docsbase models fetch bge-small-en-v1.5` to download a pinned model,
so that later searches are offline and reproducible.

**Why this priority:** separates the only network step from search, and keeps the build and
runtime free of weights.
**Independent test:** after `fetch`, the model directory has every registry file and a
`.complete` marker; repeating `fetch` downloads nothing.
**Acceptance scenarios:**
- Given network access, when `fetch <name>` runs, then all pinned files are downloaded,
  size/sha256 verified, and the cache is marked complete.
- Given no network, when `fetch <name>` runs and the model is absent, then it fails with an
  error that names the missing model and does not leave a partial install.
- Given a partially downloaded model, when search runs, then it fails with "run models
  fetch" (never uses partial files).

### P3 — Select the model for a project or as a global default
As a developer, I want `models use` to switch the embedding/rerank model for the current
project (or for new projects with `--global`), so that provider choice is explicit.

**Why this priority:** model choice changes `model_id`, invalidates the vector cache and
ANN; it must be deliberate and scoped.
**Independent test:** inside a project, `docsbase models use embed bge-small-en-v1.5`
writes `[hybrid.embedding]` in `.docsbase.toml` and leaves `[hybrid.reranker]` untouched;
`docsbase models use rerank jina-reranker-v1-turbo-en --window 50` writes only that stage;
outside a project both fail unless `--global` is passed.
**Acceptance scenarios:**
- Given a project directory, when `models use embed <model>` runs without a scope flag,
  then `.docsbase.toml` gets `[hybrid.embedding]` only.
- Given a project directory, when `models use rerank <model> --window 50` runs, then only
  `[hybrid.reranker]` changes.
- Given `models use` without a purpose, then it prints help and changes nothing.
- Given a command that tries to set both purposes, then it is rejected by the CLI shape.
- Given no project, when `models use` runs without `--global`, then it fails with a message
  explaining the required scope.
- Given both `--local` and `--global`, then the command fails.
- Given `--provider fastembed` and an unfetched model, then it fails and names
  `docsbase models fetch`.

### P4 — Offline embedding for index/backfill/search
As a developer, I want search and backfill to embed locally without network, so that
private corpora never leave the machine.

**Why this priority:** this is the product value; everything else enables it.
**Independent test:** with the model fetched and network disabled, indexing a project
produces vectors, `search_docs` returns dense results, and no network syscall is attempted
(the existing offline test suite stays green for the default build; a feature-gated test
asserts no fetch on the search path).
**Acceptance scenarios:**
- Given `provider = "fastembed"` with a fetched model, when the dense channel runs, then
  `model_id` is `fastembed/<name>@d<dim>@l2@r<rev8>` and vectors are cached under it.
- Given a missing model, when a project is opened, then it fails at open/search with an
  admission error (`docsbase models fetch …`).
- Given `dimensions` that differ from the registry, when config is loaded, then it fails
  before any embedding.

### P5 — Local rerank
As a developer, I want the reranker to run locally, so that the whole hybrid pipeline is
offline.

**Why this priority:** completes the pipeline; rerank is optional (window bounded).
**Independent test:** a fetched reranker changes the order of candidate documents and
returns `(index, score)` pairs; without it, config validation fails.
**Acceptance scenarios:**
- Given `[hybrid.reranker] provider = "fastembed"` and a fetched model, when rerank runs,
  then every candidate receives a score and the order matches scores.
- Given an unfetched reranker, when the project opens, then it fails with the fetch hint.

## 5. Functional requirements

- **FR-1 (must, structural).** The global config may contain `[providers.<name>]`; the
  project file is parsed by a **separate type without a `providers` field**, so
  `[providers]` in `.docsbase.toml` is a parse error mapped to `Admission` (not a
  post-merge check). A committed `runtime_path` must never be able to load a library.
- **FR-2 (must).** `docsbase connect` with no arguments prints provider status
  (`Unknown | NotConnected | Connected{version} | Broken{reason}`).
- **FR-3 (must).** `connect openrouter` reads `OPENROUTER_API_KEY` from the environment,
  probes the provider (documented request and its token cost; offline → actionable
  error), and records `[providers.openrouter]`; the key is never stored or logged.
- **FR-4 (must).** `connect fastembed` installs ONNX Runtime **1.24.4** for the current
  target (`linux-x64`, `linux-aarch64`, `osx-arm64`, `win-x64`, `win-arm64`;
  macOS >= 14), verifies sha256 and size, installs atomically, and records
  `[providers.fastembed]`; supports `--check`, `--force`; `--local` is rejected.
  `connect` never selects a model.
- **FR-5 (must).** A closed model registry (name, task, repo, pinned revision, files,
  dim, pooling, quantization, output key, license, prefixes, prompt version) exists in
  code. The registry selects nothing; OpenRouter's existing stage defaults are unchanged.
  For any non-openrouter provider, `model` is **required** in the stage config — a missing
  model is an admission error and must not inherit the OpenRouter default.
- **FR-6 (must).** `models fetch <name>` downloads only registry files at the pinned
  revision, verifies size+sha256 and enforces per-file/archive size caps, installs
  atomically under a lock, writes `.complete` last, is idempotent, and re-verifies
  file hashes when a cached model is opened (a modified cache is not trusted). Offline or
  mismatch → error with no partial install.
- **FR-7 (must).** `models list` reports registry entries, cached revisions, sizes and
  status; `models remove <name>` removes all cached revisions and refuses (or warns and
  requires `--force`) while a daemon/process may have the model loaded.
- **FR-8 (must).** `models use` is purpose-first and single-purpose per invocation:
  `models use embed <model> [options]` or `models use rerank <model> [options]`.
  The purpose subcommand is required; **one model per invocation**, and setting both stages
  in one call is impossible by CLI shape (no `--embedding`/`--rerank` mixing).
  Options are scoped to the purpose: `embed` takes `--dims` (optional for fastembed —
  inferred from the registry and validated; required for OpenRouter), `rerank` takes
  `--window`; both take `--provider`, `--local`, `--global`, `--dry-run`, `--path`.
  `--provider` may be omitted only when the model name is a local registry entry (provider
  inferred); an unknown name requires `--provider` (OpenRouter has no catalog).
  Scope: inside a project it writes `.docsbase.toml` by default (`--local` optional);
  `--global` writes the global config (defaults for new projects) and is required outside a
  project; both flags → error. Only the selected stage's table is written; the other stage
  is untouched. Validation: provider connected, model cached (fetch hint), dims/window in
  bounds. Probe per provider: OpenRouter under `hybrid-remote`, fastembed offline against
  the cached model; `models use` must work in a `hybrid-local`-only build.
- **FR-9 (must).** `provider = "fastembed"` constructs `Embedder`/`Reranker`; a **single
  canonical model-id function** (not two) is used by the embedder, `coverage()` and the ANN
  warm path: `fastembed/<name>@d<dim>@<l2|raw>@r<rev8>@p<prompt_version>`. `normalize`
  must be true for registry models.
- **FR-10 (must).** Asymmetric retrieval prefixes: `Embedder` gains
  `embed_query(&self, text: &str) -> Result<Vec<f32>>` (default: `embed([text])`).
  `daemon::tools::dense_candidates` (the search path) calls `embed_query`; backfill calls
  `embed` for passages. Registry entries carry `query_prefix`/`passage_prefix` (E5:
  `query: `/`passage: `; BGE: instruction/empty). A prefix change bumps `prompt_version`
  and therefore the canonical model id.
- **FR-11 (must).** Runtime resolution: `ORT_DYLIB_PATH` is read by docsbase itself; if it
  is unset, a connected `[providers.fastembed]` entry is required; path order is
  `ORT_DYLIB_PATH` → `providers.fastembed.runtime_path` → data-dir → admission error.
  `ort::init_from(path)` is the **first** ORT call (before any session), and a failed load
  is an admission error (the implementation must not let `ort::api()` panic on an
  uninitialized library).
- **FR-12 (must).** Error taxonomy: provisioning failures (missing provider/runtime/model,
  dims mismatch, unsupported target, offline fetch) are `Admission` with exactly one
  corrective action; a hash/size mismatch is an `Internal` fetch error that leaves nothing
  installed; inference failures are `Internal`. No silent fallback to lexical/exact because
  of a local-provider failure.
- **FR-13 (should).** Lifecycle integration: `docsbase uninstall` removes installed
  runtimes; a revision bump (`model_id` change) reports the re-embed/ANN-invalidate cost;
  `models list` shows provider state, selected model id and cache size (observability);
  provisioning under a running daemon notes that the daemon must reload/restart.

## 6. Non-functional requirements (ISO 25010)

- **NFR-1 (portability / offline default).** The default `cargo tree` contains none of
  `fastembed`, `ort`, `tokenizers`, `hf-hub`, `ureq`, `reqwest`; a CI gate asserts this.
- **NFR-2 (maintainability).** MSRV 1.88, edition 2024, `cargo fmt --check`, clippy
  `--all-targets -D warnings`, no `unwrap`/`expect`/`panic!` in `src/**`.
- **NFR-3 (security).** HTTPS with rustls; pinned revisions; sha256 for runtime and model
  files; size caps; cache permissions 0700; the runtime is treated as executable code and
  is installed only by explicit `connect`; ONNX/tokenizer inputs are untrusted.
- **NFR-4 (resource efficiency).** `max_length` default 512, configurable batch size;
  `intra_threads` capped at `min(4, cores)`; inter-op threads via `session_config` only if
  exposed (fastembed 7.1.1 exposes intra only). Model/runtime memory is excluded from
  `DenseCache.max_bytes`; a model is loaded once per process. Target budgets verified in T8:
  RSS ≤ 700 MB with e5-small int8 + jina int8, embed p95 ≤ 250 ms per 512-token chunk,
  rerank p95 ≤ 200 ms for 50 documents on the CI runner class.
- **NFR-5 (reliability / idempotency).** `connect` and `fetch` can be repeated safely;
  concurrent fetches are serialized by a lock; an interrupted install leaves no usable
  partial state (`.complete` last, temp+rename).
- **NFR-6 (startup failure).** A config that selects fastembed with a missing runtime or
  model fails when the project is opened/searched, not mid-backfill.
- **NFR-7 (performance).** Local embed/rerank must not block async workers (`blocking::enter`
  contract); one model instance per config key; no per-call model load.
- **NFR-8 (artifact size).** The 40 MiB binary budget (NFR-2 of the product design) excludes
  runtime and model caches; their sizes are documented in `docs/CONFIG.md`.

## 7. Data model

**Provider entry (global config):**

| Field | Type | Meaning |
|---|---|---|
| `name` | table key | `fastembed`, `openrouter` |
| `enabled` | bool | provider prepared on this machine |
| `runtime` | string? | `fastembed`: installed ONNX Runtime version |
| `runtime_path` | string? | optional override path |

**Model registry entry (code):** `name`, `task` (`Embedding|Rerank`), `repo`,
`revision` (HF commit sha), `onnx`, `additional[]`, `dim`, `pooling` (`Cls|Mean`, embedding
only), `quantization`, `output_key`, `license`, `files[]` (`path`, `size`, `sha256`),
`query_prefix`, `passage_prefix`, `prompt_version`.

**Model cache:** `<cache>/<name>/<revision>/` with registry files plus `.complete`; the
revision is part of the path so updates never mix vectors.

**Selection (project or global config):** `[hybrid.embedding]` (`provider`, `model`,
`dimensions`, `normalize`) and `[hybrid.reranker]` (`provider`, `model`, `window`).

**Cache key:** `model_id = fastembed/<name>@d<dim>@<l2|raw>@r<rev8>@p<prompt_version>`.

## 8. Interfaces

**CLI**

| Command | Flags | Result |
|---|---|---|
| `docsbase connect` | — | status table |
| `docsbase connect openrouter` | — | env key validated; global provider entry written |
| `docsbase connect fastembed` | `--check`, `--force` | runtime installed; provider entry written |
| `docsbase models list` | — | registry + cached/missing |
| `docsbase models fetch <name>` | — | pinned files installed |
| `docsbase models remove <name>` | — | cache removed |
| `docsbase models use embed <model>` | `--provider`, `--dims`, `--local`, `--global`, `--dry-run`, `--path` | `[hybrid.embedding]` written or printed |
| `docsbase models use rerank <model>` | `--provider`, `--window`, `--local`, `--global`, `--dry-run`, `--path` | `[hybrid.reranker]` written or printed |

**Config files:** global `config.toml` (`[providers.*]`, optional `[hybrid.*]` defaults);
project `.docsbase.toml` (`[hybrid.*]` only).

**Errors:** admission errors carry `provider`, `model`, `action` (e.g. `docsbase connect
fastembed`), and never contain keys or file contents.

## 9. Measurable success criteria

- **SC-1.** Default build: `cargo tree` gate passes; `cargo test` green.
- **SC-2.** `connect fastembed` on a clean machine installs the pinned runtime with a
  matching sha256 and writes `[providers.fastembed]`; a second run does no download.
- **SC-3.** With no provider entry, a fastembed config fails with exactly the connect hint.
- **SC-4.** `models fetch` installs all registry files, verifies hashes, is idempotent, and
  leaves no partial install when interrupted/offline.
- **SC-5.** `models use embed|rerank <model>` is single-purpose: one stage per invocation,
  purpose-specific options only, scope matrix (project default, `--local`, `--global`,
  outside-project error, both-flags error), and it never rewrites `[providers]` or the
  other stage.
- **SC-6.** With runtime+model prepared, embedding and rerank run offline, `model_id`
  contains the revision, and cached vectors are reused after restart.
- **SC-7.** The ignored live test (connect/fetch/embed/rerank on a real model) passes on a
  machine with network, and the ledger records versions, hashes, sizes, latency and RSS.
- **SC-8.** Docs updated: ADR-17, this spec, `docs/CONFIG.md`, README, SECURITY, CHANGELOG.
- **SC-9.** Query/passage prefixes: search calls `embed_query`, backfill calls `embed`; a
  test fails if a registry prefix is dropped or applied to the wrong role (FR-10).
- **SC-10.** One canonical model id: `coverage()` reaches 100% and the ANN warm key equals
  `embedder.model_id()` for fastembed (no write-A/read-B key split) (FR-9).
- **SC-11.** `models use` works in a `hybrid-local`-only build (fastembed probe offline);
  `models remove`/`uninstall` handle runtime and model caches; a re-pin reports the
  re-embed cost (FR-7, FR-13).

## 10. Assumptions & challenges

| Assumption | Adversarial question | Resolution |
|---|---|---|
| ORT 1.24.4 can be downloaded per platform and loaded with `ort-load-dynamic` | What if `ort` rc.13 changes the dynamic-loading API or the prebuilt is unavailable? | `ort v2.0.0+` maps to ORT 1.24.4 (`api-24`); five assets exist; pin both; a live test + `--check`; `ORT_DYLIB_PATH` fallback |
| Pinned HF revision + sha256 is enough provenance | What if HF rewrites LFS objects or a repo is deleted? | Revision is a commit sha; sha256 is in-code; `fetch` fails closed; registry update is a code change |
| `[providers]` only in global config | What about a project checked into a repo with a provider entry? | Explicit validation error; machine capabilities are not repository state |
| No default model | Is the UX acceptable? | `models use --provider fastembed --embedding …` is explicit; `connect` prints the exact snippet; defaults would hide re-embedding costs |
| 14-day cooldown allows `fastembed 7.1.1` | What if 7.1.1 has a bug fixed in 7.2, or we need file-based embedding loading? | No file-based embedding path exists at MSRV 1.88 (7.5 still uses `Vec<u8>` and needs ort rc.15/MSRV 1.92); budget one model copy in RSS; upgrade is an MSRV task |
| No weights in the binary | Does `fastembed` embed any model? | Only code; `UserDefinedEmbeddingModel` is built from fetched files |
| `models use` can rewrite TOML without `toml_edit` | Are comments lost? | Known limitation, printed by the command; no new dependency in v1 |
| Oniguruma (C) builds on all CI targets | Windows/macOS toolchains? | Already build bundled sqlite3; CI matrix proves it in T7 |

## 11. Out of scope

GPU backends, image embeddings, model auto-update, model sharing between machines,
MCP management tools, `docsbase serve` model endpoints, quantized-on-the-fly conversion.

## 12. Open questions

- **OQ-1.** Exact ORT 1.24.4 release URLs/sha256 for the five targets — resolved in T2
  (official Microsoft release archives; macOS x86_64 is not published).
- **OQ-3.** Live CI run cadence for a real model — default: manual/nightly, never per-PR.
- **OQ-5.** Multilingual default: E5-small int8 (118 MB, prefixes) vs bge-small (English,
  133 MB). Proposed: list both; docs recommend E5 for RU/EN; no automatic default.
- **OQ-7.** Mixed providers per stage (fastembed embedding + OpenRouter rerank): allowed by
  config and written with two separate invocations (`models use embed …`, then
  `models use rerank …`); no command sets both stages at once.
