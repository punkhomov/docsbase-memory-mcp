# Offline Representation Matrix Plan

> Execute sequentially with superpowers:executing-plans. T23 implements only the bounded offline research kernel; all execution uses fake stages.

**Goal:** verify independent document/rerank recipes through the real search pipeline before any new inference.
**Architecture:** exact preflight → guarded body/context mapping → two complete document banks + one shared unchanged-query bank → recipe/membership-qualified temporary caches → actual search_docs_with with memoized transforming reranker. No production edits.
**Stack:** Rust1.88, existing traits/SQLite/vector cache/Tantivy/serde_json; new crates: none.

Binding constraints: no production unwrap/expect/panic, new dependencies require ADR, no baseline/frozen/CI changes, no live stage construction or API calls. #71 remains open.

## Task T23

Files: tests/support/representation_matrix.rs (new), tests/support/representation.rs, tests/search_representation.rs, dated benchmark requirements/design/tasks/progress.

- [x] RED: real indexed unique-body corpus, role-aware fake embedder and deterministic reranker; missing run_matrix entrypoint must fail compilation. Duplicate-body/different-context corpus must reject before any stage method. Preserve existing T20/T22 duplicate support.
- [x] Preflight validates pins/dimensions, k1…50, corpus and unambiguous raw→context mapping before any stage call. Keep SQLite/Tantivy unchanged.
- [x] Explicit document embed calls per recipe; embed_query once per unique unchanged query, separate from same-text documents. Check complete cardinality/dimension/finiteness before caching. Recipe namespaces include base model pin, recipe and full membership digest; query bank is shared despite coarse namespace changes.
- [x] Transform actual ordered candidate inputs in a test-only Reranker. Memo key includes model/query/ordered exact role+recipe+input identities/top_n. Release mutex before provider method. Reject incomplete/duplicate/out-of-range/nonfinite results before caching. Sequential private kernel, no concurrent execution guarantee/singleflight claim.
- [x] Run Bm25Rerank/RrfRerank for all four recipe pairs through search_docs_with; repeat each to verify warm reuse. Keep original k/qrels/citations; record actual pools, returned original citations and exact transformed input identities/bytes. No copied ranking/scoring, metadata rewrites or quality claims.
- [x] GREEN meaningful tests: exact input/role dispatch and shared query bank, independent document/rerank effects, original citations, warm memo reuse; ambiguous body rejection; invalid vectors/query/rerank output and stage errors fail closed. Fake outputs prove machinery only. Method calls are counted separately from unknown wire requests; quality/tokens/cost remain null.
- [x] Default/cli-full representation suite, unchanged search_bench gate, fmt/all-target cli-full Clippy. Fresh task review, findings fixed/ledger committed.
- [ ] Fresh final review, required native CI and inventory audit; guarded squash integration authorized by user. No live pilot/production adoption.

The reusable kernel accepts stage interfaces for test injection but has no live entrypoint, client construction, key lookup, provider transport, persistent recording format or budget implementation. An inference runner with attempt/cost caps and replay format is a separate scope. The research namespaces deliberately invalidate coarsely; they do not implement #78 production cache policy.
