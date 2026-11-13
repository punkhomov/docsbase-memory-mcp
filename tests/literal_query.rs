//! Explicit literal input must not acquire query-language meaning.

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};

use docsbase_memory::config::Config;
use docsbase_memory::daemon::{registry, tools};
use docsbase_memory::error::{Error, Result};
use docsbase_memory::hybrid::dense::DenseCache;
use docsbase_memory::hybrid::jobs::embed_project_chunks_with;
use docsbase_memory::hybrid::{HybridStages, Ranking, Reranker};
use docsbase_memory::index::QueryMode;
use docsbase_memory::index::chunk::{Chunk, ChunkKind};
use docsbase_memory::index::tantivy_index::{Hit, IndexHandle, ReadIndex};
use docsbase_memory::store::Db;
use docsbase_memory::store::models::{Project, ProjectStatus};
use docsbase_memory::vector_cache::Embedder;
use serde_json::json;
use tempfile::TempDir;

// Original issue87/SciFact strings, including its original spelling of baise.
const SCIENTIFIC_QUERIES: &[&str] = &[
    "The minor G allele of FOXO3 is related to more severe symptoms of Crohn's Disease.",
    "The ureABIEFGH gene cluster encodes urease maturation proteins : UreD/UreH, UreE, UreF, and UreG.",
    "Transplanted human glial progenitor cells are incapable of forming a neural network with host animals' neurons.",
    "Commelina yellow mottle virus' (ComYMV) genome consists of 7489 baise pairs.",
    "A high microerythrocyte count raises vulnerability to severe anemia in homozygous alpha (+)- thalassemia trait subjects.",
    "Macropinocytosis contributes to a cell's supply of amino acids via the intracellular uptake of protein.",
    "Pyridostatin destabilizes the G - quadruplex in the telomeric region.",
];

struct IndexFixture {
    index: IndexHandle,
    reader: ReadIndex,
    // Drop both index owners before removing their directory on Windows.
    _directory: TempDir,
}

impl IndexFixture {
    fn new(documents: &[(i64, &str, &str)]) -> Self {
        let directory = TempDir::new().expect("index directory");
        let mut index = IndexHandle::open_or_create(directory.path()).expect("open index");
        let chunks: Vec<_> = documents
            .iter()
            .map(|(id, title, text)| Chunk {
                doc_id: *id,
                seq: 0,
                heading_path: vec![(*title).to_owned()],
                kind: ChunkKind::Prose,
                line_start: 1,
                line_end: 1,
                text: (*text).to_owned(),
            })
            .collect();
        index.add_chunks(&chunks).expect("index documents");
        index.commit().expect("commit");
        index.mark_rebuilt().expect("mark ready");
        let reader = ReadIndex::open(directory.path()).expect("read-only index");
        Self {
            index,
            reader,
            _directory: directory,
        }
    }
}

fn ids(hits: &[Hit]) -> BTreeSet<i64> {
    hits.iter().map(|hit| hit.doc_id).collect()
}

#[test]
fn literal_retrieves_all_seven_original_scientific_queries() {
    let documents: Vec<_> = SCIENTIFIC_QUERIES
        .iter()
        .enumerate()
        .map(|(i, query)| {
            (
                i64::try_from(i + 1).expect("small fixture"),
                "Claim",
                *query,
            )
        })
        .collect();
    let fixture = IndexFixture::new(&documents);
    for (id, _, query) in documents {
        let writer_hits = fixture
            .index
            .search_with_mode(query, 20, QueryMode::Literal)
            .expect("literal through writer-owned reader");
        let reader_hits = fixture
            .reader
            .search_with_mode(query, 20, QueryMode::Literal)
            .expect("literal through read-only reader");
        assert!(ids(&writer_hits).contains(&id), "literal query: {query}");
        assert!(
            ids(&reader_hits).contains(&id),
            "read-only literal: {query}"
        );
        assert!(
            matches!(fixture.index.search(query, 20), Err(Error::Query { .. })),
            "the original syntax errors remain typed errors: {query}"
        );
    }
}

