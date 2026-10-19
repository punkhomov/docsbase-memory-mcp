//! Search benchmark harness: fixture schema, loader, validator (T1 + DELTA-1).
//!
//! Offline gate foundation (FR-24…FR-28; NFR-5, NFR-6): typed fixtures,
//! content hashes, model-pin parsing, adjudication gates (`reviewed_by`,
//! reason), lexical control for the semantic slice. No production code is
//! touched; the harness lives in `tests/**`.

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

/// One relevant document for a case.
#[derive(Debug, Clone, Deserialize)]
pub struct ExpectedHit {
    pub path: String,
    pub lines: Option<LineRange>,
    /// 1 = acceptable, 2 = primary answer (FR-2).
    pub grade: u8,
}

/// One document that must not appear in top-k (FR-10, FR-26).
#[derive(Debug, Clone, Deserialize)]
pub struct AbsentHit {
    pub path: String,
    pub lines: Option<LineRange>,
    /// Why the document is a trap (adjudication evidence; required non-empty).
    #[serde(default)]
    pub reason: String,
}

/// One benchmark case.
#[derive(Debug, Clone, Deserialize)]
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
#[derive(Debug, Clone, Deserialize)]
pub struct QueryFile(pub Vec<QueryCase>);

/// One frozen corpus document (FR-1).
#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)] // origin/license/frozen_at consumed by provenance checks (T2+)
pub struct ManifestDoc {
    pub path: String,
    pub origin: String,
    pub license: String,
    pub frozen_at: String,
    pub blake3: String,
}

/// `corpus/MANIFEST.json`: the frozen corpus inventory.
#[derive(Debug, Clone, Deserialize)]
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
        let corpus_hash = corpus_hash(root)?;
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
/// blake3(file_bytes))` list of `.md` documents under `root/corpus`
/// (`MANIFEST.json` itself is not a document and is excluded).
pub fn corpus_hash(root: &Path) -> anyhow::Result<String> {
    let corpus_dir = root.join("corpus");
    let mut entries: BTreeMap<String, String> = BTreeMap::new();
    collect_markdown(&corpus_dir, &corpus_dir, &mut entries)?;
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

/// FR-26: critical classes and absent-bearing cases need ≥2 reviewers;
/// grade-1 or critical cases need a rationale in `notes`.
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
#[derive(Debug)]
#[allow(dead_code)] // per-case fields consumed by the T8/T10 report
pub struct LegacyCaseReport {
    pub id: String,
    pub query: String,
    pub hit: bool,
    pub absent_hit: bool,
    pub ndcg: f64,
}

/// Legacy parity outcome over the exact old corpus copy.
#[derive(Debug)]
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
        let ndcg = rank.map_or(0.0, |r| {
            // Ranks are single digits; the float holds them exactly.
            #[allow(clippy::cast_precision_loss)]
            let denom = (r + 2) as f64;
            1.0 / denom.log2()
        });
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
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
        std::fs::rename(&tmp, dir.join("manifest.json"))
            .with_context(|| format!("replace {}", dir.join("manifest.json").display()))?;
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
        std::fs::rename(&tmp, path).with_context(|| format!("replace {}", path.display()))?;
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
#[derive(Debug, Clone)]
#[allow(dead_code)] // chunk_id/heading_path/lines consumed by T8 metrics
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
    for case in cases {
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
        out.push(ConfigCaseRun {
            id: case.id.clone(),
            citations: parse_citations(&value, &case.id)?,
        });
    }
    Ok(ConfigRun { cases: out })
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
