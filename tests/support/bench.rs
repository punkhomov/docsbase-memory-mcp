//! Search benchmark harness: fixture schema, loader, validator (T1 + DELTA-1).
//!
//! Offline gate foundation (FR-24…FR-28; NFR-5, NFR-6): typed fixtures,
//! content hashes, model-pin parsing, adjudication gates (`reviewed_by`,
//! reason), lexical control for the semantic slice. No production code is
//! touched; the harness lives in `tests/**`.

// Shared by `search_bench` (offline) and `search_bench_live` (record smoke):
// each target uses a different subset, so per-target dead-code lints would
// fire on the remainder.
#![allow(dead_code)]

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::ops::RangeInclusive;
use std::path::{Component, Path};
use std::sync::Arc;

use anyhow::Context;
use serde::Deserialize;
use tantivy::tokenizer::{TokenStream, Tokenizer};

use docsbase_memory::config::Config;
use docsbase_memory::daemon::registry;
use docsbase_memory::daemon::tools::search_docs_with;
use docsbase_memory::hybrid::dense::DenseCache;
use docsbase_memory::hybrid::{HybridStages, Ranking, Reranker};
use docsbase_memory::index::job::run_full;
use docsbase_memory::index::tantivy_index::IndexHandle;
use docsbase_memory::index::tokenizer::IdentifierTokenizer;
use docsbase_memory::store::Db;
use docsbase_memory::store::models::{Project, ProjectStatus};
use docsbase_memory::store::repo;
use docsbase_memory::vector_cache::{Embedder, VectorCache};

/// Minimum cases per query class (FR-3).
pub const MIN_CASES_PER_SLICE: usize = 8;
/// Query `k` bounds (FR-24).
pub const MIN_QUERY_K: usize = 3;
/// Query `k` bounds (FR-24).
pub const MAX_QUERY_K: usize = 20;

/// Classes whose relevance judgments are non-obvious (FR-26).
const CRITICAL_CLASSES: &[&str] = &["semantic", "hard_negative"];
/// Semantic overlap ceiling (FR-27; calibratable, see DELTA-1 table).
pub const SEMANTIC_MAX_LEXICAL_JACCARD: f64 = 0.5;

/// 1-based inclusive line range. Wire form is the `[start, end]` array (FR-2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LineRange {
    pub start: u32,
    pub end: u32,
}

impl<'de> Deserialize<'de> for LineRange {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let (start, end) = <(u32, u32)>::deserialize(deserializer)?;
        Ok(Self { start, end })
    }
}

impl serde::Serialize for LineRange {
    /// Mirror of the `[start, end]` wire form above.
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        (self.start, self.end).serialize(serializer)
    }
}

/// One relevant document for a case.
#[derive(Debug, Clone, Deserialize, serde::Serialize)]
pub struct ExpectedHit {
    pub path: String,
    pub lines: Option<LineRange>,
    /// 1 = acceptable, 2 = primary answer (FR-2).
    pub grade: u8,
}

/// One document that must not appear in top-k (FR-10, FR-26).
#[derive(Debug, Clone, Deserialize, serde::Serialize)]
pub struct AbsentHit {
    pub path: String,
    pub lines: Option<LineRange>,
    /// Why the document is a trap (adjudication evidence; required non-empty).
    #[serde(default)]
    pub reason: String,
}

/// One benchmark case.
#[derive(Debug, Clone, Deserialize, serde::Serialize)]
#[allow(dead_code)] // query/source consumed by runner (T7) and gate (T10); notes by T6 provenance
pub struct QueryCase {
    pub id: String,
    pub query: String,
    pub class: String,
    pub k: usize,
    pub expected: Vec<ExpectedHit>,
    pub absent: Vec<AbsentHit>,
    pub source: String,
    pub notes: String,
    /// Adjudicators (FR-26); ≥2 required for critical classes and
    /// absent-bearing cases.
    #[serde(default)]
    pub reviewed_by: Vec<String>,
}

/// `queries.json` is a bare array of cases.
#[derive(Debug, Clone, Deserialize, serde::Serialize)]
pub struct QueryFile(pub Vec<QueryCase>);

/// One frozen corpus document (FR-1).
#[derive(Debug, Clone, Deserialize, serde::Serialize)]
#[allow(dead_code)] // origin/license/frozen_at consumed by provenance checks (T2+)
pub struct ManifestDoc {
    pub path: String,
    pub origin: String,
    pub license: String,
    pub frozen_at: String,
    pub blake3: String,
}

/// `corpus/MANIFEST.json`: the frozen corpus inventory.
#[derive(Debug, Clone, Deserialize, serde::Serialize)]
pub struct CorpusManifest {
    pub documents: Vec<ManifestDoc>,
}

/// Pinned model identity for one fixture set (FR-25).
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)] // Rerank variant + parser consumed by replay stages (T7)
pub enum ModelPin {
    Embedding {
        provider: String,
        model: String,
        dim: u32,
        normalize: bool,
    },
    Rerank {
        provider: String,
        model: String,
    },
}

/// Parses `provider/model@d<dim>@<l2|raw>` (embedding) or `provider/model`
/// (rerank) into a [`ModelPin`].
#[allow(dead_code)] // consumed by replay stages (T7)
pub fn parse_model_pin(raw: &str) -> anyhow::Result<ModelPin> {
    if let Some((head, dim_norm)) = raw.split_once("@d") {
        let (provider, model) = head
            .split_once('/')
            .ok_or_else(|| anyhow::anyhow!("model pin {raw:?} needs provider/model"))?;
        let (dim_raw, norm_raw) = dim_norm
            .split_once('@')
            .ok_or_else(|| anyhow::anyhow!("model pin {raw:?} needs @d<dim>@<l2|raw>"))?;
        let dim: u32 = dim_raw
            .parse()
            .map_err(|_| anyhow::anyhow!("model pin {raw:?} has non-numeric dim"))?;
        let normalize = match norm_raw {
            "l2" => true,
            "raw" => false,
            other => anyhow::bail!("model pin {raw:?} needs l2|raw, got {other:?}"),
        };
        return Ok(ModelPin::Embedding {
            provider: provider.to_owned(),
            model: model.to_owned(),
            dim,
            normalize,
        });
    }
    if raw.contains('@') {
        anyhow::bail!("model pin {raw:?} is neither embedding nor rerank form");
    }
    let (provider, model) = raw
        .split_once('/')
        .ok_or_else(|| anyhow::anyhow!("model pin {raw:?} needs provider/model"))?;
    Ok(ModelPin::Rerank {
        provider: provider.to_owned(),
        model: model.to_owned(),
    })
}

/// Loaded fixture set: cases plus the corpus inventory and its hash.
/// `queries_hash` (design §5.2) belongs to the T10 baseline, which compares
/// canonical query sets; T1 only needs the corpus side.
#[derive(Debug)]
#[allow(dead_code)] // corpus_hash consumed by baseline/record (T10+)
pub struct Fixtures {
    pub cases: Vec<QueryCase>,
    pub manifest: CorpusManifest,
    pub corpus_hash: String,
}

impl Fixtures {
    /// Structural load only: parses JSON and hashes the corpus. Semantic
    /// checks are [`validate_queries`].
    pub fn load(root: &Path) -> anyhow::Result<Self> {
        let queries_raw = std::fs::read_to_string(root.join("queries.json"))
            .with_context(|| format!("read {}", root.join("queries.json").display()))?;
        let QueryFile(cases) = serde_json::from_str(&queries_raw)
            .with_context(|| "parse queries.json as an array of cases")?;
        let manifest_raw = std::fs::read_to_string(root.join("corpus").join("MANIFEST.json"))
            .with_context(|| "read corpus/MANIFEST.json")?;
        let manifest: CorpusManifest =
            serde_json::from_str(&manifest_raw).context("parse corpus/MANIFEST.json")?;
        let corpus_hash = corpus_hash(&root.join("corpus"))?;
        Ok(Self {
            cases,
            manifest,
            corpus_hash,
        })
    }
}

/// blake3 hex of one file's bytes.
pub fn blake3_file(path: &Path) -> anyhow::Result<String> {
    let bytes = std::fs::read(path).with_context(|| format!("read {}", path.display()))?;
    Ok(blake3_hash_bytes(&bytes))
}

fn blake3_hash_bytes(bytes: &[u8]) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(bytes);
    hasher.finalize().to_hex().to_string()
}

/// Canonical corpus hash: blake3 over the sorted `(relative_path,
/// blake3(file_bytes))` list of `.md` documents directly under `corpus_dir`
/// (`MANIFEST.json` itself is not a document and is excluded).
pub fn corpus_hash(corpus_dir: &Path) -> anyhow::Result<String> {
    let mut entries: BTreeMap<String, String> = BTreeMap::new();
    collect_markdown(corpus_dir, corpus_dir, &mut entries)?;
    let mut canonical = String::new();
    for (rel, hash) in &entries {
        canonical.push_str(rel);
        canonical.push('\n');
        canonical.push_str(hash);
        canonical.push('\n');
    }
    Ok(blake3_hash_bytes(canonical.as_bytes()))
}

fn collect_markdown(
    corpus_dir: &Path,
    dir: &Path,
    entries: &mut BTreeMap<String, String>,
) -> anyhow::Result<()> {
    let mut names: Vec<_> = std::fs::read_dir(dir)
        .with_context(|| format!("list {}", dir.display()))?
        .collect::<Result<_, _>>()
        .with_context(|| format!("list {}", dir.display()))?;
    names.sort_by_key(std::fs::DirEntry::file_name);
    for entry in names {
        let path = entry.path();
        if path.is_dir() {
            collect_markdown(corpus_dir, &path, entries)?;
        } else if path.extension().is_some_and(|ext| ext == "md") {
            let rel = path
                .strip_prefix(corpus_dir)
                .map_err(|_| anyhow::anyhow!("walk escaped {}", corpus_dir.display()))?
                .to_string_lossy()
                .replace('\\', "/");
            entries.insert(rel, blake3_file(&path)?);
        }
    }
    Ok(())
}

/// Jaccard similarity over index-tokenizer tokens (FR-27): the same
/// normalization, stemming and CJK bigrams the search sees. Sets, not
/// multisets: one canonical token counts once.
pub fn lexical_jaccard(query: &str, text: &str) -> f64 {
    let mut tokenizer = IdentifierTokenizer::default();
    let query_tokens = token_set(&mut tokenizer, query);
    let text_tokens = token_set(&mut tokenizer, text);
    let union = query_tokens.union(&text_tokens).count();
    if union == 0 {
        return 1.0;
    }
    #[allow(clippy::cast_precision_loss)] // counts are tiny; ratio only
    let score = query_tokens.intersection(&text_tokens).count() as f64 / union as f64;
    score
}

fn token_set(tokenizer: &mut IdentifierTokenizer, text: &str) -> BTreeSet<String> {
    let mut stream = tokenizer.token_stream(text);
    let mut tokens = BTreeSet::new();
    while let Some(token) = stream.next() {
        tokens.insert(token.text.clone());
    }
    tokens
}

/// Fail-closed validation (FR-24, FR-26, FR-27): every structural defect is
/// an error naming the case or path. Checks run most-specific-first so a case
/// with several defects reports the sharpest one; slice minimums run last.
/// Corpus/query size bounds (design §5.1) belong to T6 `fixtures_are_valid`
/// — chunk counts need the T2 index env, so they cannot live here.
pub fn validate_queries(
    cases: &[QueryCase],
    corpus: &Path,
    required_classes: &[&str],
) -> anyhow::Result<()> {
    let manifest: CorpusManifest = serde_json::from_str(
        &std::fs::read_to_string(corpus.join("MANIFEST.json"))
            .with_context(|| format!("read {}", corpus.join("MANIFEST.json").display()))?,
    )
    .context("parse corpus/MANIFEST.json")?;
    check_ids_classes_k(cases, required_classes)?;
    check_grades(cases)?;
    check_notes_reviewers(cases)?;
    check_absent_reasons(cases)?;
    check_paths(cases, &manifest, corpus)?;
    check_manifest_hashes(&manifest, corpus)?;
    check_no_overlap(cases)?;
    check_lines(cases, corpus)?;
    check_slice_minimums(cases, required_classes)?;
    Ok(())
}

fn check_ids_classes_k(cases: &[QueryCase], required_classes: &[&str]) -> anyhow::Result<()> {
    let mut seen_ids = BTreeSet::new();
    for case in cases {
        if !seen_ids.insert(case.id.as_str()) {
            anyhow::bail!("duplicate case id {:?}", case.id);
        }
        if !required_classes.contains(&case.class.as_str()) {
            anyhow::bail!("case {:?}: unknown class {:?}", case.id, case.class);
        }
        if !(MIN_QUERY_K..=MAX_QUERY_K).contains(&case.k) {
            anyhow::bail!(
                "case {:?}: k={} outside {MIN_QUERY_K}..={MAX_QUERY_K}",
                case.id,
                case.k
            );
        }
    }
    Ok(())
}

fn check_grades(cases: &[QueryCase]) -> anyhow::Result<()> {
    for case in cases {
        for hit in &case.expected {
            if !(1..=2).contains(&hit.grade) {
                anyhow::bail!(
                    "case {:?}: grade={} outside 1..=2 for {:?}",
                    case.id,
                    hit.grade,
                    hit.path
                );
            }
        }
    }
    Ok(())
}

/// FR-2/FR-26: critical classes and absent-bearing cases need ≥2 reviewers;
/// every other case needs ≥1; grade-1 or critical cases need a rationale in
/// `notes`.
fn check_notes_reviewers(cases: &[QueryCase]) -> anyhow::Result<()> {
    for case in cases {
        let critical = CRITICAL_CLASSES.contains(&case.class.as_str());
        if (critical || !case.absent.is_empty()) && case.reviewed_by.len() < 2 {
            anyhow::bail!(
                "case {:?}: needs >=2 reviewers (semantic/hard_negative/absent), found {}",
                case.id,
                case.reviewed_by.len()
            );
        }
        if case.reviewed_by.is_empty() {
            anyhow::bail!(
                "case {:?}: needs >=1 reviewer (FR-2), found none",
                case.id,
            );
        }
        let needs_notes = critical || case.expected.iter().any(|hit| hit.grade == 1);
        if needs_notes && case.notes.trim().is_empty() {
            anyhow::bail!(
                "case {:?}: notes required (grade 1 or critical class)",
                case.id
            );
        }
    }
    Ok(())
}

/// FR-26: every trap documents why it is a trap; "not in expected" is not
/// evidence.
fn check_absent_reasons(cases: &[QueryCase]) -> anyhow::Result<()> {
    for case in cases {
        for hit in &case.absent {
            if hit.reason.trim().is_empty() {
                anyhow::bail!(
                    "case {:?}: absent {:?} needs a reason (FR-26)",
                    case.id,
                    hit.path
                );
            }
        }
    }
    Ok(())
}

fn check_paths(
    cases: &[QueryCase],
    manifest: &CorpusManifest,
    corpus_dir: &Path,
) -> anyhow::Result<()> {
    let manifest_paths: BTreeSet<&str> = manifest
        .documents
        .iter()
        .map(|doc| doc.path.as_str())
        .collect();
    for case in cases {
        for path in case
            .expected
            .iter()
            .map(|hit| hit.path.as_str())
            .chain(case.absent.iter().map(|hit| hit.path.as_str()))
        {
            reject_escape(&case.id, path)?;
            if !manifest_paths.contains(path) {
                anyhow::bail!("case {:?}: path {path:?} not in corpus", case.id);
            }
            if !corpus_dir.join(path).is_file() {
                anyhow::bail!("case {:?}: file {path:?} missing on disk", case.id);
            }
        }
    }
    Ok(())
}

