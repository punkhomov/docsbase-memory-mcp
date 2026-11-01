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

/// State of the ANN tier as the daemon last observed it (FR-5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnnState {
    /// The tier is off by configuration.
    Disabled,
    /// Hybrid is on but no search has run since the daemon started.
    Loading,
    /// A background build is in flight.
    Building,
    /// The ANN served the last search.
    Ready,
    /// The project is above the resident cap; exact streaming answers.
    TooLarge,
    /// The last search fell back to exact for another reason.
    Fallback,
    /// The last observation is older than the TTL.
    Stale,
    /// The caller cannot see the daemon's state (CLI direct mode).
    Unknown,
}

impl AnnState {
    /// Stable wire name (contract).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Disabled => "disabled",
            Self::Loading => "loading",
            Self::Building => "building",
            Self::Ready => "ready",
            Self::TooLarge => "too_large",
            Self::Fallback => "fallback",
            Self::Stale => "stale",
            Self::Unknown => "unknown",
        }
    }
}

/// How long a recorded observation stays authoritative (FR-6).
pub const ANN_OUTCOME_TTL: std::time::Duration = std::time::Duration::from_secs(60 * 60);

/// The last ANN outcome the search path observed for one project (FR-6).
#[derive(Debug, Clone, Copy)]
pub struct AnnOutcome {
    /// State at that moment.
    pub state: AnnState,
    /// Why, when the state is not `ready`; never empty in that case.
    pub reason: Option<&'static str>,
    /// When it was observed (monotonic).
    pub at: std::time::Instant,
}

impl AnnOutcome {
    /// True once the observation is older than [`ANN_OUTCOME_TTL`].
    #[must_use]
    pub fn is_stale(&self) -> bool {
        self.at.elapsed() > ANN_OUTCOME_TTL
    }
}

/// Filters the caller asked for (spec `search-ux` FR-5). Absent keys mean "not
/// requested", which is different from a filter that matched everything.
#[derive(Debug, Clone, Default)]
pub struct FiltersMeta {
    /// Path prefix, verbatim.
    pub path_prefix: Option<String>,
    /// Required frontmatter tags (AND).
    pub tags: Vec<String>,
    /// `"path"` when dedupe ran, `None` otherwise.
    pub dedupe: Option<&'static str>,
    /// Chunks kept per document when dedupe ran.
    pub n_per_doc: Option<usize>,
}

/// What the post-filter dropped (spec `search-ux` FR-5). Counters are `0` when
/// the corresponding mechanism was not active: a measured zero, not "unknown".
#[derive(Debug, Clone, Copy, Default)]
pub struct TrimmedMeta {
    /// Candidates dropped by `path_prefix`/`tags`.
    pub filtered_out: usize,
    /// Candidates dropped by `dedupe`.
    pub deduped: usize,
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
    /// Filters requested by the caller, `None` when telemetry is off.
    pub filters: Option<FiltersMeta>,
    /// Candidates dropped by the post-filter, `None` when telemetry is off.
    pub trimmed: Option<TrimmedMeta>,
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
        let mut meta = json!({
            "score_scale": self.score_scale.as_str(),
            "stages": self.stages.iter().map(|stage| stage.as_str()).collect::<Vec<_>>(),
            "stage_ms": Value::Object(stage_ms),
            "window": self.window,
            "index_generation": self.index_generation,
            "degraded": self.degraded.iter().map(|reason| reason.as_str()).collect::<Vec<_>>(),
        });
        if let Some(filters) = &self.filters {
            meta["filters"] = json!({
                "path_prefix": filters.path_prefix,
                "tags": filters.tags,
                "dedupe": filters.dedupe,
                "n_per_doc": filters.n_per_doc,
            });
        }
        if let Some(trimmed) = self.trimmed {
            meta["trimmed"] = json!({
                "filtered_out": trimmed.filtered_out,
                "deduped": trimmed.deduped,
            });
        }
        meta
    }
}

/// The envelope returned when the caller asks for telemetry (FR-1).
#[must_use]
pub fn envelope(hits: &[Value], meta: &SearchMeta) -> Value {
    json!({ "hits": hits, "meta": meta.to_json() })
}

#[cfg(test)]
mod tests {
    use super::{AnnState, Degraded, FiltersMeta, ScoreScale, SearchMeta, Stage, TrimmedMeta};

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
    fn ann_states_are_the_contract_set() {
        let states = [
            AnnState::Disabled,
            AnnState::Loading,
            AnnState::Building,
            AnnState::Ready,
            AnnState::TooLarge,
            AnnState::Fallback,
            AnnState::Stale,
            AnnState::Unknown,
        ];
        let names: Vec<&str> = states.iter().map(|state| state.as_str()).collect();
        assert_eq!(
            names,
            vec![
                "disabled",
                "loading",
                "building",
                "ready",
                "too_large",
                "fallback",
                "stale",
                "unknown"
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
            filters: Some(FiltersMeta {
                path_prefix: Some("docs/".to_owned()),
                tags: vec!["api".to_owned()],
                dedupe: Some("path"),
                n_per_doc: Some(2),
            }),
            trimmed: Some(TrimmedMeta {
                filtered_out: 4,
                deduped: 3,
            }),
        };
        let value = meta.to_json();
        assert_eq!(value["score_scale"], "rrf");
        assert_eq!(value["stages"], serde_json::json!(["bm25", "dense", "rrf"]));
        assert_eq!(value["stage_ms"]["bm25"], 1);
        assert_eq!(value["stage_ms"]["total"], 7);
        assert_eq!(value["window"], 50);
        assert_eq!(value["index_generation"], 3);
        assert_eq!(value["degraded"], serde_json::json!(["exact-fallback"]));
        assert_eq!(value["filters"]["path_prefix"], "docs/");
        assert_eq!(value["filters"]["tags"], serde_json::json!(["api"]));
        assert_eq!(value["filters"]["n_per_doc"], 2);
        assert_eq!(value["trimmed"]["filtered_out"], 4);
        assert_eq!(value["trimmed"]["deduped"], 3);
    }
}
