# #67 implementation plan — 2026-11-11

Base: `498753e` (#66 + literal mode). New crates: none.
Research: #117 and `filtering-research-2026-11-11.md`.

## S1 — scoped candidates, state and regressions

RED real searches beyond ×4/40/1000 global windows. Capture request-owned
eligible IDs using a literal BINARY path range and tolerant tag AND under a
short metadata snapshot. Admit logical preparation bytes using the existing
search byte policy. Intersect Tantivy with a zero-score document term set;
preserve corpus statistics, query modes and long-chunk bounds. Stream exact
dense over eligible chunks in bounded hash pages, validating generation and
admission each phase; no SQL/project/config lease over vector IO or providers.
Fuse scoped source ranks, rerank once, preserve the document cap. Release
scope admission before final payload admission. Cover state/errors/cleanup,
score invariance and unfiltered compatibility. Fresh task review and fixes.

## S2 — evidence and delivery

Measure real 20k two-folder cold/warm broad/narrow scope vs query-first
reference. Document diagnostics and exact dense tradeoff. Run MSRV tests,
clippy/fmt, quality gate and appropriate static/offline/artifact checks.
Fresh final review and fixes; ready PR with required native CI. Human merge.
#68, ACL, MMR, scope caches and adaptive filtered ANN remain separate work.