fn check_manifest_hashes(manifest: &CorpusManifest, corpus_dir: &Path) -> anyhow::Result<()> {
    for doc in &manifest.documents {
        // Inventory paths are validated like case paths: MANIFEST.json is a
        // fixture file, and NFR-5 keeps every fixture path inside the root.
        reject_escape(&doc.path, &doc.path)?;
        let actual = blake3_file(&corpus_dir.join(&doc.path))?;
        if actual != doc.blake3 {
            anyhow::bail!(
                "hash mismatch for {:?}: manifest {} != file {actual}",
                doc.path,
                doc.blake3
            );
        }
    }
    Ok(())
}

fn check_no_overlap(cases: &[QueryCase]) -> anyhow::Result<()> {
    for case in cases {
        let expected: BTreeSet<&str> = case.expected.iter().map(|hit| hit.path.as_str()).collect();
        for hit in &case.absent {
            if expected.contains(hit.path.as_str()) {
                anyhow::bail!(
                    "case {:?}: expected/absent overlap on {:?}",
                    case.id,
                    hit.path
                );
            }
        }
    }
    Ok(())
}

fn check_lines(cases: &[QueryCase], corpus: &Path) -> anyhow::Result<()> {
    for case in cases {
        for (path, lines, role) in case
            .expected
            .iter()
            .map(|hit| (&hit.path, &hit.lines, "expected"))
            .chain(
                case.absent
                    .iter()
                    .map(|hit| (&hit.path, &hit.lines, "absent")),
            )
        {
            if let Some(range) = lines {
                let count = u32::try_from(count_lines(&corpus.join(path))?)
                    .map_err(|_| anyhow::anyhow!("{path:?}: absurd line count"))?;
                if !(1..=count).contains(&range.start)
                    || range.start > range.end
                    || range.end > count
                {
                    anyhow::bail!(
                        "case {:?}: {role} lines {}-{} outside 1..={count} for {path:?}",
                        case.id,
                        range.start,
                        range.end
                    );
                }
            }
        }
        // FR-27: every semantic expected hit carries lines — the Jaccard is
        // measured on the expected span, not the whole document.
        if case.class == "semantic" {
            for hit in &case.expected {
                let range = hit.lines.ok_or_else(|| {
                    anyhow::anyhow!(
                        "case {:?}: semantic expected {:?} needs lines",
                        case.id,
                        hit.path
                    )
                })?;
                let span = span_text(corpus, &hit.path, range)?;
                let score = lexical_jaccard(&case.query, &span);
                if score > SEMANTIC_MAX_LEXICAL_JACCARD {
                    anyhow::bail!(
                        "case {:?}: lexical_jaccard {score:.3} > {SEMANTIC_MAX_LEXICAL_JACCARD} for {:?} (semantic must not share lexicon)",
                        case.id,
                        hit.path
                    );
                }
            }
        }
    }
    Ok(())
}

fn span_text(corpus: &Path, rel: &str, range: LineRange) -> anyhow::Result<String> {
    let text = std::fs::read_to_string(corpus.join(rel))
        .with_context(|| format!("read {}", corpus.join(rel).display()))?;
    let lines: Vec<&str> = text.lines().collect();
    let start = (range.start as usize).saturating_sub(1);
    let end = (range.end as usize).min(lines.len());
    Ok(lines.get(start..end).unwrap_or(&[]).join("\n"))
}

fn check_slice_minimums(cases: &[QueryCase], required_classes: &[&str]) -> anyhow::Result<()> {
    let mut per_class: BTreeMap<&str, usize> = BTreeMap::new();
    for case in cases {
        *per_class.entry(case.class.as_str()).or_default() += 1;
    }
    let mut thin: Vec<String> = Vec::new();
    for class in required_classes {
        if per_class.get(class).copied().unwrap_or(0) < MIN_CASES_PER_SLICE {
            thin.push((*class).to_owned());
        }
    }
    if !thin.is_empty() {
        anyhow::bail!(
            "slices below minimum {MIN_CASES_PER_SLICE}: {}",
            thin.join(", ")
        );
    }
    Ok(())
}

fn reject_escape(id: &str, path: &str) -> anyhow::Result<()> {
    if Path::new(path).components().any(|component| {
        matches!(
            component,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        )
    }) {
        anyhow::bail!("case {id:?}: path {path:?} escapes corpus");
    }
    Ok(())
}

fn count_lines(path: &Path) -> anyhow::Result<usize> {
    let text = std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    Ok(text.lines().count())
}

/// One embedding pin and one rerank pin per run at most (FR-25): pins of the
/// same variant must be identical; mixing variants is the one allowed pair.
pub fn validate_model_pins(pins: &[ModelPin]) -> anyhow::Result<()> {
    let mut embedding: Option<&ModelPin> = None;
    let mut rerank: Option<&ModelPin> = None;
    for pin in pins {
        let slot = match pin {
            ModelPin::Embedding { .. } => &mut embedding,
            ModelPin::Rerank { .. } => &mut rerank,
        };
        if let Some(first) = slot {
            if *first != pin {
                anyhow::bail!("mixed model pins in one run: {first:?} vs {pin:?}");
            }
        } else {
            *slot = Some(pin);
        }
    }
    Ok(())
}

/// Temporary indexed project (design §5.4). The tantivy handle is kept alive
/// for the env lifetime; chunk vectors are preloaded by T7.
pub struct BenchEnv {
    pub cache: tempfile::TempDir,
    /// Kept alive for the env lifetime: dropping `TempDir` deletes the corpus.
    #[allow(dead_code)]
    pub root: tempfile::TempDir,
    pub db: Db,
    pub project: Project,
    #[allow(dead_code)] // consumed by T7 preload_vectors
    pub chunk_hashes: Vec<(u64, String)>,
    #[allow(dead_code)] // consumed by T7 preload_vectors
    pub chunk_texts: Vec<String>,
    pub _index: IndexHandle,
}

/// Copies `corpus_dir` into a temp root without touching file bytes, opens a
/// `Db`, registers the project (current `SCHEMA_VERSION`, never hardcoded),
/// and runs a full index with `Config::default()`.
pub fn build_env(
    corpus_dir: &Path,
    name: &str,
    expected_docs: RangeInclusive<usize>,
) -> anyhow::Result<BenchEnv> {
    let cache = tempfile::TempDir::new().context("temp cache")?;
    let root = tempfile::TempDir::new().context("temp root")?;
    copy_tree(corpus_dir, root.path())?;
    let mut db = Db::open(cache.path()).context("open bench db")?;
    let canonical = root
        .path()
        .canonicalize()
        .context("canonicalize bench root")?;
    let project = registry::ensure_project(&mut db, &canonical).context("register project")?;
    let index_dir = cache
        .path()
        .join("projects")
        .join(project.id.to_string())
        .join("tantivy");
    let mut index = IndexHandle::open_or_create(&index_dir).context("open bench index")?;
    let stats = run_full(&mut db, &mut index, &project, &Config::default())
        .context("index bench corpus")?;
    if !expected_docs.contains(&stats.docs) {
        anyhow::bail!(
            "{name}: expected {} docs, indexed {}",
            format_range(&expected_docs),
            stats.docs
        );
    }
    let chunk_hashes = repo::chunk_hashes(db.connection(), project.id)?;
    let chunk_texts = repo::chunk_texts(db.connection(), project.id)?;
    if chunk_hashes.is_empty() || chunk_hashes.len() != chunk_texts.len() {
        anyhow::bail!(
            "{name}: chunk join broken ({} hashes vs {} texts)",
            chunk_hashes.len(),
            chunk_texts.len()
        );
    }
    let mut project = project;
    project.status = ProjectStatus::Indexed;
    Ok(BenchEnv {
        cache,
        root,
        db,
        project,
        chunk_hashes,
        chunk_texts,
        _index: index,
    })
}

fn format_range(range: &RangeInclusive<usize>) -> String {
    format!("{}..={}", range.start(), range.end())
}

fn copy_tree(src: &Path, dst: &Path) -> anyhow::Result<()> {
    for entry in std::fs::read_dir(src).with_context(|| format!("list {}", src.display()))? {
        let entry = entry?;
        let target = dst.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            std::fs::create_dir_all(&target)?;
            copy_tree(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}

/// One legacy-parity case report: `hit`/`absent_hit` must match the frozen
/// old baseline (FR-16); `ndcg` is diagnostic only. Single-relevant nDCG.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[allow(dead_code)] // per-case fields consumed by the T8/T10 report
pub struct LegacyCaseReport {
    pub id: String,
    pub query: String,
    pub hit: bool,
    pub absent_hit: bool,
    pub ndcg: f64,
}

/// Legacy parity outcome over the exact old corpus copy.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct LegacyParity {
    pub cases: Vec<LegacyCaseReport>,
    pub matches: bool,
}

#[derive(Debug, serde::Deserialize)]
struct OldBaselineCase {
    query: String,
    class: String,
    hit: bool,
    absent_hit: bool,
}

#[derive(Debug, serde::Deserialize)]
struct OldBaseline {
    cases: Vec<OldBaselineCase>,
}

/// Runs the 12 frozen legacy cases (Bm25, `k=3`) on a byte-identical copy of
/// the old corpus and compares `hit`/`absent_hit` with the frozen
/// `search-quality/baseline.json` (FR-16, SC-6).
pub fn run_legacy_parity() -> anyhow::Result<LegacyParity> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/search-bench");
    let env = build_env(&root.join("legacy-corpus"), "search-bench-legacy", 10..=10)?;
    let queries_raw =
        std::fs::read_to_string(root.join("legacy.json")).context("read legacy.json")?;
    let QueryFile(cases) = serde_json::from_str(&queries_raw).context("parse legacy.json")?;
    let baseline_raw = std::fs::read_to_string(root.join("legacy-baseline.json"))
        .context("read legacy-baseline.json")?;
    let baseline: OldBaseline =
        serde_json::from_str(&baseline_raw).context("parse legacy-baseline.json")?;
    let mut reports = Vec::with_capacity(cases.len());
    let mut matches = true;
    for case in &cases {
        let value = search_docs_with(
            &HybridStages::default(),
            &DenseCache::new(),
            &env.db,
            env.cache.path(),
            &env.project,
            &serde_json::json!({ "query": case.query, "limit": case.k }),
            RERANK_WINDOW,
        )
        .with_context(|| format!("legacy search for {:?}", case.id))?;
        let paths: Vec<&str> = value
            .as_array()
            .ok_or_else(|| anyhow::anyhow!("legacy search returned no array"))?
            .iter()
            .filter_map(|hit| hit.get("path")?.as_str())
            .collect();
        let expected: Vec<&str> = case.expected.iter().map(|hit| hit.path.as_str()).collect();
        let absent: Vec<&str> = case.absent.iter().map(|hit| hit.path.as_str()).collect();
        let rank = paths.iter().position(|path| expected.contains(path));
        let hit = rank.is_some();
        let absent_hit = paths.iter().any(|path| absent.contains(path));
        let ndcg = rank.map_or(0.0, |r| discount(r + 1)); // table, never libm (design §5.6)
        let old = baseline
            .cases
            .iter()
            .find(|old| old.query == case.query && old.class == case.class)
            .ok_or_else(|| {
                anyhow::anyhow!("legacy baseline has no {:?} / {:?}", case.query, case.class)
            })?;
        if old.hit != hit || old.absent_hit != absent_hit {
            matches = false;
        }
        reports.push(LegacyCaseReport {
            id: case.id.clone(),
            query: case.query.clone(),
            hit,
            absent_hit,
            ndcg,
        });
    }
    Ok(LegacyParity {
        cases: reports,
        matches,
    })
}

/// Rerank window used when a configuration needs candidates beyond the
/// limit (design §5.1; matches the production default).
pub const RERANK_WINDOW: u32 = 50;

/// Which pipeline to run (design §5.5).
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
pub enum ConfigId {
    Bm25,
    Bm25Rerank,
    Rrf,
    RrfRerank,
}

/// Offline vector fixture: raw LE f32 vectors keyed by content hash (FR-5).
#[derive(Debug, Clone)]
pub struct VectorFixture {
    pub model_pin: String,
    pub dim: usize,
    pub corpus_hash: String,
    pub queries_hash: String,
    pub vectors: HashMap<String, Vec<f32>>,
}

#[derive(Debug, serde::Deserialize)]
struct VectorManifest {
    model_pin: String,
    dim: usize,
    #[allow(dead_code)]
    count: usize,
    corpus_hash: String,
    queries_hash: String,
    bin: String,
    entries: Vec<VectorEntry>,
}

#[derive(Debug, serde::Deserialize)]
struct VectorEntry {
    text_hash: String,
    offset_bytes: u64,
    len_floats: usize,
}

impl VectorFixture {
    /// Loads `manifest.json` + the referenced `.bin` from `dir`,
    /// validating offsets, lengths, finiteness and dim.
    pub fn load(dir: &Path) -> anyhow::Result<Self> {
        let raw = std::fs::read_to_string(dir.join("manifest.json"))
            .with_context(|| format!("read {}", dir.join("manifest.json").display()))?;
        let manifest: VectorManifest =
            serde_json::from_str(&raw).context("parse vector manifest")?;
        let bytes = std::fs::read(dir.join(&manifest.bin))
            .with_context(|| format!("read {}", dir.join(&manifest.bin).display()))?;
        let floats = bytes_to_f32_le(&bytes)?;
        let mut vectors = HashMap::with_capacity(manifest.entries.len());
        for entry in &manifest.entries {
            if entry.offset_bytes % 4 != 0 {
                anyhow::bail!(
                    "vector {} has misaligned offset {}",
                    entry.text_hash,
                    entry.offset_bytes
                );
            }
            let start = usize::try_from(entry.offset_bytes)
                .map_err(|_| anyhow::anyhow!("offset overflow for {}", entry.text_hash))?;
            let end = start
                .checked_add(
                    entry
                        .len_floats
                        .checked_mul(4)
                        .ok_or_else(|| anyhow::anyhow!("len overflow for {}", entry.text_hash))?,
                )
                .ok_or_else(|| anyhow::anyhow!("range overflow for {}", entry.text_hash))?;
            if end > floats.len() * 4 || entry.len_floats != manifest.dim {
                anyhow::bail!(
                    "vector {} out of bounds or wrong dim (want {}, manifest dim {})",
                    entry.text_hash,
                    entry.len_floats,
                    manifest.dim
                );
            }
            let start_f = start / 4;
            let vec: Vec<f32> = floats[start_f..start_f + entry.len_floats].to_vec();
            if vec.iter().any(|v| !v.is_finite()) {
                anyhow::bail!("vector {} is not finite", entry.text_hash);
            }
            vectors.insert(entry.text_hash.clone(), vec);
        }
        Ok(Self {
            model_pin: manifest.model_pin,
            dim: manifest.dim,
            corpus_hash: manifest.corpus_hash,
            queries_hash: manifest.queries_hash,
            vectors,
        })
    }

