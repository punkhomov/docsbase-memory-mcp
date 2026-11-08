//! Explicit offline replay identity; evaluation never constructs provider clients.
use super::{ADAPTER_VERSION, ExternalDataset, ExternalOptions, hash, read_json};
use crate::support::bench::{
    self, ConfigId, ModelPin, ReplayEmbedder, ReplayReranker, ReplaySet, RerankFixture,
    VectorFixture,
};
use anyhow::{Context, ensure};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReplayIdentity {
    pub derived_corpus_hash: String,
    pub queries_hash: String,
    pub adapter_version: u32,
    pub tokenizer_version: u32,
    pub embedding_model_pin: Option<String>,
    pub rerank_model_pin: Option<String>,
}
pub struct LoadedReplay {
    pub stages: ReplaySet,
    pub pins: Vec<String>,
    pub hashes: BTreeMap<String, String>,
}
pub fn load(data: &ExternalDataset, options: &ExternalOptions) -> anyhow::Result<LoadedReplay> {
    let mut loaded = LoadedReplay {
        stages: ReplaySet {
            embedder: None,
            reranker: None,
            vectors: None,
        },
        pins: vec![],
        hashes: BTreeMap::new(),
    };
    let dense = options
        .configs
        .iter()
        .any(|c| matches!(c, ConfigId::Rrf | ConfigId::RrfRerank));
    let rerank = options
        .configs
        .iter()
        .any(|c| matches!(c, ConfigId::Bm25Rerank | ConfigId::RrfRerank));
    if !dense && !rerank {
        return Ok(loaded);
    }
    let dir = options
        .replay_dir
        .as_ref()
        .context("explicit replay directory required")?;
    let identity_path = dir.join("identity.json");
    let identity_bytes = std::fs::read(&identity_path).context("read replay identity.json")?;
    let identity: ReplayIdentity =
        serde_json::from_slice(&identity_bytes).context("parse replay identity")?;
    ensure!(
        identity.derived_corpus_hash == data.derived_corpus_hash
            && identity.queries_hash == data.queries_hash,
        "replay corpus/queries identity mismatch"
    );
    ensure!(
        identity.adapter_version == ADAPTER_VERSION
            && identity.tokenizer_version == docsbase_memory::index::tokenizer::TOKENIZER_VERSION,
        "replay adapter/tokenizer mismatch"
    );
    loaded
        .hashes
        .insert("identity.json".into(), hash(&identity_bytes));
    if dense {
        let pin = identity
            .embedding_model_pin
            .context("embedding model pin required")?;
        load_dense(data, dir, &pin, &mut loaded)?;
        loaded.pins.push(pin);
    }
    if rerank {
        let pin = identity
            .rerank_model_pin
            .context("rerank model pin required")?;
        ensure!(
            matches!(bench::parse_model_pin(&pin)?, ModelPin::Rerank { .. }),
            "rerank model pin has wrong kind"
        );
        let rel = format!("rerank/{}.json", bench::pin_slug(&pin));
        let fixture = RerankFixture::load(&dir.join(&rel))?;
        // Product reranking can append unscored candidates. An evaluation
        // recording must prove this selected stage scored every candidate.
        for (key, pool) in &fixture.pools {
            let unique: std::collections::BTreeSet<_> =
                pool.results.iter().map(|(index, _)| *index).collect();
            ensure!(
                pool.top_n == key.candidate_hashes.len()
                    && pool.results.len() == pool.top_n
                    && unique.len() == pool.top_n,
                "rerank recording needs complete unique scores for every candidate"
            );
        }

        ensure!(
            fixture.model_pin == pin,
            "recorded rerank model pin mismatch"
        );
        loaded
            .hashes
            .insert(rel.clone(), bench::blake3_file(&dir.join(rel))?);
        loaded.stages.reranker = Some(Arc::new(ReplayReranker {
            fixture: Arc::new(fixture),
        }));
        loaded.pins.push(pin);
    }
    Ok(loaded)
}

fn load_dense(
    data: &ExternalDataset,
    dir: &Path,
    pin: &str,
    loaded: &mut LoadedReplay,
) -> anyhow::Result<()> {
    let ModelPin::Embedding { dim, .. } = bench::parse_model_pin(pin)? else {
        anyhow::bail!("embedding model pin has wrong kind");
    };
    let rel = format!("vectors/{}", bench::pin_slug(pin));
    let root = dir.join(&rel);
    let manifest: serde_json::Value = read_json(&root.join("manifest.json"))?;
    let bin = manifest["bin"]
        .as_str()
        .context("vector manifest bin missing")?;
    ensure!(
        !bin.is_empty()
            && bin
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
            && bin != "."
            && bin != "..",
        "vector bin must be a filename"
    );
    let fixture = VectorFixture::load(&root)?;
    ensure!(
        fixture.model_pin == pin && fixture.dim == usize::try_from(dim)?,
        "replay vector model/dimension mismatch"
    );
    ensure!(
        fixture.corpus_hash == data.derived_corpus_hash,
        "recorded vector corpus mismatch"
    );
    // Annotation-only changes do not alter inference inputs; query/chunk
    // coverage below is authoritative for content-addressed replay.
    for case in &data.cases {
        fixture.get(&case.query)?;
    }
    loaded.hashes.insert(
        format!("{rel}/manifest.json"),
        bench::blake3_file(&root.join("manifest.json"))?,
    );
    loaded
        .hashes
        .insert(format!("{rel}/{bin}"), bench::blake3_file(&root.join(bin))?);
    loaded.stages.embedder = Some(Arc::new(ReplayEmbedder {
        pin: pin.to_owned(),
        fixture: Arc::new(fixture.clone()),
    }));
    loaded.stages.vectors = Some(fixture);

    Ok(())
}
