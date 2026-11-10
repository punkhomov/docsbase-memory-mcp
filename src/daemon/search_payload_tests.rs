//! Owned response stages use captured data and explicit admission, never SQL.

use super::*;
use crate::hybrid::Reranker;
use crate::hybrid::work::Cancellation;
use crate::index::tantivy_index::IndexHandle;
use crate::search::budget::PayloadBudget;
use crate::search::settings::SearchSettings;
use crate::store::models::ProjectStatus;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

const CAP: u64 = 4 * 1024 * 1024;
// Failure watchdog only. All successful ordering is driven by channel messages.
const WATCHDOG: Duration = Duration::from_secs(300);

struct Fixture {
    db: Db,
    project: Project,
    cache: tempfile::TempDir,
    _root: tempfile::TempDir,
}

impl Fixture {
    fn new() -> Self {
        let cache = tempfile::tempdir().expect("cache");
        let root = tempfile::tempdir().expect("root");
        std::fs::create_dir(root.path().join("guide")).expect("guide");
        for (path, text) in [
            ("guide/a.md", "old widgets alpha"),
            ("guide/b.md", "old widgets beta"),
            ("other.md", "old widgets gamma"),
        ] {
            std::fs::write(
                root.path().join(path),
                format!("---\ntags: old\n---\n# Heading\n\n{text}\n"),
            )
            .expect("document");
        }
        let mut db = Db::open(cache.path()).expect("writer");
        let mut project = registry::ensure_project(&mut db, root.path()).expect("project");
        let mut index =
            IndexHandle::open_or_create(&lifecycle::index_dir(cache.path(), project.id))
                .expect("index");
        run_full(&mut db, &mut index, &project, &Config::default()).expect("index run");
        project.status = ProjectStatus::Indexed;
        Self {
            db,
            project,
            cache,
            _root: root,
        }
    }

    fn context<'a>(
        &'a self,
        stages: &'a HybridStages,
        dense: &'a DenseCache,
        budget: &'a PayloadBudget,
        cancellation: &'a Cancellation,
    ) -> SearchContext<'a> {
        SearchContext {
            stages,
            dense,
            cache: self.cache.path(),
            project: &self.project,
            window: 3,
            payload_budget: budget,
            cancellation,
        }
    }
}

type Documents = Vec<(Vec<String>, usize)>;

struct Spy {
    calls: AtomicUsize,
    seen: Mutex<Documents>,
    effect: Box<dyn Fn() -> Result<()> + Send + Sync>,
}

impl Spy {
    fn new(effect: impl Fn() -> Result<()> + Send + Sync + 'static) -> Arc<Self> {
        Arc::new(Self {
            calls: AtomicUsize::new(0),
            seen: Mutex::new(Vec::new()),
            effect: Box::new(effect),
        })
    }

    fn stages(self: &Arc<Self>) -> HybridStages {
        HybridStages {
            reranker: Some(self.clone()),
            ..HybridStages::default()
        }
    }
}

impl Reranker for Spy {
    fn rerank(&self, _: &str, docs: &[String], top_n: usize) -> Result<Vec<(usize, f32)>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.seen.lock().expect("seen").push((docs.to_vec(), top_n));
        (self.effect)()?;
        Ok((0..docs.len())
            .rev()
            .enumerate()
            .map(|(rank, index)| {
                (
                    index,
                    f32::from(u16::try_from(docs.len() - rank).expect("bounded candidates")),
                )
            })
            .collect())
    }
}

fn budget(bytes: u64) -> PayloadBudget {
    PayloadBudget::new(SearchSettings {
        max_request_payload_bytes: bytes,
        max_retained_payload_bytes: bytes,
        ..crate::search::settings::standalone()
            .expect("owner policy")
            .into_settings()
    })
    .expect("policy")
}

fn args() -> Value {
    json!({"query":"widgets", "limit":3, "snippet":true, "explain":true,
        "path_prefix":"guide/", "tags":["old"], "dedupe":"path"})
}

fn prepare(
    fixture: &Fixture,
    stages: &HybridStages,
    budget: &PayloadBudget,
    cancellation: &Cancellation,
) -> PreparedSearch {
    prepare_search(
        &fixture.context(stages, &DenseCache::new(), budget, cancellation),
        &fixture.db,
        parse_search_args(&args()).expect("arguments"),
    )
    .expect("prepare")
}

fn assert_released(budget: &PayloadBudget, capacity: u64) {
    drop(
        budget
            .reserve(capacity)
            .expect("all payload charge returned"),
    );
}