    /// Writes `.bin` (concatenated LE f32, entries sorted by hash) and then
    /// atomically replaces `manifest.json` (commit point: readers never see
    /// a manifest pointing at a half-written blob).
    pub fn write(&self, dir: &Path) -> anyhow::Result<()> {
        std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
        let mut entries: Vec<(&String, &Vec<f32>)> = self.vectors.iter().collect();
        entries.sort_by(|a, b| a.0.cmp(b.0));
        let mut bytes = Vec::new();
        let mut meta = Vec::new();
        for (hash, vec) in &entries {
            if vec.len() != self.dim {
                anyhow::bail!("vector {hash} has dim {}, want {}", vec.len(), self.dim);
            }
            if vec.iter().any(|v| !v.is_finite()) {
                anyhow::bail!("vector {hash} is not finite");
            }
            let offset = bytes.len();
            for value in *vec {
                bytes.extend_from_slice(&value.to_le_bytes());
            }
            meta.push(serde_json::json!({
                "text_hash": hash,
                "offset_bytes": offset,
                "len_floats": vec.len(),
            }));
        }
        let digest = blake3_hash_bytes(&bytes);
        let bin_name = format!("vectors-{digest}.bin");
        std::fs::write(dir.join(&bin_name), &bytes)
            .with_context(|| format!("write {}", dir.join(&bin_name).display()))?;
        let manifest = serde_json::json!({
            "model_pin": self.model_pin,
            "dim": self.dim,
            "count": entries.len(),
            "corpus_hash": self.corpus_hash,
            "queries_hash": self.queries_hash,
            "bin": bin_name,
            "entries": meta,
        });
        let tmp = dir.join("manifest.json.tmp");
        std::fs::write(&tmp, manifest.to_string())
            .with_context(|| format!("write {}", tmp.display()))?;
        atomic_replace(&tmp, &dir.join("manifest.json"))?;
        // Re-records with different content land under a new content-hashed
        // name; the manifest is the commit point, so anything it does not
        // reference is garbage (NFR-2 storage cap). Sweep it now, while the
        // new set is known-good.
        Self::sweep_vector_dir(dir, &bin_name)?;
        Ok(())
    }

    /// Removes unreferenced blobs from a vector fixture dir: any
    /// `vectors-*.bin` that is not the live one, plus leftover `*.tmp`
    /// files. Only runs after a successful manifest commit, so a failure
    /// mid-write never deletes the previous good set.
    fn sweep_vector_dir(dir: &Path, live_bin: &str) -> anyhow::Result<()> {
        let entries = std::fs::read_dir(dir).with_context(|| format!("list {}", dir.display()))?;
        for entry in entries {
            let entry = entry.with_context(|| format!("read entry of {}", dir.display()))?;
            let path = entry.path();
            let name = path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default();
            let extension = path.extension().and_then(|ext| ext.to_str()).unwrap_or("");
            let stale_bin = name.starts_with("vectors-")
                && extension.eq_ignore_ascii_case("bin")
                && name != live_bin;
            if stale_bin || extension.eq_ignore_ascii_case("tmp") {
                std::fs::remove_file(&path)
                    .with_context(|| format!("remove {}", path.display()))?;
            }
        }
        Ok(())
    }

    /// Looks up one text by its content hash; missing hash is a fail-closed
    /// error naming the key (FR-7), never a silent skip.
    pub fn get(&self, text: &str) -> anyhow::Result<&[f32]> {
        let hash = blake3_hash_bytes(text.as_bytes());
        self.get_by_hash(&hash)
            .with_context(|| format!("no vector for text {text:?} (model {})", self.model_pin))
    }

    /// Looks up one pre-hashed text (used by [`preload_vectors`]).
    pub fn get_by_hash(&self, text_hash: &str) -> anyhow::Result<&[f32]> {
        self.vectors
            .get(text_hash)
            .map(Vec::as_slice)
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "no vector for text_hash {text_hash} (model {})",
                    self.model_pin
                )
            })
    }
}

fn bytes_to_f32_le(bytes: &[u8]) -> anyhow::Result<Vec<f32>> {
    if bytes.len() % 4 != 0 {
        anyhow::bail!("vector blob length {} is not a multiple of 4", bytes.len());
    }
    Ok(bytes
        .chunks_exact(4)
        .map(|chunk| f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
        .collect())
}

/// Offline rerank fixture: recorded scores keyed by the exact candidate set
/// (FR-6). A replayed query must present byte-identical candidates.
#[derive(Debug, Clone)]
pub struct RerankFixture {
    pub model_pin: String,
    pub pools: HashMap<RerankKey, RerankPool>,
}

/// Exact candidate set address: query hash + ordered candidate hashes.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RerankKey {
    pub query_hash: String,
    pub candidate_hashes: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct RerankPool {
    pub top_n: usize,
    pub results: Vec<(usize, f32)>,
    pub usage: Option<Usage>,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct Usage {
    #[serde(default)]
    pub tokens: Option<u64>,
    #[serde(default)]
    pub search_units: Option<u64>,
}

#[derive(Debug, serde::Deserialize)]
struct RawRerankFile {
    model_pin: String,
    pools: Vec<RawRerankPool>,
}

#[derive(Debug, serde::Deserialize)]
struct RawRerankPool {
    query_hash: String,
    top_n: usize,
    candidate_hashes: Vec<String>,
    results: Vec<RawRerankResult>,
    #[serde(default)]
    usage: Option<Usage>,
}

#[derive(Debug, serde::Deserialize)]
struct RawRerankResult {
    index: usize,
    score: f32,
}

impl RerankFixture {
    /// Loads and validates one rerank JSON (`top_n` within the candidate set,
    /// no duplicate pools).
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let raw =
            std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
        let file: RawRerankFile = serde_json::from_str(&raw).context("parse rerank fixture")?;
        let mut pools = HashMap::with_capacity(file.pools.len());
        for pool in file.pools {
            if pool.top_n == 0 || pool.top_n > pool.candidate_hashes.len() {
                anyhow::bail!(
                    "rerank top_n {} outside 1..={} candidates (query {})",
                    pool.top_n,
                    pool.candidate_hashes.len(),
                    pool.query_hash
                );
            }
            for result in &pool.results {
                if result.index >= pool.candidate_hashes.len() {
                    anyhow::bail!(
                        "rerank result index {} out of range for {} candidates (query {})",
                        result.index,
                        pool.candidate_hashes.len(),
                        pool.query_hash
                    );
                }
                if !result.score.is_finite() {
                    anyhow::bail!("rerank score is not finite (query {})", pool.query_hash);
                }
            }
            let key = RerankKey {
                query_hash: pool.query_hash.clone(),
                candidate_hashes: pool.candidate_hashes.clone(),
            };
            if pools.contains_key(&key) {
                anyhow::bail!("duplicate rerank pool for query {}", key.query_hash);
            }
            pools.insert(
                key,
                RerankPool {
                    top_n: pool.top_n,
                    results: pool
                        .results
                        .into_iter()
                        .map(|result| (result.index, result.score))
                        .collect(),
                    usage: pool.usage,
                },
            );
        }
        Ok(Self {
            model_pin: file.model_pin,
            pools,
        })
    }

    /// Writes the fixture atomically (temp file + rename).
    pub fn write(&self, path: &Path) -> anyhow::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create {}", parent.display()))?;
        }
        let mut pools: Vec<_> = self.pools.iter().collect();
        pools.sort_by(|a, b| {
            a.0.query_hash
                .cmp(&b.0.query_hash)
                .then_with(|| a.0.candidate_hashes.cmp(&b.0.candidate_hashes))
        });
        let pools_json: Vec<serde_json::Value> = pools
            .iter()
            .map(|(key, pool)| {
                serde_json::json!({
                    "query_hash": key.query_hash,
                    "top_n": pool.top_n,
                    "candidate_hashes": key.candidate_hashes,
                    "results": pool.results.iter().map(|(index, score)| {
                        serde_json::json!({"index": index, "score": score})
                    }).collect::<Vec<_>>(),
                    "usage": pool.usage.as_ref().map(|usage| {
                        serde_json::json!({"tokens": usage.tokens, "search_units": usage.search_units})
                    }),
                })
            })
            .collect();
        let file = serde_json::json!({"model_pin": self.model_pin, "pools": pools_json});
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, file.to_string())
            .with_context(|| format!("write {}", tmp.display()))?;
        atomic_replace(&tmp, path)?;
        Ok(())
    }

    /// Finds the pool for the exact candidate set; any deviation (order,
    /// membership, count) is a fail-closed error, never a stale-score reuse.
    pub fn pool(&self, query: &str, docs: &[String]) -> anyhow::Result<&RerankPool> {
        let key = RerankKey {
            query_hash: blake3_hash_bytes(query.as_bytes()),
            candidate_hashes: docs
                .iter()
                .map(|doc| blake3_hash_bytes(doc.as_bytes()))
                .collect(),
        };
        self.pools.get(&key).ok_or_else(|| {
            anyhow::anyhow!(
                "no rerank pool for query {} with {} candidates (model {})",
                key.query_hash,
                key.candidate_hashes.len(),
                self.model_pin
            )
        })
    }
}

/// [`Embedder`] replaying recorded vectors (FR-5).
#[derive(Debug, Clone)]
pub struct ReplayEmbedder {
    pub pin: String,
    pub fixture: Arc<VectorFixture>,
}

impl Embedder for ReplayEmbedder {
    fn model_id(&self) -> &str {
        &self.pin
    }

    fn embed(&self, texts: &[String]) -> docsbase_memory::error::Result<Vec<Vec<f32>>> {
        texts
            .iter()
            .map(|text| {
                self.fixture.get(text).map(<[f32]>::to_vec).map_err(|err| {
                    docsbase_memory::error::Error::internal(format!("replay embed: {err}"))
                })
            })
            .collect()
    }
}

/// [`Reranker`] replaying recorded scores (FR-6).
#[derive(Debug, Clone)]
pub struct ReplayReranker {
    pub fixture: Arc<RerankFixture>,
}

impl Reranker for ReplayReranker {
    fn rerank(
        &self,
        query: &str,
        docs: &[String],
        top_n: usize,
    ) -> docsbase_memory::error::Result<Vec<(usize, f32)>> {
        let pool = self.fixture.pool(query, docs).map_err(|err| {
            docsbase_memory::error::Error::internal(format!("replay rerank: {err}"))
        })?;
        if pool.top_n != top_n {
            return Err(docsbase_memory::error::Error::internal(format!(
                "rerank top_n {top_n} != recorded {} (query {})",
                pool.top_n,
                blake3_hash_bytes(query.as_bytes())
            )));
        }
        for (index, _) in &pool.results {
            if *index >= docs.len() {
                return Err(docsbase_memory::error::Error::internal(format!(
                    "rerank result index {index} out of range for {} candidates",
                    docs.len()
                )));
            }
        }
        Ok(pool.results.clone())
    }
}

/// Builds [`HybridStages`] for one harness configuration (design §5.5).
#[must_use]
pub fn stages_for(
    cfg: ConfigId,
    embedder: Option<Arc<dyn Embedder>>,
    reranker: Option<Arc<dyn Reranker>>,
) -> HybridStages {
    match cfg {
        ConfigId::Bm25 => HybridStages::default(),
        ConfigId::Bm25Rerank => HybridStages {
            embedder: None,
            reranker,
            ranking: Ranking::Lexical,
        },
        ConfigId::Rrf => HybridStages {
            embedder,
            reranker: None,
            ranking: Ranking::Rrf,
        },
        ConfigId::RrfRerank => HybridStages {
            embedder,
            reranker,
            ranking: Ranking::Rrf,
        },
    }
}

/// One citation as returned by `search_docs` (design §5.6).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[allow(dead_code)] // chunk_id/heading_path/lines read by T8 metrics + T10 report
pub struct Citation {
    pub chunk_id: u64,
    pub path: String,
    pub heading_path: String,
    pub lines: (u32, u32),
}

/// One case run: the ranked citations for a case id.
#[derive(Debug, Clone)]
pub struct ConfigCaseRun {
    #[allow(dead_code)] // id consumed by T8 metrics/T10 report
    pub id: String,
    pub citations: Vec<Citation>,
}

/// One configuration run over all cases.
#[derive(Debug, Clone)]
pub struct ConfigRun {
    pub cases: Vec<ConfigCaseRun>,
}

/// Runs one configuration over all cases through [`search_docs_with`]:
/// one warm-up call (discarded), then every case with its own `k`.
/// A fresh [`DenseCache`] per run keeps the cold-scan measurement honest.
pub fn run_config(
    env: &BenchEnv,
    cases: &[QueryCase],
    cfg: ConfigId,
    stages: &HybridStages,
) -> anyhow::Result<ConfigRun> {
    run_config_timed(env, cases, cfg, stages).map(|(run, _latencies_ms)| run)
}

/// Same as [`run_config`] plus per-call `search_docs_with` latency in
/// milliseconds (warm-up excluded; one sample per case, call order).
/// T10 needs the samples for the per-config p50/p95 latency report (FR-22).
pub fn run_config_timed(
    env: &BenchEnv,
    cases: &[QueryCase],
    cfg: ConfigId,
    stages: &HybridStages,
) -> anyhow::Result<(ConfigRun, Vec<f64>)> {
    let dense = DenseCache::new();
    // Warm-up call (discarded): warms the tantivy reader and the dense
    // scan. Errors surface on the first real case below with case context,
    // which diagnoses better than failing the warm-up.
    let first = cases
        .first()
        .ok_or_else(|| anyhow::anyhow!("run_config needs at least one case"))?;
    let _ = search_docs_with(
        stages,
        &dense,
        &env.db,
        env.cache.path(),
        &env.project,
        &serde_json::json!({"query": first.query, "limit": first.k}),
        RERANK_WINDOW,
    );
    let mut out = Vec::with_capacity(cases.len());
    let mut latencies_ms = Vec::with_capacity(cases.len());
    for case in cases {
        let started = std::time::Instant::now();
        let value = search_docs_with(
            stages,
            &dense,
            &env.db,
            env.cache.path(),
            &env.project,
            &serde_json::json!({"query": case.query, "limit": case.k}),
            RERANK_WINDOW,
        )
        .with_context(|| format!("run {:?} for case {:?}", cfg, case.id))?;
        latencies_ms.push(started.elapsed().as_secs_f64() * 1000.0);
        out.push(ConfigCaseRun {
            id: case.id.clone(),
            citations: parse_citations(&value, &case.id)?,
        });
    }
    Ok((ConfigRun { cases: out }, latencies_ms))
}

fn parse_citations(value: &serde_json::Value, case_id: &str) -> anyhow::Result<Vec<Citation>> {
    let hits = value
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("case {case_id}: search did not return an array"))?;
    hits.iter()
        .map(|hit| {
            let path = hit
                .get("path")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| anyhow::anyhow!("case {case_id}: citation without path"))?
                .to_owned();
            let heading_path = hit
                .get("heading_path")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_owned();
            let lines = hit.get("lines").and_then(|lines| {
                let array = lines.as_array()?;
                if array.len() != 2 {
                    return None;
                }
                let start = u32::try_from(array[0].as_u64()?).ok()?;
                let end = u32::try_from(array[1].as_u64()?).ok()?;
                Some((start, end))
            });
            let lines =
                lines.ok_or_else(|| anyhow::anyhow!("case {case_id}: citation without lines"))?;
            let chunk_id = hit
                .get("chunk_id")
                .and_then(serde_json::Value::as_u64)
                .ok_or_else(|| anyhow::anyhow!("case {case_id}: citation without chunk_id"))?;
            Ok(Citation {
                chunk_id,
                path,
                heading_path,
                lines,
            })
        })
        .collect()
}

/// Fills the [`VectorCache`] with the fixture vectors for every chunk of the
/// project (missing hash is a fail-closed error, never a partial cache).
pub fn preload_vectors(env: &BenchEnv, fixture: &VectorFixture) -> anyhow::Result<()> {
    let mut cache = VectorCache::open(env.cache.path()).context("open vector cache")?;
    for (_chunk_id, hash) in repo::chunk_hashes(env.db.connection(), env.project.id)? {
        let vector = fixture.get_by_hash(&hash)?;
        cache.put(&fixture.model_pin, &hash, vector)?;
    }
    Ok(())
}

