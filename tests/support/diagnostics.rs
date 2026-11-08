//! Absolute retrieval quality is advisory and independent of the regression verdict.
use super::bench::{Baseline, CaseReport, ConfigId, FailureKind};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct QualityCounts {
    pub queries: usize,
    pub hits: usize,
    pub misses: usize,
    pub accepted_misses: usize,
    pub new_misses: usize,
    pub unclassified_misses: usize,
    pub top1_relevant: usize,
    pub top1_trap: usize,
    pub top1_unjudged: usize,
    pub empty_results: usize,
    pub mean_recall: f64,
    pub mean_ndcg: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConfigQuality {
    pub total: QualityCounts,
    pub slices: BTreeMap<String, QualityCounts>,
    pub miss_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QualityDiagnostics {
    pub configs: BTreeMap<ConfigId, ConfigQuality>,
    pub retained_trap_allowances: usize,
    /// Intersection over exactly these configurations, never an implied unseen mode.
    pub common_miss_configs: Vec<ConfigId>,
    pub common_miss_ids: Vec<String>,
}

#[allow(clippy::cast_precision_loss)] // Benchmark query counts are far below 2^53.
fn counts(cases: &[&CaseReport], previous: &BTreeMap<&str, bool>) -> QualityCounts {
    let mut out = QualityCounts {
        queries: cases.len(),
        ..QualityCounts::default()
    };
    for case in cases {
        if case.metrics.hit {
            out.hits += 1;
        } else {
            out.misses += 1;
            match previous.get(case.id.as_str()) {
                Some(false) => out.accepted_misses += 1,
                Some(true) => out.new_misses += 1,
                None => out.unclassified_misses += 1,
            }
        }
        if case.ranks.is_empty() {
            out.empty_results += 1;
        } else if case.metrics.forbidden_hit {
            out.top1_trap += 1;
        } else if case.metrics.mrr >= 1.0 - super::bench::GATE_EPSILON {
            out.top1_relevant += 1;
        } else {
            out.top1_unjudged += 1;
        }
        out.mean_recall += case.metrics.recall;
        out.mean_ndcg += case.metrics.ndcg;
    }
    if out.queries > 0 {
        out.mean_recall /= out.queries as f64;
        out.mean_ndcg /= out.queries as f64;
    }
    out
}

pub fn diagnose(current: &Baseline, baseline: Option<&Baseline>) -> QualityDiagnostics {
    let mut configs = BTreeMap::new();
    let mut common: Option<BTreeSet<String>> = None;
    for (cfg, report) in &current.configs {
        let previous: BTreeMap<&str, bool> = baseline
            .and_then(|base| base.configs.get(cfg))
            .map(|r| {
                r.cases
                    .iter()
                    .map(|c| (c.id.as_str(), c.metrics.hit))
                    .collect()
            })
            .unwrap_or_default();
        let all: Vec<&CaseReport> = report.cases.iter().collect();
        let mut grouped: BTreeMap<&str, Vec<&CaseReport>> = BTreeMap::new();
        for case in &report.cases {
            grouped.entry(&case.class).or_default().push(case);
        }
        let misses: BTreeSet<String> = report
            .cases
            .iter()
            .filter(|c| !c.metrics.hit)
            .map(|c| c.id.clone())
            .collect();
        common = Some(match common {
            None => misses.clone(),
            Some(ids) => ids.intersection(&misses).cloned().collect(),
        });
        configs.insert(
            *cfg,
            ConfigQuality {
                total: counts(&all, &previous),
                slices: grouped
                    .into_iter()
                    .map(|(name, cases)| (name.to_owned(), counts(&cases, &previous)))
                    .collect(),
                miss_ids: misses.into_iter().collect(),
            },
        );
    }
    QualityDiagnostics {
        common_miss_configs: configs.keys().copied().collect(),
        configs,
        retained_trap_allowances: baseline
            .unwrap_or(current)
            .known_failures
            .iter()
            .filter(|f| f.kind == FailureKind::ForbiddenHit)
            .count(),
        common_miss_ids: common.unwrap_or_default().into_iter().collect(),
    }
}
