# Offline Candidate Census Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox syntax.

**Goal:** collect exact historical raw-vector candidate pools with occurrence-safe raw/context inputs and no inference.

**Architecture:** current search_docs_with selects candidates; a recording identity reranker exposes their complete order through citations. Resolve each by chunk_id to the existing preflight row. Legacy query vectors stay explicitly historical.

**Tech Stack:** Rust1.88, existing SQLite/Tantivy/replay harness, serde_json; no new crates.

**Spec:** search-benchmark/requirements.md CENSUS-71-1…4 and design.md T22.

## Global Constraints

- No `unwrap`/`expect`/`panic!` in `src/**` (tests may use them).
- No new crates without an ADR; the default build stays offline (no network crates).
- The golden baseline is a no-regression gate: improvements are allowed, silent regressions are not. Never regenerate it as a side effect.
- No production, frozen recordings/baseline, CI, tokenizer or runtime configuration edits; no live API calls.

### Task T22: occurrence-safe offline candidate capture

**Files:** create tests/support/representation_census.rs; modify tests/support/representation.rs and tests/search_representation.rs; dated specs/progress.

**Interfaces:** existing prepare(env,cases,hash,revision)->Result<Value> retains its defaults. New private prepare_with_pins adds explicit embedding/rerank pins; collect_candidates(env:&BenchEnv,cases:&[QueryCase],vectors:&VectorFixture,corpus_hash:&str,revision:&str)->Result<Value> returns the preflight plus historical pools.

- [ ] Step1 RED: use existing real duplicate corpus/cases helpers, make4dim pinned replay vectors, call collect_candidates and assert both a.md/b.md shared body occurrences have different context keys. Compare ordered raw_inputs to actual CollectingReranker seen for original k. Run cargo test --locked --features cli-full --test search_representation candidate_census. Expected missing collector compilation error before implementation.

```rust
let root=corpus();
let env=build_env(root.path(), "census", 2..=2).unwrap();
let mut vectors=census_vectors(&env,&cases());
let report=representation::collect_candidates(&env,&cases(),&vectors,&corpus_hash(root.path()).unwrap(),"test-revision").unwrap();
assert_eq!(report["provider_requests"],0);
assert!(report["quality"].is_null());
vectors.query_vectors.as_mut().unwrap().clear();
assert!(representation::collect_candidates(&env,&cases(),&vectors,&corpus_hash(root.path()).unwrap(),"test-revision").is_err());
```

Define census_vectors in the same test file: document hashes come from env.chunk_hashes, each vector is[1,0,0,0], query hashes from unchanged case.query with the same dimension, model pin test/census@d4@l2; query_vectors Some. No fake search result lists.

- [ ] Step2 implement common explicit-pin preflight; prepare still obtains existing component default target pins and delegates. Build census metadata maps from project-local SQL doc_id→path and preflight(path,seq)→row. Use chunk_id_parts to resolve actual citation IDs. Reject duplicate/missing map entries.
- [ ] Step3 instantiate ReplayEmbedder only; check parsed model/dim, all vectors finite and matching dimensions, every query present, then preload_vectors. For each case/config use a fresh capture reranker, args query/original text + limit50, existing RERANK_WINDOW, DenseCache shared only within this isolated capture. Reject k>50 before execution.
- [ ] Step4 validate captured query/body/top_n against complete citations and original occurrence rows. Empty search must have no capture. Return ordered candidates with exact raw/context strings/keys, two rerank pool keys, byte sums, explicit vector role provenance, original qrels/k and source/harness identities. No quality scores.
- [ ] Step5 GREEN: run the new corpus tests plus frozen replay, require every nonempty observed pool to match the existing RerankFixture query/ordered-body/top_n key. Empty lexical observations stay in the denominator. Reuse outside_source_output/clean_revision/write_new for manual export, with DOCSBASE_REPRESENTATION_CENSUS_OUT.
- [ ] Step6 verify default/cli-full search_representation and unchanged search_bench gate; clippy --locked --features cli-full --all-targets -- -D warnings; fmt/diff. Fresh task review; fix Critical/Important and record minors; append Task T22 complete/Ruling to progress.md and commit.
- [ ] Step7 on the clean commit export the full frozen81docs/327chunks/193cases/386observations outside source; audit every candidate/body/context and frozen-pool agreement, then final review/required docs/linux/macos/windows CI and guarded squash merge as explicitly authorized by the user. #71 stays open for new provider recordings and measured full-slice results.

Manual command after task commit:

```sh
DOCSBASE_REPRESENTATION_CENSUS_OUT=/tmp/representation-census.json RUSTUP_TOOLCHAIN=1.88.0 cargo test --locked --features cli-full --test search_representation -- --ignored --exact collect_frozen_representation_candidates --nocapture
```
