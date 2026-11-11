//! Same product search seam as the frozen runner; query errors stay measurable.
use super::{ExternalCase, ExternalDataset, metrics};
use crate::support::bench::{self, BenchEnv};
use anyhow::Context;
use docsbase_memory::daemon::tools::search_docs_with;
use docsbase_memory::error::Error;
use docsbase_memory::hybrid::{HybridStages, dense::DenseCache};
use docsbase_memory::index::QueryMode;

pub fn run(
    env: &BenchEnv,
    data: &ExternalDataset,
    stages: &HybridStages,
    mode: QueryMode,
) -> anyhow::Result<(Vec<ExternalCase>, Vec<f64>)> {
    let dense = DenseCache::new();
    let first = data.cases.first().context("external runner has no cases")?;
    let args =
        |query: &str, k: usize| serde_json::json!({"query":query,"query_mode":mode,"limit":k});
    // One discarded warm-up, identical to the frozen runner's convention.
    let _ = search_docs_with(
        stages,
        &dense,
        &env.db,
        env.cache.path(),
        &env.project,
        &args(&first.query, first.k),
        bench::RERANK_WINDOW,
    );
    let mut cases = Vec::with_capacity(data.cases.len());
    let mut times = Vec::with_capacity(data.cases.len());
    for case in &data.cases {
        let started = std::time::Instant::now();
        let result = search_docs_with(
            stages,
            &dense,
            &env.db,
            env.cache.path(),
            &env.project,
            &args(&case.query, case.k),
            bench::RERANK_WINDOW,
        );
        times.push(started.elapsed().as_secs_f64() * 1000.0);
        let report = match result {
            Ok(value) => {
                let citations = bench::parse_citations(&value, &case.id)?;
                metrics::evaluate(data, &case.id, &citations, case.k)?
            }
            Err(err @ Error::Query { .. }) => {
                let mut report = metrics::evaluate(data, &case.id, &[], case.k)?;
                report.error = Some(err.to_string());
                report.metrics.top1_judgement = "query_error".into();
                report
            }
            Err(err) => {
                return Err(err).with_context(|| format!("external search failed for {}", case.id));
            }
        };
        cases.push(report);
    }
    Ok((cases, times))
}