/// Atomic file replace via temp file + rename (FR-19): readers never see a
/// half-written fixture. Both fixture writers commit through this.
pub fn atomic_replace(tmp: &Path, dst: &Path) -> anyhow::Result<()> {
    std::fs::rename(tmp, dst)
        .with_context(|| format!("replace {} with {}", dst.display(), tmp.display()))
}

/// Test-only reranker that records the exact candidate lists production
/// sends it (design §5.10: the T9 stand-in for T11's live capture).
/// An empty candidate list is a fail-closed error, never silent.
pub struct CollectingReranker {
    pub seen: std::sync::Mutex<Vec<(String, Vec<String>)>>,
}

impl docsbase_memory::hybrid::Reranker for CollectingReranker {
    fn rerank(
        &self,
        query: &str,
        docs: &[String],
        _top_n: usize,
    ) -> docsbase_memory::error::Result<Vec<(usize, f32)>> {
        if docs.is_empty() {
            return Err(docsbase_memory::error::Error::internal(
                "collecting reranker saw an empty candidate list",
            ));
        }
        self.seen
            .lock()
            .map_err(|_| {
                docsbase_memory::error::Error::internal("collecting reranker lock poisoned")
            })?
            .push((query.to_owned(), docs.to_vec()));
        Ok((0..docs.len()).map(|index| (index, 1.0)).collect())
    }
}

/// Checked-in nDCG discounts `1 / log2(rank + 1)` for ranks 1..=20
/// (design §5.1: k is capped at 20, so the table suffices). Literals match
/// the spec verbatim for bit-identical output on every OS; no `log2` call
/// exists anywhere near this path. `LOG10_2` is bit-identical to the
/// design's literal at that position. The 17-digit form is intentional
/// (spec-verbatim), hence the second allow.
#[allow(clippy::unreadable_literal, clippy::excessive_precision)]
pub const NDCG_DISCOUNT: [f64; 20] = [
    1.0,
    0.63092975357145753,
    0.5,
    0.43067655807339306,
    0.38685280723454163,
    0.35620718710802218,
    0.33333333333333331,
    0.31546487678572877,
    std::f64::consts::LOG10_2,
    0.28906482631788788,
    0.27894294565112987,
    0.27023815442731974,
    0.26264953503719357,
    0.2559580248098155,
    0.25,
    0.24465054211822604,
    0.23981246656813146,
    0.23540891336663824,
    0.23137821315975915,
    0.22767024869695299,
];

fn discount(rank_1based: usize) -> f64 {
    NDCG_DISCOUNT
        .get(rank_1based.saturating_sub(1))
        .copied()
        .unwrap_or(0.0)
}

/// Per-case metrics (design §5.6, FR-8).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CaseMetrics {
    pub hit: bool,
    pub recall: f64,
    pub mrr: f64,
    pub ndcg: f64,
    pub citation_overlap: f64,
    pub forbidden_hit: bool,
    pub duplicate_path_ratio: f64,
}

/// Early cutoff at 3 from the same ranked list (design §5.7: the @10 view
/// is the full [`CaseMetrics`], so it is not duplicated here). Field names
/// mirror the spec verbatim.
#[allow(clippy::struct_field_names)]
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct EarlyMetrics {
    pub hit_at_3: bool,
    pub recall_at_3: f64,
    pub ndcg_at_3: f64,
}

/// One slice aggregate: macro-average over its cases (design §5.7).
#[derive(Debug, Clone, PartialEq, Default, serde::Serialize, serde::Deserialize)]
pub struct SliceReport {
    pub cases: usize,
    pub hit_rate: f64,
    pub recall: f64,
    pub mrr: f64,
    pub ndcg: f64,
    pub citation_overlap: f64,
    pub forbidden_rate: f64,
    pub duplicate_path_ratio: f64,
}

/// Baseline-vs-current delta (design §5.7; computed in the gate, T10).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct DeltaReport {
    pub ndcg_delta: f64,
    pub hit_rate_delta: f64,
    pub wins: usize,
    pub losses: usize,
    pub ties: usize,
    pub ndcg_ci95: (f64, f64),
}

/// Hit pattern of one case across configurations (design §5.7, FR-31).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum CasePattern {
    AllHit,
    AllMiss,
    Mixed,
}

/// Per-slice outcome counts (design §5.7).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub struct OutcomeCounts {
    pub all_hit: usize,
    pub all_miss: usize,
    pub mixed: usize,
}

/// Outcome matrix by slice (design §5.7, FR-31).
#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub struct OutcomeMatrix {
    pub by_slice: BTreeMap<String, OutcomeCounts>,
}

/// Classifies one case's hit pattern. Empty patterns carry no signal and
/// count as misses.
#[must_use]
pub fn classify_case(hits: &[bool]) -> CasePattern {
    if hits.iter().all(|&hit| hit) && !hits.is_empty() {
        CasePattern::AllHit
    } else if hits.iter().any(|&hit| hit) {
        CasePattern::Mixed
    } else {
        CasePattern::AllMiss
    }
}

/// Tallies per-case patterns by slice (FR-31).
#[must_use]
pub fn outcome_matrix(patterns: &[(&str, CasePattern)]) -> OutcomeMatrix {
    let mut matrix = OutcomeMatrix::default();
    for (slice, pattern) in patterns {
        let counts = matrix.by_slice.entry((*slice).to_owned()).or_default();
        match pattern {
            CasePattern::AllHit => counts.all_hit += 1,
            CasePattern::AllMiss => counts.all_miss += 1,
            CasePattern::Mixed => counts.mixed += 1,
        }
    }
    matrix
}

/// Paired bootstrap CI of per-case deltas with a fixed seed
/// (design §5.7): deterministic across runs and platforms. Percentiles use
/// nearest-rank on 1000 resamples: index 24 ≈ p2.5, index 974 ≈ p97.5.
/// Counts are tiny (< 10^6 test cases), so `usize as f64` is exact here.
#[allow(clippy::cast_precision_loss)]
pub fn bootstrap_ci(deltas: &[f64]) -> (f64, f64) {
    const RESAMPLES: usize = 1000;
    const SEED: u64 = 0x9E37_79B9_7F4A_7C15;
    if deltas.is_empty() {
        return (0.0, 0.0);
    }
    let mut rng = XorShift64(SEED);
    let n = deltas.len();
    let mut means = Vec::with_capacity(RESAMPLES);
    for _ in 0..RESAMPLES {
        let mut sum = 0.0;
        for _ in 0..n {
            #[allow(clippy::cast_possible_truncation)]
            let draw = (rng.next() % n as u64) as usize;
            sum += deltas[draw];
        }
        means.push(sum / n as f64);
    }
    means.sort_by(f64::total_cmp);
    let low = means[RESAMPLES * 25 / 1000 - 1];
    let high = means[(RESAMPLES * 975 / 1000 - 1).min(RESAMPLES - 1)];
    (low, high)
}

struct XorShift64(u64);

impl XorShift64 {
    /// Canonical xorshift64* (Vigna): 12/25/27 shifts plus the multiplier.
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
}

/// Delta between two paired per-case metric lists (T10 gate input).
/// `tolerance` is the DELTA-1 win/loss band (FR-14).
pub fn delta_report(
    baseline_ndcg: &[f64],
    current_ndcg: &[f64],
    baseline_hit_rate: f64,
    current_hit_rate: f64,
    tolerance: f64,
) -> anyhow::Result<DeltaReport> {
    if baseline_ndcg.len() != current_ndcg.len() {
        anyhow::bail!(
            "delta needs paired nDCG lists, got {} vs {}",
            baseline_ndcg.len(),
            current_ndcg.len()
        );
    }
    let mut wins = 0;
    let mut losses = 0;
    let mut ties = 0;
    let mut deltas = Vec::with_capacity(baseline_ndcg.len());
    for (base, current) in baseline_ndcg.iter().zip(current_ndcg.iter()) {
        let delta = current - base;
        deltas.push(delta);
        if delta > tolerance {
            wins += 1;
        } else if delta < -tolerance {
            losses += 1;
        } else {
            ties += 1;
        }
    }
    let current_mean = mean(current_ndcg);
    let base_mean = mean(baseline_ndcg);
    Ok(DeltaReport {
        ndcg_delta: current_mean - base_mean,
        hit_rate_delta: current_hit_rate - baseline_hit_rate,
        wins,
        losses,
        ties,
        ndcg_ci95: bootstrap_ci(&deltas),
    })
}

/// Mean of a slice; empty input yields 0.0. Counts are tiny, so the
/// `usize as f64` conversion below is exact.
#[allow(clippy::cast_precision_loss)]
fn mean(values: &[f64]) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    values.iter().sum::<f64>() / values.len() as f64
}

/// Macro-average of case metrics, grouped by slice (design §5.7).
/// Counts are tiny (< 10^6 cases), so `usize as f64` is exact here.
#[allow(clippy::cast_precision_loss)]
pub fn aggregate_slices<'a>(
    cases: impl IntoIterator<Item = (&'a str, &'a CaseMetrics)>,
) -> BTreeMap<String, SliceReport> {
    let mut by_class: BTreeMap<&str, Vec<&CaseMetrics>> = BTreeMap::new();
    for (class, metrics) in cases {
        by_class.entry(class).or_default().push(metrics);
    }
    by_class
        .into_iter()
        .map(|(class, metrics)| {
            let n = metrics.len() as f64;
            let sum =
                |field: fn(&CaseMetrics) -> f64| metrics.iter().map(|m| field(m)).sum::<f64>() / n;
            let forbidden = metrics.iter().filter(|m| m.forbidden_hit).count() as f64 / n;
            let hits = metrics.iter().filter(|m| m.hit).count() as f64 / n;
            (
                class.to_owned(),
                SliceReport {
                    cases: metrics.len(),
                    hit_rate: hits,
                    recall: sum(|m| m.recall),
                    mrr: sum(|m| m.mrr),
                    ndcg: sum(|m| m.ndcg),
                    citation_overlap: sum(|m| m.citation_overlap),
                    forbidden_rate: forbidden,
                    duplicate_path_ratio: sum(|m| m.duplicate_path_ratio),
                },
            )
        })
        .collect()
}

/// Evaluates one case over its top-k citations (design §5.6, FR-8…FR-10).
/// Every expected element counts at most once: a second chunk matching an
/// already-found expected doc adds no gain (duplicate-trap protection).
pub fn evaluate_case(hits: &[Citation], case: &QueryCase) -> CaseMetrics {
    evaluate_at(hits, case, case.k)
}

/// A citation matches an expected/absent hit when paths are equal and, if
/// the hit names lines, the intervals intersect (design §5.6). This single
/// predicate drives gain, recall, MRR, citation overlap and forbidden
/// detection alike: ranking metrics and citation precision share one notion
/// of "found", so a right-doc/wrong-chunk hit earns no gain but also never
/// counts as citation overlap.
fn matches_hit(path: &str, lines: Option<LineRange>, hit: &Citation) -> bool {
    hit.path == path && line_ranges_overlap(lines, hit.lines)
}

fn line_ranges_overlap(expected: Option<LineRange>, actual: (u32, u32)) -> bool {
    match expected {
        // No range recorded: path equality is the whole contract.
        None => true,
        // Inclusive on both ends: touching at one line counts.
        Some(range) => range.start.max(actual.0) <= range.end.min(actual.1),
    }
}

/// Internal worker: `evaluate_case` is `evaluate_at` with the case's own
/// `k`. Counts are tiny, so `usize as f64` conversions below are exact.
#[allow(clippy::cast_precision_loss)]
fn evaluate_at(hits: &[Citation], case: &QueryCase, k: usize) -> CaseMetrics {
    let hits: Vec<&Citation> = hits.iter().take(k).collect();
    let n = hits.len();
    let mut matched = vec![false; case.expected.len()];
    let mut dcg = 0.0;
    let mut first_rank: Option<usize> = None;
    let mut overlap = 0usize;
    let mut seen_paths = BTreeSet::new();
    let mut dupes = 0usize;
    for (position, hit) in hits.iter().enumerate() {
        let rank = position + 1;
        if !seen_paths.insert(hit.path.as_str()) {
            dupes += 1;
        }
        for (idx, expected) in case.expected.iter().enumerate() {
            if matched[idx] {
                continue;
            }
            if matches_hit(&expected.path, expected.lines, hit) {
                matched[idx] = true;
                let gain = 2f64.powi(i32::from(expected.grade)) - 1.0;
                dcg += gain * discount(rank);
                if expected.grade >= 1 && first_rank.is_none() {
                    first_rank = Some(rank);
                }
                break;
            }
        }
        if case
            .expected
            .iter()
            .any(|expected| matches_hit(&expected.path, expected.lines, hit))
        {
            overlap += 1;
        }
    }
    let found_relevant = matched
        .iter()
        .zip(case.expected.iter())
        .any(|(found, expected)| *found && expected.grade >= 1);
    let mut grades: Vec<u8> = case
        .expected
        .iter()
        .map(|expected| expected.grade)
        .collect();
    grades.sort_unstable_by(|a, b| b.cmp(a));
    let idcg: f64 = grades
        .iter()
        .enumerate()
        .map(|(position, grade)| (2f64.powi(i32::from(*grade)) - 1.0) * discount(position + 1))
        .sum();
    // DELTA-3: forbidden is a rank-1 property. A trap below rank 1 is the
    // reading agent's to ignore; only a trap as the top answer misleads.
    let forbidden_hit = hits.first().is_some_and(|hit| {
        case.absent
            .iter()
            .any(|trap| matches_hit(&trap.path, trap.lines, hit))
    });
    let recall = if case.expected.is_empty() {
        1.0
    } else {
        matched.iter().filter(|found| **found).count() as f64 / case.expected.len() as f64
    };
    CaseMetrics {
        hit: found_relevant,
        recall,
        mrr: first_rank.map_or(0.0, |rank| 1.0 / rank as f64),
        ndcg: if idcg == 0.0 { 1.0 } else { dcg / idcg },
        citation_overlap: if n == 0 {
            0.0
        } else {
            overlap as f64 / n as f64
        },
        forbidden_hit,
        duplicate_path_ratio: if n == 0 { 0.0 } else { dupes as f64 / n as f64 },
    }
}

/// Early cutoff at 3 from the same ranked list (design §5.7: the @10 view
/// is the full [`CaseMetrics`], so it is not duplicated here).
pub fn early_metrics(hits: &[Citation], case: &QueryCase) -> EarlyMetrics {
    let at_3 = evaluate_at(hits, case, 3);
    EarlyMetrics {
        hit_at_3: at_3.hit,
        recall_at_3: at_3.recall,
        ndcg_at_3: at_3.ndcg,
    }
}

/// Writes one vector blob: `.bin` first, then the manifest as the atomic
/// commit point (design §5.2). A crash leaves at most an orphan `.bin`
/// that no manifest references.
pub fn write_vector_fixture(fixture: &VectorFixture, dir: &Path) -> anyhow::Result<()> {
    fixture.write(dir)
}

/// Writes one rerank JSON atomically (design §5.3).
pub fn write_rerank_fixture(fixture: &RerankFixture, path: &Path) -> anyhow::Result<()> {
    fixture.write(path)
}