#[test]
fn prepared_response_outlives_database_and_preserves_provider_input_order() {
    fn owned<T: Send + Sync + 'static>(_: &T) {}
    let fixture = Fixture::new();
    let spy = Spy::new(|| Ok(()));
    let stages = spy.stages();
    let budget = budget(CAP);
    let cancellation = Cancellation::new();
    let prepared = prepare(&fixture, &stages, &budget, &cancellation);
    owned(&prepared);
    let expected_documents: Vec<_> = prepared
        .ordered
        .iter()
        .map(|(id, _)| {
            let (doc, seq) = chunk_id_parts(*id);
            repo::chunk_text(fixture.db.connection(), doc, seq)
                .expect("text query")
                .expect("text")
        })
        .collect();
    let generation =
        repo::index_generation(fixture.db.connection(), fixture.project.id).expect("generation");
    drop(fixture.db);
    let value = prepared
        .finish(&stages, &cancellation)
        .expect("owned finish");
    assert_eq!(value["meta"]["index_generation"], generation);
    assert_eq!(value["hits"].as_array().expect("hits").len(), 2);
    assert_eq!(
        spy.seen.lock().expect("seen").as_slice(),
        &[(expected_documents, 3)]
    );
    assert_released(&budget, CAP);
}

#[test]
fn provider_side_sync_cannot_mix_new_text_citations_filters_or_generation() {
    let fixture = Fixture::new();
    let cache = fixture.cache.path().to_path_buf();
    let project = fixture.project.id;
    let before = repo::index_generation(fixture.db.connection(), project).expect("generation");
    let spy = Spy::new(move || {
        let writer = Db::open(&cache)?;
        writer.connection().execute_batch(
            "UPDATE docs SET rel_path='changed/'||rel_path, frontmatter_json='{\"tags\":[\"new\"]}';
             UPDATE chunks SET text='replacement', heading_path='new heading', line_start=90, line_end=99;",
        ).expect("sync writes");
        repo::bump_index_generation(writer.connection(), project)?;
        Ok(())
    });
    let value = search_docs_in(
        &fixture.context(
            &spy.stages(),
            &DenseCache::new(),
            &budget(CAP),
            &Cancellation::new(),
        ),
        &fixture.db,
        &args(),
    )
    .expect("snapshot response");
    assert_eq!(value["meta"]["index_generation"], before);
    assert_eq!(
        repo::index_generation(fixture.db.connection(), project).expect("new gen"),
        before + 1
    );
    let hits = value["hits"].as_array().expect("hits");
    assert_eq!(hits.len(), 2);
    for row in hits {
        assert!(row["path"].as_str().expect("path").starts_with("guide/"));
        assert!(
            row["snippet"]
                .as_str()
                .expect("snippet")
                .contains("old widgets")
        );
        assert_ne!(row["heading_path"], "new heading");
        assert!(row["lines"][0].as_u64().expect("line") < 90);
    }
}

#[test]
fn explicit_request_limit_refuses_before_provider_and_retains_no_charge() {
    let fixture = Fixture::new();
    let spy = Spy::new(|| Ok(()));
    let policy = budget(1);
    let error = search_docs_in(
        &fixture.context(
            &spy.stages(),
            &DenseCache::new(),
            &policy,
            &Cancellation::new(),
        ),
        &fixture.db,
        &args(),
    )
    .expect_err("payload admission");
    assert!(matches!(error, Error::Admission { .. }));
    assert_eq!(spy.calls.load(Ordering::SeqCst), 0);
    assert_released(&policy, 1);
}

