//! Binary document relevance measured within the product's citation window.
use super::ExternalDataset;
use crate::support::bench::Citation;
use anyhow::{Context, ensure};
use serde::Serialize;
use std::collections::BTreeSet;

#[derive(Debug, Serialize)]
pub struct ExternalRank {
    pub document_id: String,
    pub lines: (u32, u32),
    pub judgement: String,
}
#[derive(Debug, Serialize)]
pub struct ExternalMetrics {
    pub hit: bool,
    pub recall: f64,
    pub mrr: f64,
    pub ndcg: f64,
    pub top1_judgement: String,
    pub duplicate_documents: usize,
}
#[derive(Debug, Serialize)]
pub struct ExternalCase {
    pub id: String,
    pub k: usize,
    pub relevant_document_ids: Vec<String>,
    pub ranks: Vec<ExternalRank>,
    pub metrics: ExternalMetrics,
    pub error: Option<String>,
}
#[derive(Debug, Serialize)]
pub struct Summary {
    pub queries: usize,
    pub hits: usize,
    pub misses: usize,
    pub mean_recall: f64,
    pub mean_mrr: f64,
    pub mean_ndcg: f64,
    pub top1_relevant: usize,
    pub top1_explicit_nonrelevant: usize,
    pub top1_unjudged: usize,
    pub empty_results: usize,
    pub query_errors: usize,
}
#[allow(clippy::cast_precision_loss)] // Dataset counts/ranks are well below 2^53.
pub fn evaluate(
    data: &ExternalDataset,
    id: &str,
    hits: &[Citation],
    k: usize,
) -> anyhow::Result<ExternalCase> {
    let qrels = data.qrels.get(id).context("missing case qrels")?;
    let relevant_document_ids: Vec<_> = qrels
        .iter()
        .filter(|(_, s)| **s > 0)
        .map(|(d, _)| d.clone())
        .collect();
    ensure!(!relevant_document_ids.is_empty(), "case has no positives");
    let mut ranks = Vec::new();
    let mut seen = BTreeSet::new();
    let mut found = 0usize;
    let mut dcg = 0.0;
    let mut mrr = 0.0;
    let mut duplicates = 0;
    for (i, hit) in hits.iter().take(k).enumerate() {
        let doc = data
            .path_to_id
            .get(&hit.path)
            .context("citation outside derived corpus")?;
        let fresh = seen.insert(doc.clone());
        if !fresh {
            duplicates += 1;
        }
        let judgement = match qrels.get(doc) {
            Some(s) if *s > 0 => "relevant",
            Some(_) => "explicit_nonrelevant",
            None => "unjudged",
        };
        if judgement == "relevant" && fresh {
            found += 1;
            dcg += 1.0 / ((i + 2) as f64).log2();
            if mrr == 0.0 {
                mrr = 1.0 / (i + 1) as f64;
            }
        }
        ranks.push(ExternalRank {
            document_id: doc.clone(),
            lines: hit.lines,
            judgement: judgement.into(),
        });
    }
    let ideal: f64 = (0..relevant_document_ids.len().min(k))
        .map(|i| 1.0 / ((i + 2) as f64).log2())
        .sum();
    let top1 = ranks
        .first()
        .map_or("empty", |r| r.judgement.as_str())
        .into();
    let recall = found as f64 / relevant_document_ids.len() as f64;
    Ok(ExternalCase {
        id: id.into(),
        k,
        relevant_document_ids,
        ranks,
        error: None,
        metrics: ExternalMetrics {
            hit: found > 0,
            recall,
            mrr,
            ndcg: dcg / ideal,
            top1_judgement: top1,
            duplicate_documents: duplicates,
        },
    })
}
#[allow(clippy::cast_precision_loss)]
pub fn summarize(cases: &[ExternalCase]) -> Summary {
    let n = cases.len() as f64;
    let hits = cases.iter().filter(|c| c.metrics.hit).count();
    let count = |label: &str| {
        cases
            .iter()
            .filter(|c| c.metrics.top1_judgement == label)
            .count()
    };
    Summary {
        queries: cases.len(),
        hits,
        misses: cases.len() - hits,
        mean_recall: cases.iter().map(|c| c.metrics.recall).sum::<f64>() / n,
        mean_mrr: cases.iter().map(|c| c.metrics.mrr).sum::<f64>() / n,
        mean_ndcg: cases.iter().map(|c| c.metrics.ndcg).sum::<f64>() / n,
        top1_relevant: count("relevant"),
        top1_explicit_nonrelevant: count("explicit_nonrelevant"),
        top1_unjudged: count("unjudged"),
        empty_results: count("empty"),
        query_errors: count("query_error"),
    }
}