/// Records replayable fixtures (FR-19, design §5.10): embeds every unique
/// chunk + query text, writes the vector fixture, preloads the cache, runs
/// the `Bm25Rerank` and `RrfRerank` configs through a collecting rerank
/// stage to capture the exact candidate windows production sends, replays
/// them through `reranker` for real scores, and writes the rerank fixture.
/// Every write is atomic; a failure anywhere before the final commit leaves
/// the previous fixture set untouched and valid.
///
/// `rerank_model` is an explicit parameter because [`Reranker`] carries no
/// identity by design (scores are never cached, so there is no consumer).
#[allow(clippy::too_many_arguments)]
pub fn record_fixtures_with(
    env: &BenchEnv,
    cases: &[QueryCase],
    embedder: &Arc<dyn Embedder>,
    reranker: &dyn Reranker,
    rerank_model: &str,
    vector_out: &Path,
    rerank_out: &Path,
) -> anyhow::Result<()> {
    let mut texts: Vec<String> = repo::chunk_texts(env.db.connection(), env.project.id)?;
    for case in cases {
        if !texts.iter().any(|text| text == &case.query) {
            texts.push(case.query.clone());
        }
    }
    let mut fixture = VectorFixture {
        model_pin: embedder.model_id().to_owned(),
        dim: 0,
        corpus_hash: corpus_hash(env.root.path())?,
        queries_hash: blake3_hash_bytes(&serde_json::to_vec(cases)?),
        vectors: HashMap::new(),
    };
    let matrix = embedder
        .embed(&texts)
        .map_err(|err| anyhow::anyhow!("record embed: {err}"))?;
    if matrix.is_empty() {
        anyhow::bail!("record embed: empty result");
    }
    let dim = matrix
        .first()
        .map(Vec::len)
        .ok_or_else(|| anyhow::anyhow!("record embed: empty result"))?;
    fixture.dim = dim;
    for (text, vector) in texts.iter().zip(matrix) {
        if vector.len() != dim || vector.iter().any(|value| !value.is_finite()) {
            anyhow::bail!("record embed: bad vector for text hash");
        }
        fixture
            .vectors
            .insert(blake3_hash_bytes(text.as_bytes()), vector);
    }
    write_vector_fixture(&fixture, vector_out)?;
    preload_vectors(env, &fixture)?;
    // Observe through the just-recorded fixture, never through the live
    // embedder: provider inference is not bit-deterministic across calls,
    // so warm-up, observation and every future replay must share the exact
    // fixture vectors — otherwise pool keys drift and replay misses.
    let replay: Arc<dyn Embedder> = Arc::new(ReplayEmbedder {
        pin: fixture.model_pin.clone(),
        fixture: Arc::new(fixture),
    });
    let pools = observe_rerank_pools(env, cases, &replay, reranker, rerank_model)?;
    let rr = RerankFixture {
        model_pin: rerank_model.to_owned(),
        pools,
    };
    write_rerank_fixture(&rr, rerank_out)?;
    Ok(())
}

/// Free-tier pacing between provider calls (`OpenRouter` free routes allow
/// ~20 requests/minute; paid routes do not need it). Keyed off the `:free`
/// suffix so paid records keep full speed.
pub const FREE_MODEL_PACE: std::time::Duration = std::time::Duration::from_secs(4);

/// Observes the exact candidate windows both rerank configs send (FR-6) and
/// scores them through the live `reranker`. Deterministic in the fixture:
/// every call — warm-up, observation, future replay — embeds through the
/// same vectors, so pool keys are stable across runs and processes.
///
/// # Errors
/// Bails on run, collection or scoring failures.
pub fn observe_rerank_pools(
    env: &BenchEnv,
    cases: &[QueryCase],
    embedder: &Arc<dyn Embedder>,
    reranker: &dyn Reranker,
    rerank_model: &str,
) -> anyhow::Result<HashMap<RerankKey, RerankPool>> {
    let collector = Arc::new(CollectingReranker {
        seen: std::sync::Mutex::new(Vec::new()),
    });
    for cfg in [ConfigId::Bm25Rerank, ConfigId::RrfRerank] {
        let stages = stages_for(cfg, Some(embedder.clone()), Some(collector.clone()));
        run_config(env, cases, cfg, &stages)?;
    }
    let mut pools = HashMap::new();
    for (query, docs) in collector
        .seen
        .lock()
        .map_err(|_| anyhow::anyhow!("collector lock"))?
        .iter()
    {
        if rerank_model.contains(":free") {
            std::thread::sleep(FREE_MODEL_PACE);
        }
        let results = reranker
            .rerank(query, docs, docs.len())
            .map_err(|err| anyhow::anyhow!("record rerank: {err}"))?;
        let key = RerankKey {
            query_hash: blake3_hash_bytes(query.as_bytes()),
            candidate_hashes: docs
                .iter()
                .map(|doc| blake3_hash_bytes(doc.as_bytes()))
                .collect(),
        };
        pools.insert(
            key,
            RerankPool {
                top_n: docs.len(),
                results,
                usage: None,
            },
        );
    }
    Ok(pools)
}

// ---------------------------------------------------------------------------
// T10: baseline, gate, report (FR-12…FR-15, FR-20, FR-22, FR-28, FR-30,
// FR-31; NFR-1, NFR-3, NFR-8).
// ---------------------------------------------------------------------------

/// Harness version stamped into every baseline (FR-12).
pub const BENCH_VERSION: u32 = 1;
/// Fixture-schema version stamped into every baseline (FR-12).
pub const FIXTURE_VERSION: u32 = 1;
/// Per-case and per-slice nDCG drop tolerance (FR-14).
pub const NDCG_DROP_TOLERANCE: f64 = 0.02;
/// Float comparison epsilon: no-change must never false-fail (design §5.8).
pub const GATE_EPSILON: f64 = 1e-9;
/// Absolute latency ceiling in milliseconds (FR-22).
pub const MAX_LATENCY_MS: f64 = 500.0;
/// Saturated-case ceiling: at most 70% of cases may hit on all configs.
pub const SATURATED_RATIO_MAX: f64 = 0.70;
/// Minimum semantic-slice disagreements (direction-agnostic, FR-28).
pub const SEMANTIC_DISAGREEMENTS_MIN: usize = 3;
/// Minimum bm25 vs bm25+rerank disagreements (direction-agnostic, FR-28).
pub const RERANK_DISAGREEMENTS_MIN: usize = 1;

/// One case in a baseline/report: full ranked list plus metrics at k and 3.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CaseReport {
    pub id: String,
    pub class: String,
    pub k: usize,
    pub ranks: Vec<Citation>,
    pub metrics: CaseMetrics,
    pub early: EarlyMetrics,
}

/// Local search latency distribution in milliseconds (FR-22).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct LatencyReport {
    pub p50_ms: f64,
    pub p95_ms: f64,
}

/// One configuration in a baseline/report.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ConfigReport {
    pub cases: Vec<CaseReport>,
    pub slices: BTreeMap<String, SliceReport>,
    pub latency: LatencyReport,
}

/// Baseline self-check: the benchmark must discriminate (FR-28).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct DiscriminationReport {
    pub saturated_total: usize,
    pub saturated_ratio: f64,
    pub non_saturated_by_slice: BTreeMap<String, usize>,
    pub semantic_disagreements: usize,
    pub rerank_disagreements: usize,
    pub semantic_max_lexical_jaccard: f64,
    pub lexical_tokenizer_version: u32,
}

/// Frozen benchmark outcome: quality shape of one full run (FR-13).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Baseline {
    pub bench_version: u32,
    pub fixture_version: u32,
    pub corpus_hash: String,
    pub legacy_corpus_hash: String,
    pub queries_hash: String,
    pub model_pins: Vec<String>,
    pub generated_at: String,
    pub index_build_ms: f64,
    pub configs: BTreeMap<ConfigId, ConfigReport>,
    pub legacy_parity: LegacyParity,
    pub discrimination: DiscriminationReport,
    pub outcome_matrix: OutcomeMatrix,
    /// Failures the baseline run itself produced (DELTA-3): the gate fails
    /// only on failures NOT in this set (regressions), and reports fixed
    /// ones as improvements. A red baseline stays auditable, never silent.
    pub known_failures: Vec<Failure>,
}

/// Pareto point of one configuration (live only, FR-32).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ParetoPoint {
    pub config: ConfigId,
    pub quality: f64,
    pub cost_usd: f64,
    pub latency_p95_ms: f64,
    pub on_frontier: bool,
}

/// Quality-vs-cost frontier (live only, FR-32).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ParetoReport {
    pub points: Vec<ParetoPoint>,
}

/// Gate input: the current run in baseline shape plus comparison output.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Report {
    pub baseline: Baseline,
    pub deltas: BTreeMap<ConfigId, BTreeMap<String, DeltaReport>>,
    pub warnings: Vec<String>,
    pub discrimination: DiscriminationReport,
    pub outcome_matrix: OutcomeMatrix,
    pub pareto: Option<ParetoReport>,
    /// Trap placement diagnostics (DELTA-3): traps below rank 1 and unjudged
    /// top-1 pooling candidates. Attached explicitly via
    /// [`attach_trap_diagnostics`]; advisory only, never a gate input, so a
    /// green run still shows trap movement toward the top.
    #[serde(default)]
    pub trap_diagnostics: Vec<String>,
}

/// Machine-readable gate failure (NFR-8: every hard fail carries a reason).
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct Failure {
    pub config: Option<ConfigId>,
    pub slice: Option<String>,
    pub case_id: Option<String>,
    pub kind: FailureKind,
    pub expected: String,
    pub actual: String,
}

/// Gate failure kinds (FR-14 check list, design §5.8).
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum FailureKind {
    VersionMismatch,
    HashMismatch,
    ForbiddenHit,
    HitDrop,
    NdcgDrop,
    SliceHitRateDrop,
    SliceNdcgDrop,
    MissingConfig,
    CaseDrift,
    LegacyMismatch,
    Latency,
    Discrimination(String),
}

/// Inputs for [`build_baseline_like`] that are not derived from runs.
#[derive(Debug, Clone)]
pub struct ReportMeta {
    pub corpus_hash: String,
    pub legacy_corpus_hash: String,
    pub queries_hash: String,
    pub model_pins: Vec<String>,
    pub legacy_parity: LegacyParity,
    pub index_build_ms: f64,
}

impl ReportMeta {
    /// Stand-in for fake-stage unit tests: fixed hashes, empty parity.
    #[must_use]
    pub fn synthetic() -> Self {
        Self {
            corpus_hash: "synthetic".to_owned(),
            legacy_corpus_hash: "synthetic-legacy".to_owned(),
            queries_hash: "synthetic-queries".to_owned(),
            model_pins: vec!["test/fake@d4@l2".to_owned()],
            legacy_parity: LegacyParity {
                cases: vec![],
                matches: true,
            },
            index_build_ms: 0.0,
        }
    }

    /// Meta for a current run compared against an existing baseline: hashes
    /// and pins must reproduce the baseline, otherwise the gate fails.
    #[must_use]
    pub fn from_baseline(baseline: &Baseline) -> Self {
        Self {
            corpus_hash: baseline.corpus_hash.clone(),
            legacy_corpus_hash: baseline.legacy_corpus_hash.clone(),
            queries_hash: baseline.queries_hash.clone(),
            model_pins: baseline.model_pins.clone(),
            legacy_parity: baseline.legacy_parity.clone(),
            index_build_ms: 0.0,
        }
    }
}

/// Builds the current run in baseline shape from config runs (FR-13).
///
/// # Errors
/// Bails on empty latency samples, unknown case ids, or metric failures.
pub fn build_baseline_like(
    cases: &[QueryCase],
    runs: &[(ConfigId, ConfigRun, Vec<f64>)],
    meta: ReportMeta,
    semantic_jaccard_max: f64,
) -> anyhow::Result<Baseline> {
    let mut configs = BTreeMap::new();
    // Per-case hits across configs, in canonical order, for the outcome
    // matrix (FR-31) and the discrimination report (FR-28).
    let mut hits_by_case: BTreeMap<&str, Vec<bool>> = BTreeMap::new();
    for (cfg, run, latencies) in runs {
        if latencies.is_empty() {
            anyhow::bail!("config {cfg:?}: no latency samples");
        }
        let run_by_id: BTreeMap<&str, &ConfigCaseRun> = run
            .cases
            .iter()
            .map(|case_run| (case_run.id.as_str(), case_run))
            .collect();
        let mut reports = Vec::with_capacity(cases.len());
        for case in cases {
            let case_run = run_by_id
                .get(case.id.as_str())
                .ok_or_else(|| anyhow::anyhow!("config {cfg:?}: unknown case {:?}", case.id))?;
            let metrics = evaluate_case(&case_run.citations, case);
            let early = early_metrics(&case_run.citations, case);
            hits_by_case
                .entry(case.id.as_str())
                .or_default()
                .push(metrics.hit);
            reports.push(CaseReport {
                id: case.id.clone(),
                class: case.class.clone(),
                k: case.k,
                ranks: case_run.citations.clone(),
                metrics,
                early,
            });
        }
        let class_metrics: Vec<(&str, &CaseMetrics)> = cases
            .iter()
            .map(|case| {
                let report = reports
                    .iter()
                    .find(|report| report.id == case.id)
                    .expect("report built for every case above");
                (case.class.as_str(), &report.metrics)
            })
            .collect();
        let slices = aggregate_slices(class_metrics);
        let latency = LatencyReport {
            p50_ms: percentile(latencies, 0.5),
            p95_ms: percentile(latencies, 0.95),
        };
        configs.insert(
            *cfg,
            ConfigReport {
                cases: reports,
                slices,
                latency,
            },
        );
    }
    let patterns: Vec<(&str, CasePattern)> = cases
        .iter()
        .map(|case| {
            let hits: &[bool] = hits_by_case
                .get(case.id.as_str())
                .map_or(&[], Vec::as_slice);
            (case.class.as_str(), classify_case(hits))
        })
        .collect();
    let outcome_matrix = outcome_matrix(&patterns);
    let discrimination = compute_discrimination(&configs, cases, semantic_jaccard_max);
    Ok(Baseline {
        bench_version: BENCH_VERSION,
        fixture_version: FIXTURE_VERSION,
        corpus_hash: meta.corpus_hash,
        legacy_corpus_hash: meta.legacy_corpus_hash,
        queries_hash: meta.queries_hash,
        model_pins: meta.model_pins,
        generated_at: unix_timestamp(),
        index_build_ms: meta.index_build_ms,
        configs,
        legacy_parity: meta.legacy_parity,
        discrimination,
        outcome_matrix,
        // Filled by regenerate_from_fixtures via self-gate; direct builders
        // leave it empty (no known state to tolerate).
        known_failures: Vec::new(),
    })
}

/// Wraps a baseline-shaped current run with deltas against a baseline.
#[must_use]
pub fn build_report(current: &Baseline, baseline: Option<&Baseline>) -> Report {
    let mut deltas: BTreeMap<ConfigId, BTreeMap<String, DeltaReport>> = BTreeMap::new();
    let mut warnings = Vec::new();
    if let Some(base) = baseline {
        for (cfg, cur) in &current.configs {
            let Some(base_cfg) = base.configs.get(cfg) else {
                continue;
            };
            let mut by_slice = BTreeMap::new();
            for (slice, cur_slice) in &cur.slices {
                let Some(base_slice) = base_cfg.slices.get(slice) else {
                    continue;
                };
                match paired_ndcg(&base_cfg.cases, &cur.cases, slice) {
                    Some((base_ndcg, cur_ndcg)) => {
                        if let Ok(delta) = delta_report(
                            &base_ndcg,
                            &cur_ndcg,
                            base_slice.hit_rate,
                            cur_slice.hit_rate,
                            NDCG_DROP_TOLERANCE,
                        ) {
                            by_slice.insert(slice.clone(), delta);
                        }
                    }
                    None => warnings.push(format!(
                        "deltas unavailable for {cfg:?}/{slice}: case set drift"
                    )),
                }
            }
            deltas.insert(*cfg, by_slice);
        }
    } else {
        warnings.push("no baseline attached; deltas unavailable".to_owned());
    }
    Report {
        baseline: current.clone(),
        deltas,
        warnings,
        discrimination: current.discrimination.clone(),
        outcome_matrix: current.outcome_matrix.clone(),
        pareto: None,
        // Attached explicitly by callers with cases in hand
        // (`attach_trap_diagnostics`); never a default.
        trap_diagnostics: Vec::new(),
    }
}