#[test]
fn aggregate_budget_stays_charged_through_provider_then_allows_retry() {
    let fixture = Fixture::new();
    let cancellation = Cancellation::new();
    let sample = prepare(
        &fixture,
        &Spy::new(|| Ok(())).stages(),
        &budget(CAP),
        &cancellation,
    );
    let charge = sample.payload.charged_bytes();
    drop(sample);
    let policy = budget(charge);
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let release_rx = Mutex::new(release_rx);
    let blocked = Spy::new(move || {
        entered_tx.send(()).expect("entered");
        release_rx
            .lock()
            .expect("receiver")
            .recv_timeout(WATCHDOG)
            .expect("release");
        Ok(())
    });
    let cache = fixture.cache.path().to_path_buf();
    let project = fixture.project.clone();
    std::thread::scope(|scope| {
        let policy_ref = &policy;
        let blocked_ref = &blocked;
        let task = scope.spawn(move || {
            let reader = Db::open_readonly_with_settings(
                &cache,
                &crate::store::settings::standalone().expect("db policy"),
            )
            .expect("reader");
            search_docs_in(
                &SearchContext {
                    stages: &blocked_ref.stages(),
                    dense: &DenseCache::new(),
                    cache: &cache,
                    project: &project,
                    window: 3,
                    payload_budget: policy_ref,
                    cancellation: &Cancellation::new(),
                },
                &reader,
                &args(),
            )
        });
        entered_rx.recv_timeout(WATCHDOG).expect("provider entered");
        let other = Spy::new(|| Ok(()));
        let refused = search_docs_in(
            &fixture.context(&other.stages(), &DenseCache::new(), &policy, &cancellation),
            &fixture.db,
            &args(),
        );
        assert!(matches!(refused, Err(Error::Admission { .. })));
        assert_eq!(other.calls.load(Ordering::SeqCst), 0);
        release_tx.send(()).expect("release");
        task.join().expect("worker").expect("first response");
        search_docs_in(
            &fixture.context(&other.stages(), &DenseCache::new(), &policy, &cancellation),
            &fixture.db,
            &args(),
        )
        .expect("retry after release");
        assert_eq!(other.calls.load(Ordering::SeqCst), 1);
    });
    assert_released(&policy, charge);
}

#[test]
fn cancelled_preparation_never_starts_provider() {
    let fixture = Fixture::new();
    let spy = Spy::new(|| Ok(()));
    let policy = budget(CAP);
    let cancellation = Cancellation::new();
    cancellation.cancel();
    assert!(
        search_docs_in(
            &fixture.context(&spy.stages(), &DenseCache::new(), &policy, &cancellation),
            &fixture.db,
            &args(),
        )
        .is_err()
    );
    assert_eq!(spy.calls.load(Ordering::SeqCst), 0);
    assert_released(&policy, CAP);
}

#[test]
fn cancellation_between_capture_and_finish_skips_rerank() {
    let fixture = Fixture::new();
    let spy = Spy::new(|| Ok(()));
    let stages = spy.stages();
    let policy = budget(CAP);
    let cancellation = Cancellation::new();
    let prepared = prepare(&fixture, &stages, &policy, &cancellation);
    cancellation.cancel();
    assert!(prepared.finish(&stages, &cancellation).is_err());
    assert_eq!(spy.calls.load(Ordering::SeqCst), 0);
    assert_released(&policy, CAP);
}

#[test]
fn provider_cancellation_stops_response_and_releases_payload() {
    let fixture = Fixture::new();
    let cancellation = Cancellation::new();
    let token = cancellation.clone();
    let spy = Spy::new(move || {
        token.cancel();
        Ok(())
    });
    let policy = budget(CAP);
    assert!(
        search_docs_in(
            &fixture.context(&spy.stages(), &DenseCache::new(), &policy, &cancellation),
            &fixture.db,
            &args(),
        )
        .is_err()
    );
    assert_eq!(spy.calls.load(Ordering::SeqCst), 1);
    assert_released(&policy, CAP);
}

#[test]
fn provider_error_returns_all_charge() {
    let fixture = Fixture::new();
    let spy = Spy::new(|| Err(Error::internal("provider failure")));
    let policy = budget(CAP);
    let error = search_docs_in(
        &fixture.context(
            &spy.stages(),
            &DenseCache::new(),
            &policy,
            &Cancellation::new(),
        ),
        &fixture.db,
        &args(),
    )
    .expect_err("provider error");
    assert!(error.to_string().contains("provider failure"));
    assert_released(&policy, CAP);
}

#[test]
fn provider_unwind_returns_all_charge() {
    let fixture = Fixture::new();
    let spy = Spy::new(|| panic!("test provider unwind"));
    let policy = budget(CAP);
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        search_docs_in(
            &fixture.context(
                &spy.stages(),
                &DenseCache::new(),
                &policy,
                &Cancellation::new(),
            ),
            &fixture.db,
            &args(),
        )
    }));
    assert!(outcome.is_err());
    assert_released(&policy, CAP);
}

#[test]
fn zero_limit_preserves_empty_wire_without_payload_reservation() {
    let fixture = Fixture::new();
    let spy = Spy::new(|| Ok(()));
    let value = search_docs_in(
        &fixture.context(
            &spy.stages(),
            &DenseCache::new(),
            &budget(1),
            &Cancellation::new(),
        ),
        &fixture.db,
        &json!({"query":"widgets", "limit":0}),
    )
    .expect("empty response");
    assert_eq!(value, json!([]));
    assert_eq!(spy.calls.load(Ordering::SeqCst), 0);
}
