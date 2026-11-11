# Scoped retrieval diagnostic — 2026-11-11

Issue #67; follow-up design tracker #117. Offline Linux x86_64, Rust1.88,
`--release --features cli-full`, 8 build jobs. Fixture: real filesystem, SQLite,
Tantivy and vector cache; 20k documents, 10k in each of `a/` and `b/`, one short
chunk each. Every chunk contains `shared`; ten documents per folder contain
`rareword`. Deterministic two-dimensional vectors, no external provider calls.

Reproduce:

```sh
RUSTUP_TOOLCHAIN=1.88.0 cargo test --locked --release --features cli-full \
  --test scoped_retrieval_perf -- --ignored --nocapture
```

## Measured results (milliseconds)

| Query / scope | Phase | Scoped lexical | Scoped RRF | Query-first lexical reference | Reference candidates examined |
|---|---|---:|---:|---:|---:|
| shared / b/ | first | 21.610 | 212.639 | 14.439 | 21840 |
| shared / b/ | warm | 16.311 | 200.872 | 13.476 | 21840 |
| rareword / b/ | first | 14.769 | 180.643 | 0.284 | 36 |
| rareword / b/ | warm | 14.442 | 163.629 | 0.223 | 36 |
| shared / b/09999.md | first | 10.043 | 20.377 | 23.936 | 41840 |
| shared / b/09999.md | warm | 10.568 | 14.377 | 25.103 | 41840 |

Initial implementation used ordinary `VectorCache::get` for each eligible
vector, including one LRU autocommit each time. Broad RRF: shared first/warm
1869.927/2022.422ms; rareword 2079.110/1796.817ms. Page-local transactions and
statement reuse reduced these corresponding runs about 8.8–11.5×. Each page
still decodes/scores one vector at a time; no full vector page/scope is retained.

`first` means a fresh query cache for that case, not cold disk/OS caches: creating
and indexing the fixture already touched files. `warm` repeats the query/cache.
Each production search reopens its index under the normal request lifecycle.
The reference starts from an already open index, searches only the text field
(the fixture's query terms occur there), applies literal path membership after
collection, and widens 16×4 until enough hits or complete exhaustion. All chunks
are short: no long-chunk penalty adjustment is needed in this reference. It
checks eligible result count, not identical ranking among tied chunks. It has
no provider, payload capture, telemetry/admission/generation or SQL snapshot
lifecycle. **These columns are not interchangeable end-to-end implementations.**
No p95 claim: these are individual diagnostic repetitions, without thresholds.

The rare-query result demonstrates a genuine planner cost: constructing 10k
terms cannot exploit only 20 lexical matches. The narrow common-query case
shows the reverse: query-first refetches/examines 41840 candidates across
attempts to prove exhaustion. Neither strategy wins universally. This PR takes
the simple explicit scope contract; measured adaptive lexical planning and
bounded filtered ANN remain future work in #117. No arbitrary overscan increase
is offered as a completeness solution.

## Work and memory accounting

- Broad scope: 10k request-owned i64 IDs (80kB payload). On this 64-bit build,
  logical lexical admission is 480kB: ID + Term slot + encoded i64 per range
  document. Dense adds 38400B for one 400-hash page. This is a logical allowance,
  **not measured peak RSS**; FST/allocator/SQLite/Tantivy internal workspaces
  are not represented by it. Concurrency shares the configured retained budget.
- Narrow scope: one ID (8B), 48B logical lexical preparation or 38448B with a
  dense hash page. Prefix selection uses the existing covering path index;
  callers do not send 10k IDs through CLI/MCP.
- Broad dense: 25 metadata hash pages, 10k eligible vector lookups/touches,
  25 vector-cache transactions. Before correction: 10k LRU transactions.
  Only one vector's encoded/decoded data and a heap of 40 candidates are live
  during scoring. SQL/project/config leases end before vector-cache work;
  the vector-cache transaction ends before the next metadata page.
- No project-wide resident dense/ANN index is initialized by these requests;
  the diagnostic asserts this. No scope cache or graph per filter is created.
- Small tests independently prove an excluded corrupt vector is never decoded,
  generation change between hash pages fails rather than mixing state, and
  errors/cancellation release byte admission and vector transactions.

Two-dimensional vectors expose transaction/lookup overhead, not realistic
2048-dimensional cosine/vector IO cost. This is neither site acceptance on a
real 20k corpus nor filtered ANN recall validation. No baseline, latency budget,
quality threshold, model inputs or recording provenance was changed.