/// Pairs per-case nDCG lists of one slice by case id (baseline, current).
fn paired_ndcg(
    base_cases: &[CaseReport],
    cur_cases: &[CaseReport],
    slice: &str,
) -> Option<(Vec<f64>, Vec<f64>)> {
    let base: BTreeMap<&str, f64> = base_cases
        .iter()
        .filter(|report| report.class == slice)
        .map(|report| (report.id.as_str(), report.metrics.ndcg))
        .collect();
    let mut base_list = Vec::new();
    let mut cur_list = Vec::new();
    for report in cur_cases.iter().filter(|report| report.class == slice) {
        let value = base.get(report.id.as_str())?;
        base_list.push(*value);
        cur_list.push(report.metrics.ndcg);
    }
    if base_list.is_empty() || base_list.len() != base.len() {
        return None;
    }
    Some((base_list, cur_list))
}

/// Linear-interpolation percentile over milliseconds (deterministic).
/// Counts are tiny (cases per config), so float casts below are exact.
#[allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss
)]
#[must_use]
pub fn percentile(samples: &[f64], q: f64) -> f64 {
    if samples.is_empty() {
        return 0.0;
    }
    let mut sorted: Vec<f64> = samples.to_vec();
    sorted.sort_by(f64::total_cmp);
    let rank = q.clamp(0.0, 1.0) * (sorted.len() - 1) as f64;
    let lower = rank.floor() as usize;
    let upper = rank.ceil() as usize;
    if lower == upper {
        sorted[lower]
    } else {
        sorted[lower] + (sorted[upper] - sorted[lower]) * (rank - lower as f64)
    }
}

/// Latency budget for one configuration (FR-22): baseline p95 × 1.5 + 5 ms,
/// capped at the absolute 500 ms ceiling.
#[must_use]
pub fn latency_allowed_ms(baseline_p95_ms: f64) -> f64 {
    (baseline_p95_ms * 1.5 + 5.0).min(MAX_LATENCY_MS)
}

fn unix_timestamp() -> String {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs().to_string())
        .unwrap_or_default()
}

/// Hard-fail gate: every [`FailureKind`] carries config/slice/case context
/// (FR-14, NFR-8). Improvements never fail; only regressions do.
#[must_use]
pub fn gate_failures(current: &Report, baseline: &Baseline) -> Vec<Failure> {
    let cur = &current.baseline;
    let mut out = version_failures(cur, baseline);
    for (cfg, base_cfg) in &baseline.configs {
        let Some(cur_cfg) = cur.configs.get(cfg) else {
            out.push(Failure {
                config: Some(*cfg),
                slice: None,
                case_id: None,
                kind: FailureKind::MissingConfig,
                expected: format!("config {cfg:?} present"),
                actual: format!("config {cfg:?} missing"),
            });
            continue;
        };
        out.extend(config_failures(*cfg, base_cfg, cur_cfg));
    }
    for cfg in cur.configs.keys() {
        if !baseline.configs.contains_key(cfg) {
            out.push(Failure {
                config: Some(*cfg),
                slice: None,
                case_id: None,
                kind: FailureKind::CaseDrift,
                expected: format!("config {cfg:?} absent (baseline set)"),
                actual: format!("config {cfg:?} added"),
            });
        }
    }
    out.extend(legacy_failures(&cur.legacy_parity, &baseline.legacy_parity));
    out
}

/// Version, hash and pin-set comparison (FR-14 checks 1-2).
fn version_failures(cur: &Baseline, baseline: &Baseline) -> Vec<Failure> {
    let mut out = Vec::new();
    if cur.bench_version != baseline.bench_version {
        out.push(Failure {
            config: None,
            slice: None,
            case_id: None,
            kind: FailureKind::VersionMismatch,
            expected: format!("bench_version {}", baseline.bench_version),
            actual: format!("bench_version {}", cur.bench_version),
        });
    }
    if cur.fixture_version != baseline.fixture_version {
        out.push(Failure {
            config: None,
            slice: None,
            case_id: None,
            kind: FailureKind::VersionMismatch,
            expected: format!("fixture_version {}", baseline.fixture_version),
            actual: format!("fixture_version {}", cur.fixture_version),
        });
    }
    for (label, base_hash, cur_hash) in [
        ("corpus_hash", &baseline.corpus_hash, &cur.corpus_hash),
        (
            "legacy_corpus_hash",
            &baseline.legacy_corpus_hash,
            &cur.legacy_corpus_hash,
        ),
        ("queries_hash", &baseline.queries_hash, &cur.queries_hash),
    ] {
        if base_hash != cur_hash {
            out.push(Failure {
                config: None,
                slice: None,
                case_id: None,
                kind: FailureKind::HashMismatch,
                expected: format!("{label} {base_hash}"),
                actual: format!("{label} {cur_hash}"),
            });
        }
    }
    let mut base_pins = baseline.model_pins.clone();
    let mut cur_pins = cur.model_pins.clone();
    base_pins.sort();
    cur_pins.sort();
    if base_pins != cur_pins {
        out.push(Failure {
            config: None,
            slice: None,
            case_id: None,
            kind: FailureKind::HashMismatch,
            expected: format!("model_pins {}", base_pins.join(",")),
            actual: format!("model_pins {}", cur_pins.join(",")),
        });
    }
    out
}

/// Per-config gate checks: case drift, forbidden hits, hit/nDCG drops,
/// slice regressions (FR-14 checks 3-6).
fn config_failures(cfg: ConfigId, base_cfg: &ConfigReport, cur_cfg: &ConfigReport) -> Vec<Failure> {
    let mut out = Vec::new();
    let base_ids: BTreeMap<&str, &CaseReport> = base_cfg
        .cases
        .iter()
        .map(|report| (report.id.as_str(), report))
        .collect();
    let cur_ids: BTreeMap<&str, &CaseReport> = cur_cfg
        .cases
        .iter()
        .map(|report| (report.id.as_str(), report))
        .collect();
    out.extend(case_drift_failures(cfg, &base_ids, &cur_ids));
    out.extend(case_metric_failures(cfg, &base_ids, &cur_ids));
    out.extend(slice_failures(cfg, base_cfg, cur_cfg));
    out
}

/// Missing/extra cases within one configuration.
fn case_drift_failures(
    cfg: ConfigId,
    base_ids: &BTreeMap<&str, &CaseReport>,
    cur_ids: &BTreeMap<&str, &CaseReport>,
) -> Vec<Failure> {
    let mut out = Vec::new();
    for id in base_ids.keys() {
        if !cur_ids.contains_key(id) {
            out.push(Failure {
                config: Some(cfg),
                slice: None,
                case_id: Some((*id).to_owned()),
                kind: FailureKind::CaseDrift,
                expected: format!("case {id} present"),
                actual: format!("case {id} missing"),
            });
        }
    }
    for id in cur_ids.keys() {
        if !base_ids.contains_key(id) {
            out.push(Failure {
                config: Some(cfg),
                slice: None,
                case_id: Some((*id).to_owned()),
                kind: FailureKind::CaseDrift,
                expected: format!("case {id} absent (baseline set)"),
                actual: format!("case {id} added"),
            });
        }
    }
    out
}

/// Forbidden hits and hit/nDCG regressions per case.
fn case_metric_failures(
    cfg: ConfigId,
    base_ids: &BTreeMap<&str, &CaseReport>,
    cur_ids: &BTreeMap<&str, &CaseReport>,
) -> Vec<Failure> {
    let mut out = Vec::new();
    for (id, base_case) in base_ids {
        let Some(cur_case) = cur_ids.get(id) else {
            continue;
        };
        if cur_case.metrics.forbidden_hit {
            out.push(Failure {
                config: Some(cfg),
                slice: Some(cur_case.class.clone()),
                case_id: Some((*id).to_owned()),
                kind: FailureKind::ForbiddenHit,
                expected: "no forbidden hit".to_owned(),
                actual: "forbidden_hit = true".to_owned(),
            });
        }
        if base_case.metrics.hit && !cur_case.metrics.hit {
            out.push(Failure {
                config: Some(cfg),
                slice: Some(cur_case.class.clone()),
                case_id: Some((*id).to_owned()),
                kind: FailureKind::HitDrop,
                expected: "hit = true".to_owned(),
                actual: "hit = false".to_owned(),
            });
        }
        if base_case.metrics.ndcg - cur_case.metrics.ndcg > NDCG_DROP_TOLERANCE + GATE_EPSILON {
            out.push(Failure {
                config: Some(cfg),
                slice: Some(cur_case.class.clone()),
                case_id: Some((*id).to_owned()),
                kind: FailureKind::NdcgDrop,
                expected: format!(
                    "nDCG >= {:.4}",
                    base_case.metrics.ndcg - NDCG_DROP_TOLERANCE
                ),
                actual: format!("nDCG = {:.4}", cur_case.metrics.ndcg),
            });
        }
    }
    out
}

/// Slice regressions, missing slices (class renames) and added slices.
fn slice_failures(cfg: ConfigId, base_cfg: &ConfigReport, cur_cfg: &ConfigReport) -> Vec<Failure> {
    let mut out = Vec::new();
    for (slice, base_slice) in &base_cfg.slices {
        let Some(cur_slice) = cur_cfg.slices.get(slice) else {
            // Same case ids under a renamed class: the case-set check
            // passes (ids match) but the slice vanished — that is drift,
            // and it must fail rather than silently drop the slice (and
            // its deltas).
            out.push(Failure {
                config: Some(cfg),
                slice: Some(slice.clone()),
                case_id: None,
                kind: FailureKind::CaseDrift,
                expected: format!("slice {slice} present"),
                actual: format!("slice {slice} missing"),
            });
            continue;
        };
        if base_slice.hit_rate - cur_slice.hit_rate > GATE_EPSILON {
            out.push(Failure {
                config: Some(cfg),
                slice: Some(slice.clone()),
                case_id: None,
                kind: FailureKind::SliceHitRateDrop,
                expected: format!("hit_rate >= {:.4}", base_slice.hit_rate),
                actual: format!("hit_rate = {:.4}", cur_slice.hit_rate),
            });
        }
        if base_slice.ndcg - cur_slice.ndcg > NDCG_DROP_TOLERANCE + GATE_EPSILON {
            out.push(Failure {
                config: Some(cfg),
                slice: Some(slice.clone()),
                case_id: None,
                kind: FailureKind::SliceNdcgDrop,
                expected: format!("nDCG >= {:.4}", base_slice.ndcg - NDCG_DROP_TOLERANCE),
                actual: format!("nDCG = {:.4}", cur_slice.ndcg),
            });
        }
    }
    for slice in cur_cfg.slices.keys() {
        if !base_cfg.slices.contains_key(slice) {
            out.push(Failure {
                config: Some(cfg),
                slice: Some(slice.clone()),
                case_id: None,
                kind: FailureKind::CaseDrift,
                expected: format!("slice {slice} absent (baseline set)"),
                actual: format!("slice {slice} added"),
            });
        }
    }
    out
}

/// Legacy parity comparison (FR-16): any `hit`/`absent_hit` divergence fails.
fn legacy_failures(current: &LegacyParity, baseline: &LegacyParity) -> Vec<Failure> {
    let mut out = Vec::new();
    if current.cases.len() != baseline.cases.len() {
        out.push(Failure {
            config: None,
            slice: Some("legacy".to_owned()),
            case_id: None,
            kind: FailureKind::LegacyMismatch,
            expected: format!("{} legacy cases", baseline.cases.len()),
            actual: format!("{} legacy cases", current.cases.len()),
        });
        return out;
    }
    for (base, cur) in baseline.cases.iter().zip(current.cases.iter()) {
        if base.id != cur.id || base.hit != cur.hit || base.absent_hit != cur.absent_hit {
            out.push(Failure {
                config: None,
                slice: Some("legacy".to_owned()),
                case_id: Some(cur.id.clone()),
                kind: FailureKind::LegacyMismatch,
                expected: format!("hit={} absent_hit={}", base.hit, base.absent_hit),
                actual: format!("hit={} absent_hit={}", cur.hit, cur.absent_hit),
            });
        }
    }
    if baseline.matches != current.matches {
        out.push(Failure {
            config: None,
            slice: Some("legacy".to_owned()),
            case_id: None,
            kind: FailureKind::LegacyMismatch,
            expected: format!("matches={}", baseline.matches),
            actual: format!("matches={}", current.matches),
        });
    }
    out
}

/// Latency gate (FR-22): pure comparison; the caller applies it on Linux
/// only (`cfg!(target_os = "linux")`), other hosts record and publish.
#[must_use]
pub fn latency_failures(current: &Report, baseline: &Baseline) -> Vec<Failure> {
    let mut out = Vec::new();
    for (cfg, cur_cfg) in &current.baseline.configs {
        let Some(base_cfg) = baseline.configs.get(cfg) else {
            continue;
        };
        let allowed = latency_allowed_ms(base_cfg.latency.p95_ms);
        if cur_cfg.latency.p95_ms > allowed + GATE_EPSILON {
            out.push(Failure {
                config: Some(*cfg),
                slice: None,
                case_id: None,
                kind: FailureKind::Latency,
                expected: format!("p95 <= {allowed:.2} ms"),
                actual: format!("p95 = {:.2} ms", cur_cfg.latency.p95_ms),
            });
        }
    }
    out
}

/// Computes the discrimination self-check from config reports (FR-28).
/// Case counts are tiny, so the float casts below are exact.
#[allow(clippy::cast_precision_loss)]
#[must_use]
pub fn compute_discrimination(
    configs: &BTreeMap<ConfigId, ConfigReport>,
    cases: &[QueryCase],
    semantic_jaccard_max: f64,
) -> DiscriminationReport {
    let hits: BTreeMap<&str, Vec<bool>> = cases
        .iter()
        .map(|case| {
            let per_config = [
                ConfigId::Bm25,
                ConfigId::Bm25Rerank,
                ConfigId::Rrf,
                ConfigId::RrfRerank,
            ]
            .iter()
            .filter_map(|cfg| {
                configs
                    .get(cfg)?
                    .cases
                    .iter()
                    .find(|report| report.id == case.id)
            })
            .map(|report| report.metrics.hit)
            .collect();
            (case.id.as_str(), per_config)
        })
        .collect();
    let saturated_total = hits
        .values()
        .filter(|list| classify_case(list) == CasePattern::AllHit)
        .count();
    let total = cases.len().max(1) as f64;
    // Every slice in the corpus gets an entry (zero when fully saturated)
    // so the gate can tell "saturated" from "unevaluated".
    let mut non_saturated_by_slice: BTreeMap<String, usize> = BTreeMap::new();
    for case in cases {
        non_saturated_by_slice
            .entry(case.class.clone())
            .or_default();
    }
    for case in cases {
        let saturated = hits
            .get(case.id.as_str())
            .is_some_and(|list| classify_case(list) == CasePattern::AllHit);
        if !saturated {
            *non_saturated_by_slice
                .entry(case.class.clone())
                .or_default() += 1;
        }
    }
    let semantic_disagreements = cases
        .iter()
        .filter(|case| case.class == "semantic")
        .filter(|case| {
            hits.get(case.id.as_str())
                .is_some_and(|list| !list.is_empty() && !list.iter().all(|hit| *hit == list[0]))
        })
        .count();
    let rerank_disagreements = cases
        .iter()
        .filter(|case| {
            let bm25 = configs
                .get(&ConfigId::Bm25)
                .and_then(|report| report.cases.iter().find(|r| r.id == case.id))
                .map(|r| r.metrics.hit);
            let reranked = configs
                .get(&ConfigId::Bm25Rerank)
                .and_then(|report| report.cases.iter().find(|r| r.id == case.id))
                .map(|r| r.metrics.hit);
            matches!((bm25, reranked), (Some(a), Some(b)) if a != b)
        })
        .count();
    DiscriminationReport {
        saturated_total,
        saturated_ratio: saturated_total as f64 / total,
        non_saturated_by_slice,
        semantic_disagreements,
        rerank_disagreements,
        semantic_max_lexical_jaccard: semantic_jaccard_max,
        lexical_tokenizer_version: docsbase_memory::index::tokenizer::TOKENIZER_VERSION,
    }
}

