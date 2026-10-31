//! Search telemetry: what the caller must know about its own search
//! (spec `search-telemetry`, FR-2/FR-4).
//!
//! The names are the contract: `score_scale` says which scale `score` is on,
//! `stages` says what actually ran, and `degraded` says why a stage was skipped
//! or fell back. They are constants, never strings built at the call site.

use serde_json::{Value, json};

/// Which scale the `score` field of a citation is on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScoreScale {
    /// Raw BM25: unbounded and corpus-relative.
    Bm25,
    /// Reciprocal-rank fusion: `1/(k+rank)` magnitudes.
    Rrf,
    /// The reranker's own `0..1` output.
    Rerank,
}

impl ScoreScale {
    /// Stable wire name (contract, see `docs/MCP.md`).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Bm25 => "bm25",
            Self::Rrf => "rrf",
            Self::Rerank => "rerank",
        }
    }
}

/// A stage that actually executed for this search, in execution order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    /// Lexical retrieval over the tantivy index.
    Bm25,
    /// Query embedding plus the dense candidate list.
    Dense,
    /// Fusion of the lexical and dense lists.
    Rrf,
    /// Reranking of the fused window.
    Rerank,
}

impl Stage {
    /// Stable wire name (contract).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Bm25 => "bm25",
            Self::Dense => "dense",
            Self::Rrf => "rrf",
            Self::Rerank => "rerank",
        }
    }

    /// `OTel` `GenAI` name pair, so a future exporter needs no renaming (FR-10).
    #[must_use]
    pub fn otel(self) -> (&'static str, &'static str) {
        match self {
            Self::Bm25 => ("retrieval", "gen_ai.retrieval.bm25.duration_ms"),
            Self::Dense => ("retrieval", "gen_ai.retrieval.dense.duration_ms"),
            Self::Rrf => ("retrieval", "gen_ai.retrieval.fusion.duration_ms"),
            Self::Rerank => ("rerank", "gen_ai.rerank.duration_ms"),
        }
    }
}

/// Why a stage did not serve the query in full. Tokens are contract: adding one
/// is allowed, renaming is not (FR-4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Degraded {
    /// The background ANN build is still running; exact rows streamed instead.
    AnnBuilding,
    /// The ANN returned fewer candidates than the fetch window.
    AnnInsufficient,
    /// The ANN lookup failed; exact rows streamed instead.
    AnnError,
    /// Exact streaming answered a query the ANN could not.
    ExactFallback,
    /// The project has no rerank stage, so fused order was returned.
    NoRerankStage,
}

impl Degraded {
    /// Stable wire token (contract).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::AnnBuilding => "ann-building",
            Self::AnnInsufficient => "ann-insufficient",
            Self::AnnError => "ann-error",
            Self::ExactFallback => "exact-fallback",
            Self::NoRerankStage => "no-rerank-stage",
        }
    }
}

/// Telemetry attached to one search response (FR-2).
#[derive(Debug, Clone)]
pub struct SearchMeta {
    /// Which scale `score` is on.
    pub score_scale: ScoreScale,
    /// Stages that executed, in order.
    pub stages: Vec<Stage>,
    /// Wall time of the whole search, in milliseconds (monotonic clock).
    pub total_ms: u64,
    /// Milliseconds per stage; absent keys mean "did not run".
    pub stage_ms: Vec<(Stage, u64)>,
    /// Candidate window actually fetched, after the `MAX_HITS` clamp.
    pub window: usize,
    /// Index generation the answer was served from.
    pub index_generation: i64,
    /// Degradations observed, empty when none.
    pub degraded: Vec<Degraded>,
}

impl SearchMeta {
    /// Wire form. Field names and token values are the contract.
    #[must_use]
    pub fn to_json(&self) -> Value {
        let mut stage_ms = serde_json::Map::new();
        for (stage, ms) in &self.stage_ms {
            stage_ms.insert(stage.as_str().to_owned(), json!(ms));
        }
        stage_ms.insert("total".to_owned(), json!(self.total_ms));
        json!({
            "score_scale": self.score_scale.as_str(),
            "stages": self.stages.iter().map(|stage| stage.as_str()).collect::<Vec<_>>(),
            "stage_ms": Value::Object(stage_ms),
            "window": self.window,
            "index_generation": self.index_generation,
            "degraded": self.degraded.iter().map(|reason| reason.as_str()).collect::<Vec<_>>(),
        })
    }
}

/// The envelope returned when the caller asks for telemetry (FR-1).
#[must_use]
pub fn envelope(hits: &[Value], meta: &SearchMeta) -> Value {
    json!({ "hits": hits, "meta": meta.to_json() })
}

#[cfg(test)]
mod tests {
    use super::{Degraded, ScoreScale, SearchMeta, Stage};

    #[test]
    fn stage_names_have_unique_otel_pairs() {
        let stages = [Stage::Bm25, Stage::Dense, Stage::Rrf, Stage::Rerank];
        let mut seen = Vec::new();
        for stage in stages {
            let (operation, attribute) = stage.otel();
            assert!(!operation.is_empty() && !attribute.is_empty());
            assert!(
                !seen.contains(&attribute),
                "duplicate OTel attribute for {}",
                stage.as_str()
            );
            seen.push(attribute);
        }
    }

    #[test]
    fn degraded_tokens_are_the_contract_set() {
        let tokens: Vec<&str> = [
            Degraded::AnnBuilding,
            Degraded::AnnInsufficient,
            Degraded::AnnError,
            Degraded::ExactFallback,
            Degraded::NoRerankStage,
        ]
        .iter()
        .map(|reason| reason.as_str())
        .collect();
        assert_eq!(
            tokens,
            vec![
                "ann-building",
                "ann-insufficient",
                "ann-error",
                "exact-fallback",
                "no-rerank-stage"
            ]
        );
    }

    #[test]
    fn meta_json_names_every_field() {
        let meta = SearchMeta {
            score_scale: ScoreScale::Rrf,
            stages: vec![Stage::Bm25, Stage::Dense, Stage::Rrf],
            total_ms: 7,
            stage_ms: vec![(Stage::Bm25, 1), (Stage::Dense, 5)],
            window: 50,
            index_generation: 3,
            degraded: vec![Degraded::ExactFallback],
        };
        let value = meta.to_json();
        assert_eq!(value["score_scale"], "rrf");
        assert_eq!(value["stages"], serde_json::json!(["bm25", "dense", "rrf"]));
        assert_eq!(value["stage_ms"]["bm25"], 1);
        assert_eq!(value["stage_ms"]["total"], 7);
        assert_eq!(value["window"], 50);
        assert_eq!(value["index_generation"], 3);
        assert_eq!(value["degraded"], serde_json::json!(["exact-fallback"]));
    }
}