#[test]
fn existing_search_and_explicit_syntax_keep_identical_results() {
    let fixture = IndexFixture::new(&[(1, "Widgets", "alpha beta"), (2, "Other", "alpha gamma")]);
    assert_eq!(QueryMode::default(), QueryMode::Syntax);
    let signature = |hits: Vec<Hit>| {
        hits.into_iter()
            .map(|hit| (hit.chunk_id, hit.doc_id, hit.score.to_bits()))
            .collect::<Vec<_>>()
    };
    for query in [
        "alpha beta",
        "\"alpha beta\"",
        "title:Widgets",
        "alpha OR gamma",
        "alpha AND beta",
    ] {
        let old = fixture.index.search(query, 20).expect("existing syntax");
        let explicit = fixture
            .index
            .search_with_mode(query, 20, QueryMode::Syntax)
            .expect("explicit syntax");
        let read_only = fixture
            .reader
            .search_with_mode(query, 20, QueryMode::Syntax)
            .expect("read-only explicit syntax");
        assert_eq!(signature(old.clone()), signature(explicit), "{query}");
        assert_eq!(signature(old), signature(read_only), "{query}");
    }
    assert!(matches!(
        fixture
            .reader
            .search_with_mode("missing_field:value", 20, QueryMode::Syntax),
        Err(Error::Query { .. })
    ));
}

#[test]
fn literal_quotes_fields_and_signs_are_not_query_operators() {
    let fixture = IndexFixture::new(&[
        (1, "Other", "specialtag:needle quoted phrase"),
        (2, "Needle", "unrelated"),
        (3, "Other", "quoted only"),
        (4, "Other", "needlestack"),
        (10, "Other", "alpha"),
        (11, "Other", "beta"),
        (12, "Other", "alpha beta"),
    ]);
    let literal = |query| {
        ids(&fixture
            .reader
            .search_with_mode(query, 20, QueryMode::Literal)
            .expect("literal"))
    };
    assert_eq!(literal("specialtag:needle"), BTreeSet::from([1]));
    assert_eq!(literal("\"quoted phrase\""), BTreeSet::from([1, 3]));
    assert_eq!(literal("needle*"), BTreeSet::from([1, 2]));
    assert_eq!(literal("alpha -beta"), BTreeSet::from([10, 11, 12]));
    assert_eq!(literal("alpha AND beta"), BTreeSet::from([10, 11, 12]));
    assert_eq!(
        ids(&fixture
            .reader
            .search("\"quoted phrase\"", 20)
            .expect("phrase")),
        BTreeSet::from([1])
    );
    assert_eq!(
        ids(&fixture
            .reader
            .search("alpha AND beta", 20)
            .expect("conjunction")),
        BTreeSet::from([12])
    );
}

#[test]
fn literal_without_terms_is_query_error_but_limit_zero_still_skips_search() {
    let fixture = IndexFixture::new(&[(1, "Guide", "widgets")]);
    let punctuation = "+ - : () [] \" '";
    assert!(matches!(
        fixture
            .reader
            .search_with_mode(punctuation, 10, QueryMode::Literal),
        Err(Error::Query { .. })
    ));
    assert_eq!(
        fixture
            .reader
            .search_with_mode(punctuation, 0, QueryMode::Literal)
            .expect("limit zero"),
        [] as [docsbase_memory::index::tantivy_index::Hit; 0]
    );
    assert!(matches!(
        fixture.reader.search_with_mode("  ", 0, QueryMode::Literal),
        Err(Error::Query { .. })
    ));
}

struct ProductFixture {
    db: Db,
    project: Project,
    cache: TempDir,
    _root: TempDir,
}

impl ProductFixture {
    fn new() -> Self {
        let cache = TempDir::new().expect("cache");
        let root = TempDir::new().expect("project root");
        std::fs::write(
            root.path().join("claim.md"),
            format!("# Claim\n\n{}\n", SCIENTIFIC_QUERIES[0]),
        )
        .expect("claim");
        std::fs::write(root.path().join("guide.md"), "# Widgets\n\nwidgets guide\n")
            .expect("guide");
        let mut db = Db::open(cache.path()).expect("db");
        let canonical = root.path().canonicalize().expect("canonical");
        let mut project = registry::ensure_project(&mut db, &canonical).expect("project");
        tools::run_project_index(&mut db, cache.path(), &project, &Config::default())
            .expect("index");
        project.status = ProjectStatus::Indexed;
        Self {
            db,
            project,
            cache,
            _root: root,
        }
    }
}

#[derive(Default)]
struct CaptureEmbedder(Mutex<Vec<String>>);

impl Embedder for CaptureEmbedder {
    fn model_id(&self) -> &'static str {
        "fake/literal@d2@l2"
    }
    fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        Ok(texts.iter().map(|_| vec![1.0, 0.0]).collect())
    }
    fn embed_query(&self, query: &str) -> Result<Vec<f32>> {
        self.0.lock().expect("queries").push(query.to_owned());
        Ok(vec![1.0, 0.0])
    }
}

#[derive(Default)]
struct CaptureReranker(Mutex<Vec<String>>);