/// Validates the discrimination self-check (FR-28): a non-discriminating
/// baseline is rejected before it can be committed.
#[must_use]
pub fn discrimination_failures(disc: &DiscriminationReport) -> Vec<Failure> {
    let mut out = Vec::new();
    let mut rule = |name: &str, expected: String, actual: String| {
        out.push(Failure {
            config: None,
            slice: None,
            case_id: None,
            kind: FailureKind::Discrimination(name.to_owned()),
            expected,
            actual,
        });
    };
    if disc.saturated_ratio > SATURATED_RATIO_MAX + GATE_EPSILON {
        rule(
            "saturated_ratio",
            format!("saturated_ratio <= {SATURATED_RATIO_MAX}"),
            format!("saturated_ratio = {:.3}", disc.saturated_ratio),
        );
    }
    for (slice, count) in &disc.non_saturated_by_slice {
        if *count == 0 {
            rule(
                "slice_coverage",
                format!("slice {slice} has >= 1 non-saturated case"),
                format!("slice {slice} is fully saturated"),
            );
        }
    }
    // Slices missing from the map have zero non-saturated cases only when
    // the caller passes the full slice set (see below); a map that names no
    // slice at all is unconditionally wrong.
    if disc.non_saturated_by_slice.is_empty() {
        rule(
            "slice_coverage",
            "every slice has >= 1 non-saturated case".to_owned(),
            "no non-saturated cases at all".to_owned(),
        );
    }
    if disc.semantic_disagreements < SEMANTIC_DISAGREEMENTS_MIN {
        rule(
            "semantic_disagreement",
            format!("semantic_disagreements >= {SEMANTIC_DISAGREEMENTS_MIN}"),
            format!("semantic_disagreements = {}", disc.semantic_disagreements),
        );
    }
    if disc.rerank_disagreements < RERANK_DISAGREEMENTS_MIN {
        rule(
            "rerank_disagreement",
            format!("rerank_disagreements >= {RERANK_DISAGREEMENTS_MIN}"),
            format!("rerank_disagreements = {}", disc.rerank_disagreements),
        );
    }
    if disc.semantic_max_lexical_jaccard > SEMANTIC_MAX_LEXICAL_JACCARD + GATE_EPSILON {
        rule(
            "lexical_jaccard",
            format!("semantic_max_lexical_jaccard <= {SEMANTIC_MAX_LEXICAL_JACCARD}"),
            format!(
                "semantic_max_lexical_jaccard = {:.3}",
                disc.semantic_max_lexical_jaccard
            ),
        );
    }
    if disc.lexical_tokenizer_version != docsbase_memory::index::tokenizer::TOKENIZER_VERSION {
        rule(
            "tokenizer_version",
            format!(
                "lexical_tokenizer_version == {}",
                docsbase_memory::index::tokenizer::TOKENIZER_VERSION
            ),
            format!(
                "lexical_tokenizer_version = {}",
                disc.lexical_tokenizer_version
            ),
        );
    }
    out
}

/// Deterministic quality JSON: ranks, metrics, slices — no latency, no
/// timestamps (FR-12, NFR-3). Byte-identical across reruns and hosts.
#[must_use]
pub fn quality_json(report: &Report) -> String {
    let mut configs = serde_json::Map::new();
    for (cfg, config) in &report.baseline.configs {
        let cases: Vec<serde_json::Value> = config
            .cases
            .iter()
            .map(|case| {
                serde_json::json!({
                    "id": case.id,
                    "class": case.class,
                    "k": case.k,
                    "ranks": case.ranks,
                    "metrics": case.metrics,
                    "early": case.early,
                })
            })
            .collect();
        configs.insert(
            format!("{cfg:?}"),
            serde_json::json!({
                "cases": cases,
                "slices": config.slices,
            }),
        );
    }
    serde_json::json!({
        "bench_version": report.baseline.bench_version,
        "fixture_version": report.baseline.fixture_version,
        "corpus_hash": report.baseline.corpus_hash,
        "legacy_corpus_hash": report.baseline.legacy_corpus_hash,
        "queries_hash": report.baseline.queries_hash,
        "model_pins": report.baseline.model_pins,
        "configs": configs,
        "legacy_parity": report.baseline.legacy_parity,
        "outcome_matrix": report.outcome_matrix,
        "trap_diagnostics": report.trap_diagnostics,
        "discrimination": {
            "saturated_total": report.discrimination.saturated_total,
            "saturated_ratio": report.discrimination.saturated_ratio,
            "non_saturated_by_slice": report.discrimination.non_saturated_by_slice,
            "semantic_disagreements": report.discrimination.semantic_disagreements,
            "rerank_disagreements": report.discrimination.rerank_disagreements,
            "semantic_max_lexical_jaccard": report.discrimination.semantic_max_lexical_jaccard,
            "lexical_tokenizer_version": report.discrimination.lexical_tokenizer_version,
        },
    })
    .to_string()
}

/// Reads a frozen baseline (missing file is a hard fail, never a skip).
///
/// # Errors
/// Bails when the file is missing or unparsable (NFR-3).
pub fn load_baseline(path: &std::path::Path) -> anyhow::Result<Baseline> {
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("read baseline {}", path.display()))?;
    serde_json::from_str(&raw).context("parse baseline.json")
}

/// Writes a baseline atomically; when the quality part is identical the
/// file (and its mtime) is preserved, so unchanged regenerations stay
/// no-op commits (NFR-3, SC-9). `generated_at` and `index_build_ms` are
/// host-dependent and excluded from the comparison — but when the quality
/// part matches, the OLD values are kept (no timestamp churn).
///
/// # Errors
/// Bails on serialization or filesystem failures.
pub fn write_baseline(baseline: &Baseline, path: &std::path::Path) -> anyhow::Result<()> {
    let mut canonical = canonical_baseline_value(baseline)?;
    if let Ok(existing) = std::fs::read_to_string(path) {
        if let Ok(existing_value) = serde_json::from_str::<serde_json::Value>(&existing) {
            strip_volatile(&mut canonical);
            let mut existing_canonical = existing_value;
            strip_volatile(&mut existing_canonical);
            if existing_canonical == canonical {
                return Ok(());
            }
        }
    }
    let payload = serde_json::to_string_pretty(baseline).context("serialize baseline")?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("mkdir {}", parent.display()))?;
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, &payload).with_context(|| format!("write {}", tmp.display()))?;
    atomic_replace(&tmp, path)?;
    Ok(())
}

/// Canonical baseline JSON minus volatile fields.
fn canonical_baseline_value(baseline: &Baseline) -> anyhow::Result<serde_json::Value> {
    let mut value = serde_json::to_value(baseline).context("serialize baseline")?;
    strip_volatile(&mut value);
    Ok(value)
}

/// Removes host-dependent fields from a baseline JSON value in place.
fn strip_volatile(value: &mut serde_json::Value) {
    if let Some(map) = value.as_object_mut() {
        map.remove("generated_at");
        map.remove("index_build_ms");
    }
}

/// Directory-safe slug for a model pin: `openrouter/qwen@...` becomes a
/// single path component. T11 record paths and T10 replay must agree.
#[must_use]
pub fn pin_slug(pin: &str) -> String {
    pin.chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' {
                ch
            } else {
                '_'
            }
        })
        .collect()
}

/// Loaded replay stages plus the vector fixture they came from. The
/// fixture must be preloaded into the run env ([`preload_vectors`]):
/// chunk vectors come from the on-disk [`VectorCache`], only the query
/// vector comes from the [`ReplayEmbedder`] live call. Skipping the preload
/// silently degrades `Rrf` to `Bm25` — hence the explicit field.
pub struct ReplaySet {
    pub embedder: Option<Arc<dyn Embedder>>,
    pub reranker: Option<Arc<dyn Reranker>>,
    pub vectors: Option<VectorFixture>,
}

/// Loads replay stages for the pins declared in the baseline (FR-5, FR-21).
/// Missing fixture files are a hard fail, never a silent skip (NFR-3).
///
/// # Errors
/// Bails on missing/invalid fixtures or unparsable pins.
pub fn replay_stages(root: &std::path::Path, baseline: &Baseline) -> anyhow::Result<ReplaySet> {
    let mut embedder: Option<Arc<dyn Embedder>> = None;
    let mut reranker: Option<Arc<dyn Reranker>> = None;
    let mut vectors: Option<VectorFixture> = None;
    for pin in &baseline.model_pins {
        match parse_model_pin(pin)? {
            ModelPin::Embedding { .. } => {
                if embedder.is_some() {
                    anyhow::bail!("two embedding pins in baseline (second: {pin})");
                }
                let dir = root.join("vectors").join(pin_slug(pin));
                let fixture = VectorFixture::load(&dir).with_context(|| {
                    format!("missing vector fixture for pin {pin} (run record first)")
                })?;
                embedder = Some(Arc::new(ReplayEmbedder {
                    pin: pin.clone(),
                    fixture: Arc::new(fixture.clone()),
                }));
                vectors = Some(fixture);
            }
            ModelPin::Rerank { .. } => {
                if reranker.is_some() {
                    anyhow::bail!("two rerank pins in baseline (second: {pin})");
                }
                let path = root.join("rerank").join(format!("{}.json", pin_slug(pin)));
                let fixture = RerankFixture::load(&path).with_context(|| {
                    format!("missing rerank fixture for pin {pin} (run record first)")
                })?;
                reranker = Some(Arc::new(ReplayReranker {
                    fixture: Arc::new(fixture),
                }));
            }
        }
    }
    Ok(ReplaySet {
        embedder,
        reranker,
        vectors,
    })
}

/// Maximum FR-27 lexical Jaccard over semantic expected spans.
pub fn max_semantic_jaccard(
    cases: &[QueryCase],
    corpus_dir: &std::path::Path,
) -> anyhow::Result<f64> {
    let mut max: f64 = 0.0;
    for case in cases.iter().filter(|case| case.class == "semantic") {
        for expected in &case.expected {
            let Some(range) = expected.lines else {
                continue;
            };
            let span = span_text(corpus_dir, &expected.path, range)?;
            max = max.max(lexical_jaccard(&case.query, &span));
        }
    }
    Ok(max)
}

/// Regenerates the baseline from recorded fixtures: index the frozen corpus,
/// run all four configs through replay stages, attach legacy parity,
/// discrimination and hashes (FR-13, FR-15). Writes nothing; the ignored
/// `regenerate_baseline` test persists the result deliberately.
///
/// `accept_new_known` must be true to legalize fresh failures outside the
/// previous known set (blocker 4): without it, a regression smuggled into a
/// regenerate fails here instead of turning green. The bootstrap (no
/// previous `baseline.json`) accepts whatever the first run produces.
///
/// # Errors
/// Bails on ambiguous pins, missing fixtures, run failures, or unaccepted
/// known-set growth.
pub fn regenerate_from_fixtures(
    root: &std::path::Path,
    accept_new_known: bool,
) -> anyhow::Result<Baseline> {
    let fixtures = Fixtures::load(root).context("load fixtures")?;
    let embed_pins = discover_pins(&root.join("vectors"), true)?;
    let rerank_pins = discover_pins(&root.join("rerank"), false)?;
    if embed_pins.len() != 1 || rerank_pins.len() != 1 {
        anyhow::bail!(
            "need exactly one embedding + one rerank pin, found {embed_pins:?} / {rerank_pins:?}"
        );
    }
    let model_pins = vec![embed_pins[0].clone(), rerank_pins[0].clone()];
    let pseudo = Baseline {
        bench_version: BENCH_VERSION,
        fixture_version: FIXTURE_VERSION,
        corpus_hash: String::new(),
        legacy_corpus_hash: String::new(),
        queries_hash: String::new(),
        model_pins: model_pins.clone(),
        generated_at: String::new(),
        index_build_ms: 0.0,
        configs: BTreeMap::new(),
        legacy_parity: LegacyParity {
            cases: vec![],
            matches: true,
        },
        discrimination: DiscriminationReport {
            saturated_total: 0,
            saturated_ratio: 0.0,
            non_saturated_by_slice: BTreeMap::new(),
            semantic_disagreements: 0,
            rerank_disagreements: 0,
            semantic_max_lexical_jaccard: 0.0,
            lexical_tokenizer_version: docsbase_memory::index::tokenizer::TOKENIZER_VERSION,
        },
        outcome_matrix: OutcomeMatrix::default(),
        known_failures: Vec::new(),
    };
    let replay = replay_stages(root, &pseudo)?;
    let (embedder, reranker) = (replay.embedder, replay.reranker);
    let started = std::time::Instant::now();
    let env = build_env(&root.join("corpus"), "baseline", 60..=85).context("index corpus")?;
    // Chunk vectors live on disk: without the preload every dense config
    // silently degrades to its lexical base (the T11 pilot caught this).
    if let Some(fixture) = &replay.vectors {
        preload_vectors(&env, fixture)?;
    }
    let index_build_ms = started.elapsed().as_secs_f64() * 1000.0;
    let mut runs = Vec::new();
    for cfg in [
        ConfigId::Bm25,
        ConfigId::Bm25Rerank,
        ConfigId::Rrf,
        ConfigId::RrfRerank,
    ] {
        let stages = stages_for(cfg, embedder.clone(), reranker.clone());
        let (run, latencies) = run_config_timed(&env, &fixtures.cases, cfg, &stages)?;
        runs.push((cfg, run, latencies));
    }
    let legacy_parity = run_legacy_parity()?;
    let meta = ReportMeta {
        corpus_hash: fixtures.corpus_hash,
        legacy_corpus_hash: corpus_hash(&root.join("legacy-corpus"))?,
        queries_hash: blake3_file(&root.join("queries.json"))?,
        model_pins,
        legacy_parity,
        index_build_ms,
    };
    let jaccard = max_semantic_jaccard(&fixtures.cases, &root.join("corpus"))?;
    let mut baseline = build_baseline_like(&fixtures.cases, &runs, meta, jaccard)?;
    // Self-gate: the baseline records its own failures as known (DELTA-3).
    // Future runs fail only on NEW failures; fixed ones surface as
    // improvements. The file stays an honest snapshot, never forced green.
    let self_report = build_report(&baseline, None);
    let fresh_known = gate_failures(&self_report, &baseline);
    let old_known = load_baseline(&root.join("baseline.json"))
        .map(|previous| previous.known_failures)
        .unwrap_or_default();
    check_known_growth(&old_known, &fresh_known, accept_new_known)?;
    baseline.known_failures = fresh_known;
    Ok(baseline)
}

