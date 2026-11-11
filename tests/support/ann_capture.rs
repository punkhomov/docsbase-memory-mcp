//! Test-only preservation and replay of an already-built recall graph.

use std::path::{Path, PathBuf};

use docsbase_memory::hybrid::ann::AnnIndex;
use docsbase_memory::hybrid::ann_store::{AnnKey, AnnMeta, AnnStore};
use serde::{Deserialize, Serialize};

#[derive(Debug, PartialEq, Serialize, Deserialize)]
pub struct Observation {
    pub query: Vec<f32>,
    pub hits: Vec<(u64, f32)>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Capture {
    pub meta: AnnMeta,
    pub fetch: usize,
    pub observations: Vec<Observation>,
    pub os: String,
    pub arch: String,
    pub available_parallelism: Option<usize>,
    pub rayon_num_threads: Option<String>,
    pub source_sha: Option<String>,
}

/// A kept temporary directory avoids overwriting a concurrent failure's graph.
pub fn save(
    parent: &Path,
    ann: &AnnIndex,
    key: &AnnKey,
    fetch: usize,
    observations: &[Observation],
) -> Result<PathBuf, Box<dyn std::error::Error>> {
    std::fs::create_dir_all(parent)?;
    let bundle = tempfile::Builder::new()
        .prefix("failure-")
        .tempdir_in(parent)?
        .keep();
    // Preserve observations even if dumping the graph itself fails.
    let capture = serde_json::json!({
        "meta": AnnMeta::new(key),
        "fetch": fetch,
        "observations": observations,
        "os": std::env::consts::OS,
        "arch": std::env::consts::ARCH,
        "available_parallelism": std::thread::available_parallelism().ok().map(usize::from),
        "rayon_num_threads": std::env::var("RAYON_NUM_THREADS").ok(),
        "source_sha": std::env::var("GITHUB_SHA").ok(),
    });
    std::fs::write(
        bundle.join("observations.json"),
        serde_json::to_vec_pretty(&capture)?,
    )?;
    ann.dump(&AnnStore::new(bundle.clone()), key)?;
    Ok(bundle)
}

pub fn load(bundle: &Path) -> Result<(Capture, AnnIndex), Box<dyn std::error::Error>> {
    let capture: Capture =
        serde_json::from_slice(&std::fs::read(bundle.join("observations.json"))?)?;
    let slot = AnnStore::new(bundle.to_path_buf())
        .load(&capture.meta.key())?
        .ok_or("captured ANN slot is missing or invalid")?;
    Ok((capture, AnnIndex::load(&slot)?))
}