impl Reranker for CaptureReranker {
    fn rerank(&self, query: &str, docs: &[String], top_n: usize) -> Result<Vec<(usize, f32)>> {
        self.0.lock().expect("queries").push(query.to_owned());
        Ok(docs
            .iter()
            .enumerate()
            .take(top_n)
            .map(|(i, _)| (i, 1.0))
            .collect())
    }
}

#[test]
fn tool_literal_reaches_core_and_preserves_both_provider_inputs() {
    let fixture = ProductFixture::new();
    let embedder = Arc::new(CaptureEmbedder::default());
    embed_project_chunks_with(&fixture.db, embedder.as_ref(), fixture.project.id)
        .expect("stored vectors");
    let reranker = Arc::new(CaptureReranker::default());
    let stages = HybridStages {
        embedder: Some(Arc::clone(&embedder) as Arc<dyn Embedder>),
        reranker: Some(Arc::clone(&reranker) as Arc<dyn Reranker>),
        ranking: Ranking::Rrf,
    };
    let query = SCIENTIFIC_QUERIES[0];
    let response = tools::search_docs_with(
        &stages,
        &DenseCache::new(),
        &fixture.db,
        fixture.cache.path(),
        &fixture.project,
        &json!({"query":query,"query_mode":"literal","limit":10}),
        10,
    )
    .expect("literal through actual tool pipeline");
    assert!(
        response
            .as_array()
            .expect("default array")
            .iter()
            .any(|row| row["path"] == "claim.md")
    );
    assert_eq!(*embedder.0.lock().expect("embedding input"), vec![query]);
    assert_eq!(*reranker.0.lock().expect("rerank input"), vec![query]);
}

#[test]
fn invalid_query_mode_is_protocol_error_before_provider_work() {
    let fixture = ProductFixture::new();
    let reranker = Arc::new(CaptureReranker::default());
    let stages = HybridStages {
        embedder: None,
        reranker: Some(Arc::clone(&reranker) as Arc<dyn Reranker>),
        ranking: Ranking::Lexical,
    };
    for invalid in [json!("auto"), json!(1), json!(true), json!([]), json!({})] {
        let result = tools::search_docs_with(
            &stages,
            &DenseCache::new(),
            &fixture.db,
            fixture.cache.path(),
            &fixture.project,
            &json!({"query":"widgets","query_mode":invalid}),
            10,
        );
        let calls = reranker.0.lock().expect("calls").len();
        assert!(
            matches!(result, Err(Error::Protocol { .. })) && calls == 0,
            "result={result:?}, provider_calls={calls}"
        );
    }
}

#[test]
fn tool_missing_null_and_explicit_syntax_modes_are_compatible() {
    let fixture = ProductFixture::new();
    let search = |args| {
        tools::search_docs(&fixture.db, fixture.cache.path(), &fixture.project, &args)
            .expect("syntax search")
    };
    let legacy = search(json!({"query":"widgets"}));
    assert_eq!(legacy, search(json!({"query":"widgets","query_mode":null})));
    assert_eq!(
        legacy,
        search(json!({"query":"widgets","query_mode":"syntax"}))
    );
}

#[test]
fn literal_without_terms_stops_before_embedding_work() {
    let fixture = ProductFixture::new();
    let embedder = Arc::new(CaptureEmbedder::default());
    embed_project_chunks_with(&fixture.db, embedder.as_ref(), fixture.project.id)
        .expect("stored vectors");
    let stages = HybridStages {
        embedder: Some(Arc::clone(&embedder) as Arc<dyn Embedder>),
        reranker: None,
        ranking: Ranking::Rrf,
    };
    let result = tools::search_docs_with(
        &stages,
        &DenseCache::new(),
        &fixture.db,
        fixture.cache.path(),
        &fixture.project,
        &json!({"query":"+ - : []","query_mode":"literal"}),
        10,
    );
    assert!(matches!(result, Err(Error::Query { .. })), "{result:?}");
    assert_eq!(embedder.0.lock().expect("query inputs").len(), 0);
}

#[test]
fn ordinary_literal_words_keep_existing_scores_including_repetitions() {
    let fixture = IndexFixture::new(&[(1, "Guide", "alpha beta"), (2, "Other", "alpha gamma")]);
    for query in ["alpha", "alpha beta", "alpha alpha beta", "ALPHA alpha"] {
        let syntax = fixture.reader.search(query, 20).expect("ordinary syntax");
        let literal = fixture
            .reader
            .search_with_mode(query, 20, QueryMode::Literal)
            .expect("ordinary literal");
        assert_eq!(syntax, literal, "ordinary text: {query}");
    }
}