/// Splits current gate failures into regressions (not in the baseline's
/// known set) and fixes (known failures that no longer reproduce).
/// DELTA-3: the benchmark answers "worse or better", never forced green.
#[must_use]
pub fn gate_new_failures(current: &[Failure], known: &[Failure]) -> (Vec<Failure>, Vec<Failure>) {
    let known_set: std::collections::HashSet<&Failure> = known.iter().collect();
    let current_set: std::collections::HashSet<&Failure> = current.iter().collect();
    let mut new: Vec<Failure> = current
        .iter()
        .filter(|failure| !known_set.contains(failure))
        .cloned()
        .collect();
    let mut fixed: Vec<Failure> = known
        .iter()
        .filter(|failure| !current_set.contains(failure))
        .cloned()
        .collect();
    new.sort_by(|a, b| format!("{a:?}").cmp(&format!("{b:?}")));
    fixed.sort_by(|a, b| format!("{a:?}").cmp(&format!("{b:?}")));
    (new, fixed)
}

/// Regenerate guard (blocker 4, FR-15): fresh failures outside the old known
/// set are a laundering attempt until explicitly accepted. Without accept,
/// new failures fail loudly with their case ids; healed failures always
/// pass (DELTA-3 fix→regen). An empty old set is the first bootstrap — there
/// is nothing to compare against; reaching it by deleting `baseline.json`
/// is itself the visible destructive act.
pub fn check_known_growth(
    old_known: &[Failure],
    fresh_known: &[Failure],
    accept_new: bool,
) -> anyhow::Result<()> {
    if old_known.is_empty() {
        return Ok(());
    }
    let (new, _) = gate_new_failures(fresh_known, old_known);
    if new.is_empty() || accept_new {
        return Ok(());
    }
    anyhow::bail!(
        "regenerate refuses {} unaccepted new known-failure(s): {:?} (re-run with DOCSBASE_BENCH_ACCEPT_NEW_KNOWN=1 to legalize deliberately)",
        new.len(),
        new.iter()
            .map(|failure| failure.case_id.clone())
            .collect::<Vec<_>>()
    )
}

/// Lists recorded model pins: subdirectories of `vectors/`, `*.json` files
/// of `rerank/`. Returns the pin strings (read back from manifests).
fn discover_pins(dir: &std::path::Path, vector_side: bool) -> anyhow::Result<Vec<String>> {
    let mut pins = Vec::new();
    let entries = std::fs::read_dir(dir).with_context(|| format!("list {}", dir.display()))?;
    for entry in entries {
        let entry = entry.with_context(|| format!("read entry of {}", dir.display()))?;
        if vector_side {
            if entry.path().is_dir() {
                let manifest_raw = std::fs::read_to_string(entry.path().join("manifest.json"))
                    .with_context(|| {
                        format!("read {}", entry.path().join("manifest.json").display())
                    })?;
                let manifest: VectorManifest =
                    serde_json::from_str(&manifest_raw).context("parse vector manifest")?;
                pins.push(manifest.model_pin);
            }
        } else if entry.path().extension().is_some_and(|ext| ext == "json") {
            let fixture = RerankFixture::load(&entry.path())?;
            pins.push(fixture.model_pin);
        }
    }
    pins.sort();
    Ok(pins)
}

/// Trap placement diagnostics (DELTA-3): traps below rank 1 are the reading
/// agent's to ignore, but their positions are recorded for release review;
/// a rank-1 citation matching neither expected nor absent is an unjudged
/// top answer and a pooling candidate (FR-29). Informational only — the
/// gate fails solely on rank-1 traps (FR-10) and missing expected (FR-14).
#[must_use]
pub fn trap_warnings(current: &Baseline, cases: &[QueryCase]) -> Vec<String> {
    let by_id: std::collections::BTreeMap<&str, &QueryCase> =
        cases.iter().map(|case| (case.id.as_str(), case)).collect();
    let mut out = Vec::new();
    for (cfg, report) in &current.configs {
        for case_report in &report.cases {
            let Some(case) = by_id.get(case_report.id.as_str()) else {
                continue;
            };
            for (position, citation) in case_report.ranks.iter().enumerate() {
                let rank = position + 1;
                let is_trap = case
                    .absent
                    .iter()
                    .any(|trap| matches_hit(&trap.path, trap.lines, citation));
                if !is_trap {
                    continue;
                }
                if rank == 1 {
                    // The gate hard-fails known rank-1 traps too; tag them
                    // here so the diagnostic stream stays complete.
                    out.push(format!(
                        "trap {} at rank 1 (case {}, {cfg:?}) [known]",
                        citation.path, case.id,
                    ));
                    continue;
                }
                out.push(format!(
                    "trap {} at rank {rank} (case {}, {cfg:?})",
                    citation.path, case.id,
                ));
            }
            if let Some(top) = case_report.ranks.first() {
                let judged = case
                    .expected
                    .iter()
                    .any(|expected| matches_hit(&expected.path, expected.lines, top))
                    || case
                        .absent
                        .iter()
                        .any(|trap| matches_hit(&trap.path, trap.lines, top));
                if !judged {
                    out.push(format!(
                        "unjudged rank 1: {} (case {}, {cfg:?}) — pooling candidate",
                        top.path, case.id,
                    ));
                }
            }
        }
    }
    out.sort();
    out.dedup();
    out
}

/// Records [`trap_warnings`] into the report (DELTA-3): the diagnostics live
/// in the report artifact, not just in the failure message, so trap movement
/// stays visible on green runs. Advisory only — the gate never reads this.
pub fn attach_trap_diagnostics(report: &mut Report, cases: &[QueryCase]) {
    report.trap_diagnostics = trap_warnings(&report.baseline, cases);
}

// ---------------------------------------------------------------------------
// T13: live measurement + cost cap (FR-17, FR-18, FR-32; NFR-7).
// Everything network-shaped lives behind `hybrid-remote`; the math below
// is pure and unit-tested without a key.
// ---------------------------------------------------------------------------

/// Usage recorded from live provider responses (FR-18).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LiveUsage {
    /// Input tokens (embeddings) or 0 when the provider omits usage.
    pub prompt_tokens: u64,
    /// Output/total tokens where reported, else 0.
    pub total_tokens: u64,
    /// Provider search units (rerank) where reported, else 0.
    pub search_units: u64,
    /// Requests sent, always counted even when usage is absent.
    pub calls: u64,
}

/// Price list for cost estimates (FR-18). Unknown models return `None` and
/// the live run refuses to guess (fail-closed); add the model explicitly.
/// Token/call counts are tiny, so the float casts below are exact.
#[allow(clippy::cast_precision_loss)]
#[must_use]
pub fn estimate_cost_usd(model_pin: &str, usage: &LiveUsage) -> Option<f64> {
    // Embedding models are priced per million prompt tokens ...
    if model_pin.contains("nemotron-3-embed-1b:free") {
        return Some(0.0);
    }
    if model_pin.contains("qwen/qwen3-embedding-8b") {
        return Some(usage.prompt_tokens as f64 / 1_000_000.0 * 0.01);
    }
    // ... rerank models per call.
    // Rerank models are priced per call; only the free route is $0 —
    // a paid `nemotron-rerank` id must not match the free arm.
    if model_pin.contains("nemotron-rerank") && model_pin.contains(":free") {
        return Some(0.0);
    }
    if model_pin.contains("cohere/rerank") {
        return Some(usage.calls as f64 * 0.001);
    }
    None
}

/// Cost-cap source of truth (FR-18, NFR-7): `DOCSBASE_BENCH_COST_CAP_USD`,
/// defaulting to $2 with a warning when unset or unparsable. The cap is
/// mandatory: there is no uncapped mode.
#[must_use]
pub fn cost_cap_from_env() -> (f64, bool) {
    cost_cap_from_raw(std::env::var("DOCSBASE_BENCH_COST_CAP_USD").ok().as_deref())
}

/// Pure policy behind [`cost_cap_from_env`]: explicit finite non-negative
/// values pass through, anything else falls back to the $2 default with a
/// warning flag. Pure so the contract is unit-testable without touching the
/// process environment. Infinities are rejected: an unbounded cap would
/// silently disable the mandatory limit (FR-18).
#[must_use]
pub fn cost_cap_from_raw(raw: Option<&str>) -> (f64, bool) {
    match raw {
        Some(value) => match value.parse::<f64>() {
            Ok(cap) if cap.is_finite() && cap >= 0.0 => (cap, false),
            _ => (2.0, true),
        },
        None => (2.0, true),
    }
}

/// Refuses the next request when it would breach the cap (FR-18).
/// Pure so the policy is unit-testable without a network.
pub fn check_cost_cap(spent_usd: f64, next_estimate_usd: f64, cap_usd: f64) -> anyhow::Result<()> {
    if spent_usd + next_estimate_usd > cap_usd {
        anyhow::bail!(
            "cost cap exceeded: spent ${spent_usd:.4}, next ~${next_estimate_usd:.4}, cap ${cap_usd:.4}"
        );
    }
    Ok(())
}

/// Worst-case single-request cost used for the pre-flight cap check.
/// Flat bounds (not batch-scaled): backfill batches are few and bounded by
/// the client's own batching, so the marginal check stays conservative
/// without per-call accounting complexity.
#[must_use]
pub fn worst_case_next_usd(url: &str) -> f64 {
    if url.contains("/rerank") {
        0.002
    } else {
        0.005
    }
}

#[cfg(feature = "hybrid-remote")]
pub mod live_transport {
    use std::sync::{Arc, Mutex};

    use docsbase_memory::hybrid::wire::{Transport, TransportResponse};

    use super::{
        LiveUsage, check_cost_cap, cost_cap_from_env, estimate_cost_usd, worst_case_next_usd,
    };

    /// [`Transport`] wrapper that records usage from live provider responses
    /// and enforces the cost cap before every request (FR-17, FR-18).
    /// Parsing is defensive: unknown shapes count calls only, never fail.
    /// Cost is charged per response from the model pin; unknown pins fall
    /// back to the worst-case bound (fail toward blocking, never guessing).
    #[derive(Clone)]
    pub struct RecordingTransport {
        inner: Arc<dyn Transport>,
        model_pin: String,
        usage: Arc<Mutex<LiveUsage>>,
        spent_usd: Arc<Mutex<f64>>,
    }

    impl RecordingTransport {
        /// Wraps an inner transport with a fresh usage accumulator.
        pub fn new(inner: Arc<dyn Transport>, model_pin: &str) -> Self {
            Self {
                inner,
                model_pin: model_pin.to_owned(),
                usage: Arc::new(Mutex::new(LiveUsage::default())),
                spent_usd: Arc::new(Mutex::new(0.0)),
            }
        }

        /// Snapshot of recorded usage so far.
        #[must_use]
        pub fn usage(&self) -> LiveUsage {
            self.usage
                .lock()
                .map(|guard| guard.clone())
                .unwrap_or_default()
        }

        /// Dollars spent so far (estimated from recorded usage).
        #[must_use]
        pub fn spent_usd(&self) -> f64 {
            self.spent_usd
                .lock()
                .map(|guard| *guard)
                .unwrap_or_default()
        }
    }

    impl Transport for RecordingTransport {
        fn post_json(
            &self,
            url: &str,
            headers: &[(String, String)],
            body: &str,
        ) -> docsbase_memory::error::Result<TransportResponse> {
            let (cap, defaulted) = cost_cap_from_env();
            if defaulted {
                eprintln!("warning: DOCSBASE_BENCH_COST_CAP_USD unset, using default $2.00");
            }
            let spent = self.spent_usd();
            if let Err(err) = check_cost_cap(spent, worst_case_next_usd(url), cap) {
                return Err(docsbase_memory::error::Error::internal(format!(
                    "cost cap refuses the next request: {err}"
                )));
            }
            let response = self.inner.post_json(url, headers, body)?;
            if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&response.body) {
                let prompt = parsed
                    .pointer("/usage/prompt_tokens")
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(0);
                let total = parsed
                    .pointer("/usage/total_tokens")
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(0);
                let units = parsed
                    .pointer("/meta/billed_units/search_units")
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(0);
                let call = LiveUsage {
                    prompt_tokens: prompt,
                    total_tokens: total,
                    search_units: units,
                    calls: 1,
                };
                let charge = estimate_cost_usd(&self.model_pin, &call)
                    .unwrap_or_else(|| worst_case_next_usd(url));
                if let Ok(mut guard) = self.usage.lock() {
                    guard.calls += 1;
                    guard.prompt_tokens += prompt;
                    guard.total_tokens += total;
                    guard.search_units += units;
                }
                if let Ok(mut spent) = self.spent_usd.lock() {
                    *spent += charge;
                }
            }
            Ok(response)
        }
    }
}

/// Marks the Pareto frontier in place: a point is on it when no other point
/// has at least as much quality for at most as much cost and latency, with
/// at least one strict inequality (FR-32).
pub fn pareto_frontier(points: &mut [ParetoPoint]) {
    fn dominates(other: &ParetoPoint, point: &ParetoPoint) -> bool {
        other.quality >= point.quality
            && other.cost_usd <= point.cost_usd
            && other.latency_p95_ms <= point.latency_p95_ms
            && (other.quality > point.quality
                || other.cost_usd < point.cost_usd
                || other.latency_p95_ms < point.latency_p95_ms)
    }
    let dominated: Vec<bool> = points
        .iter()
        .map(|point| {
            points
                .iter()
                .any(|other| !std::ptr::eq(point, other) && dominates(other, point))
        })
        .collect();
    for (point, dominated) in points.iter_mut().zip(dominated) {
        point.on_frontier = !dominated;
    }
}

/// Pacing wrapper for rate-limited rerankers (free tiers): sleeps before a
/// call when the previous one was too recent. Interactive search never uses
/// this (retries cover the occasional 429); bulk jobs (record, live) do.
pub struct PacingReranker<R: Reranker> {
    inner: R,
    min_interval: std::time::Duration,
    last_call: std::sync::Mutex<Option<std::time::Instant>>,
}

impl<R: Reranker> PacingReranker<R> {
    pub fn new(inner: R, min_interval: std::time::Duration) -> Self {
        Self {
            inner,
            min_interval,
            last_call: std::sync::Mutex::new(None),
        }
    }
}

impl<R: Reranker> Reranker for PacingReranker<R> {
    fn rerank(
        &self,
        query: &str,
        docs: &[String],
        top_n: usize,
    ) -> docsbase_memory::error::Result<Vec<(usize, f32)>> {
        if let Ok(mut last) = self.last_call.lock() {
            if let Some(previous) = *last {
                let elapsed = previous.elapsed();
                if elapsed < self.min_interval {
                    std::thread::sleep(self.min_interval - elapsed);
                }
            }
            *last = Some(std::time::Instant::now());
        }
        self.inner.rerank(query, docs, top_n)
    }
}

/// Pacing wrapper for rate-limited embedders (free tiers): sleeps before a
/// call when the previous one was too recent. Mirrors [`PacingReranker`].
pub struct PacingEmbedder<E: Embedder> {
    inner: E,
    min_interval: std::time::Duration,
    last_call: std::sync::Mutex<Option<std::time::Instant>>,
}

impl<E: Embedder> PacingEmbedder<E> {
    pub fn new(inner: E, min_interval: std::time::Duration) -> Self {
        Self {
            inner,
            min_interval,
            last_call: std::sync::Mutex::new(None),
        }
    }
}

impl<E: Embedder> Embedder for PacingEmbedder<E> {
    fn model_id(&self) -> &str {
        self.inner.model_id()
    }

    fn embed(&self, texts: &[String]) -> docsbase_memory::error::Result<Vec<Vec<f32>>> {
        if let Ok(mut last) = self.last_call.lock() {
            if let Some(previous) = *last {
                let elapsed = previous.elapsed();
                if elapsed < self.min_interval {
                    std::thread::sleep(self.min_interval - elapsed);
                }
            }
            *last = Some(std::time::Instant::now());
        }
        self.inner.embed(texts)
    }
}
