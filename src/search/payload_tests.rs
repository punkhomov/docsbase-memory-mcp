use super::*;
use crate::daemon::registry;
use crate::error::Error;
use crate::hybrid::work::Cancellation;
use crate::index::tantivy_index::{MAX_HITS, chunk_id};
use crate::search::budget::PayloadBudget;
use crate::search::settings::SearchSettings;
use crate::store::{DB_FILE, Db, repo};
use std::sync::{Mutex, mpsc};
use std::time::Duration;

const TEXTS: [&str; 2] = ["A\0界🙂", "beta"];
const FRONTMATTER: &str = r#"{"tags":["rust","search"]}"#;
const WATCHDOG: Duration = Duration::from_secs(300);

struct Fixture {
    writer: Db,
    cache: tempfile::TempDir,
    _roots: [tempfile::TempDir; 2],
    project: i64,
    doc: i64,
    other: i64,
    other_doc: i64,
    ids: Vec<u64>,
}

impl Fixture {
    fn new() -> Self {
        Self::encoded("UTF-8")
    }

    fn encoded(encoding: &str) -> Self {
        let cache = tempfile::tempdir().expect("cache");
        let roots = [
            tempfile::tempdir().expect("root"),
            tempfile::tempdir().expect("other root"),
        ];
        if encoding != "UTF-8" {
            let db = rusqlite::Connection::open(cache.path().join(DB_FILE)).expect("encoding db");
            let pragma = match encoding {
                "UTF-16le" => "PRAGMA encoding='UTF-16le'; CREATE TABLE encoding_probe(value TEXT)",
                "UTF-16be" => "PRAGMA encoding='UTF-16be'; CREATE TABLE encoding_probe(value TEXT)",
                _ => panic!("unsupported test encoding"),
            };
            db.execute_batch(pragma).expect("encoding");
        }
        let mut writer = Db::open(cache.path()).expect("writer");
        let project = registry::ensure_project(&mut writer, roots[0].path())
            .expect("project")
            .id;
        let other = registry::ensure_project(&mut writer, roots[1].path())
            .expect("other")
            .id;
        let doc = seed(&writer, project, "guide.md", &TEXTS);
        let other_doc = seed(&writer, other, "private.md", &["other project's secret"]);
        Self {
            writer,
            cache,
            _roots: roots,
            project,
            doc,
            other,
            other_doc,
            ids: vec![chunk_id(doc, 1), chunk_id(doc, 0)],
        }
    }

    fn reader(&self) -> Db {
        Db::open_readonly_with_settings(
            self.cache.path(),
            &crate::store::settings::standalone().expect("policy"),
        )
        .expect("reader")
    }

    fn request(&self) -> PayloadRequest<'_> {
        PayloadRequest {
            cache_root: self.cache.path(),
            project_id: self.project,
            generation: 1,
            chunk_ids: &self.ids,
            include_text: true,
            include_filter_metadata: true,
        }
    }
}

fn seed(db: &Db, project: i64, path: &str, texts: &[&str]) -> i64 {
    let doc = repo::insert_pending_doc(db.connection(), project, path, path).expect("doc");
    repo::upsert_doc(
        db.connection(),
        project,
        doc,
        &repo::NewDoc {
            rel_path: path,
            abs_path: path,
            title: None,
            frontmatter_json: Some(FRONTMATTER),
            size: i64::try_from(texts.iter().map(|text| text.len()).sum::<usize>()).expect("size"),
            mtime: 0,
            content_hash: "indexed",
            indexed_at: 0,
        },
    )
    .expect("ready");
    let chunks: Vec<_> = texts
        .iter()
        .enumerate()
        .map(|(seq, text)| repo::NewChunk {
            seq: u32::try_from(seq).expect("seq"),
            heading_path: "Guide > 界🙂",
            kind: "prose",
            lang: None,
            line_start: 1,
            line_end: 2,
            text,
        })
        .collect();
    repo::replace_chunks(db.connection(), doc, &chunks).expect("chunks");
    repo::bump_index_generation(db.connection(), project).expect("generation");
    doc
}

fn budget(request: u64, retained: u64) -> PayloadBudget {
    PayloadBudget::new(SearchSettings {
        max_request_payload_bytes: request,
        max_retained_payload_bytes: retained,
    })
    .expect("budget")
}

fn ample() -> PayloadBudget {
    budget(1024 * 1024, 2 * 1024 * 1024)
}

fn owned<T: Send + Sync + 'static>() {}

#[test]
fn capture_owns_identities_text_order_citations_and_filter_metadata() {
    owned::<OwnedPayload>();
    let fixture = Fixture::new();
    let reader = fixture.reader();
    let payload = OwnedPayload::capture(&reader, fixture.request(), &ample(), &Cancellation::new())
        .expect("payload");
    assert_eq!(payload.cache_root(), fixture.cache.path());
    assert_eq!(payload.project_id(), fixture.project);
    assert_eq!(payload.generation(), 1);
    assert_eq!(
        payload.rerank_documents().expect("documents"),
        [TEXTS[1], TEXTS[0]]
    );
    let citation = payload.citation(fixture.ids[0]).expect("citation");
    assert_eq!(citation.path, "guide.md");
    assert_eq!(citation.heading_path, "Guide > 界🙂");
    assert_eq!((citation.line_start, citation.line_end), (1, 2));
    assert_eq!(payload.chunk_text(fixture.ids[1]), Some(TEXTS[0]));
    let meta = payload.doc_meta(fixture.doc).expect("metadata");
    assert_eq!(meta.rel_path, "guide.md");
    assert_eq!(meta.frontmatter_json.as_deref(), Some(FRONTMATTER));
    assert!(reader.connection().is_autocommit());
    drop(reader);
    assert_eq!(payload.chunk_text(fixture.ids[0]), Some(TEXTS[1]));
}

#[test]
fn capture_flags_skip_unneeded_text_and_filter_metadata() {
    let fixture = Fixture::new();
    let request = PayloadRequest {
        include_text: false,
        include_filter_metadata: false,
        ..fixture.request()
    };
    let payload = OwnedPayload::capture(&fixture.reader(), request, &ample(), &Cancellation::new())
        .expect("citations only");
    assert!(payload.citation(fixture.ids[0]).is_some());
    assert!(payload.chunk_text(fixture.ids[0]).is_none());
    assert!(payload.doc_meta(fixture.doc).is_none());
    assert!(payload.rerank_documents().is_err());
}

#[test]
fn utf8_preflight_counts_bytes_including_multibyte_and_nul_text() {
    let fixture = Fixture::new();
    let budget = ample();
    let no_text = OwnedPayload::capture(
        &fixture.reader(),
        PayloadRequest {
            include_text: false,
            ..fixture.request()
        },
        &budget,
        &Cancellation::new(),
    )
    .expect("no text");
    let with_text = OwnedPayload::capture(
        &fixture.reader(),
        fixture.request(),
        &budget,
        &Cancellation::new(),
    )
    .expect("text");
    let slots = TEXTS.len() * std::mem::size_of::<String>();
    let bytes: usize = TEXTS.iter().map(|text| text.len()).sum();
    assert_eq!(
        with_text.charged_bytes() - no_text.charged_bytes(),
        u64::try_from(slots + bytes).expect("byte charge")
    );
}

#[test]
fn utf16_preflight_admits_a_safe_utf8_upper_bound() {
    for encoding in ["UTF-16le", "UTF-16be"] {
        let fixture = Fixture::encoded(encoding);
        // CJK requires more UTF-8 bytes than its native UTF-16 storage. Make
        // that difference dominate any ASCII metadata overestimate.
        let wide = format!("{}🙂\0", "界".repeat(512));
        fixture
            .writer
            .connection()
            .execute(
                "UPDATE chunks SET text=?1 WHERE doc_id=?2 AND seq=0",
                rusqlite::params![wide, fixture.doc],
            )
            .expect("CJK-heavy source");
        let payload = OwnedPayload::capture(
            &fixture.reader(),
            fixture.request(),
            &ample(),
            &Cancellation::new(),
        )
        .expect("UTF16 payload");
        assert_eq!(
            payload.rerank_documents().expect("text"),
            [TEXTS[1], wide.as_str()]
        );
        let charge = payload.charged_bytes();
        drop(payload);
        let exact = budget(charge, charge);
        let _held = OwnedPayload::capture(
            &fixture.reader(),
            fixture.request(),
            &exact,
            &Cancellation::new(),
        )
        .expect("preflight boundary");
        assert!(exact.reserve(1).is_err());
    }
}

#[test]
fn real_payload_respects_request_and_retained_charge_boundaries() {
    let fixture = Fixture::new();
    let payload = OwnedPayload::capture(
        &fixture.reader(),
        fixture.request(),
        &ample(),
        &Cancellation::new(),
    )
    .expect("measure");
    let charge = payload.charged_bytes();
    drop(payload);
    let too_small = budget(charge - 1, charge - 1);
    assert!(
        OwnedPayload::capture(
            &fixture.reader(),
            fixture.request(),
            &too_small,
            &Cancellation::new()
        )
        .is_err()
    );
    let exact = budget(charge, charge);
    let held = OwnedPayload::capture(
        &fixture.reader(),
        fixture.request(),
        &exact,
        &Cancellation::new(),
    )
    .expect("boundary");
    assert!(
        OwnedPayload::capture(
            &fixture.reader(),
            fixture.request(),
            &exact,
            &Cancellation::new()
        )
        .is_err()
    );
    drop(held);
    assert!(
        OwnedPayload::capture(
            &fixture.reader(),
            fixture.request(),
            &exact,
            &Cancellation::new()
        )
        .is_ok()
    );
}

#[test]
fn foreign_or_missing_candidates_cannot_expose_another_projects_data() {
    let fixture = Fixture::new();
    let ids = [chunk_id(fixture.other_doc, 0), chunk_id(fixture.doc, 99)];
    let payload = OwnedPayload::capture(
        &fixture.reader(),
        PayloadRequest {
            chunk_ids: &ids,
            ..fixture.request()
        },
        &ample(),
        &Cancellation::new(),
    )
    .expect("missing data");
    assert!(payload.citation(ids[0]).is_none());
    assert!(payload.chunk_text(ids[0]).is_none());
    assert!(payload.doc_meta(fixture.other_doc).is_none());
    assert!(payload.rerank_documents().is_err());
}

#[test]
fn capture_rejects_wrong_root_generation_pending_and_invalid_candidate_sets() {
    let fixture = Fixture::new();
    let wrong = tempfile::tempdir().expect("wrong root");
    let reader = fixture.reader();
    let budget = ample();
    let cancelled = Cancellation::new();
    cancelled.cancel();
    for (root, generation, ids) in [
        (wrong.path(), 1, fixture.ids.clone()),
        (fixture.cache.path(), 2, fixture.ids.clone()),
        (fixture.cache.path(), 1, vec![fixture.ids[0]; 2]),
        (fixture.cache.path(), 1, vec![fixture.ids[0]; MAX_HITS + 1]),
    ] {
        assert!(
            OwnedPayload::capture(
                &reader,
                PayloadRequest {
                    cache_root: root,
                    generation,
                    chunk_ids: &ids,
                    ..fixture.request()
                },
                &budget,
                &Cancellation::new()
            )
            .is_err()
        );
        assert!(reader.connection().is_autocommit());
    }
    assert!(OwnedPayload::capture(&reader, fixture.request(), &budget, &cancelled).is_err());
    repo::invalidate_doc(fixture.writer.connection(), fixture.other_doc).expect("other marker");
    assert!(
        repo::has_pending_docs(fixture.writer.connection(), fixture.other).expect("other pending")
    );
    assert!(
        OwnedPayload::capture(&reader, fixture.request(), &budget, &Cancellation::new()).is_ok()
    );
    repo::invalidate_doc(fixture.writer.connection(), fixture.doc).expect("own marker");
    assert!(
        OwnedPayload::capture(&reader, fixture.request(), &budget, &Cancellation::new()).is_err()
    );
}

#[test]
fn writer_between_preflight_and_copy_cannot_mix_generation_text_or_charge() {
    let fixture = Fixture::new();
    let reader = fixture.reader();
    let budget = ample();
    let payload = OwnedPayload::capture_with(
        &reader,
        fixture.request(),
        &budget,
        &Cancellation::new(),
        || {
            let tx = fixture
                .writer
                .connection()
                .unchecked_transaction()
                .expect("writer");
            tx.execute(
                "UPDATE chunks SET text='changed after preflight' WHERE doc_id=?1 AND seq=0",
                [fixture.doc],
            )
            .expect("change");
            repo::bump_index_generation(&tx, fixture.project).expect("generation");
            tx.commit().expect("commit");
            Ok(())
        },
        |_| Ok(()),
    )
    .expect("old coherent payload");
    assert_eq!(payload.generation(), 1);
    assert_eq!(payload.chunk_text(fixture.ids[1]), Some(TEXTS[0]));
    assert!(reader.connection().is_autocommit());
    assert_eq!(
        repo::index_generation(reader.connection(), fixture.project).expect("new gen"),
        2
    );
}

#[test]
fn caller_transaction_remains_owned_by_the_caller() {
    let fixture = Fixture::new();
    let reader = fixture.reader();
    reader
        .connection()
        .execute_batch("BEGIN")
        .expect("caller begins");
    let payload = OwnedPayload::capture(&reader, fixture.request(), &ample(), &Cancellation::new())
        .expect("payload");
    assert!(!reader.connection().is_autocommit());
    drop(payload);
    reader
        .connection()
        .execute_batch("ROLLBACK")
        .expect("caller ends");
}

#[test]
fn preflight_refusal_does_not_begin_copying_payload() {
    let fixture = Fixture::new();
    let reader = fixture.reader();
    assert!(
        OwnedPayload::capture_with(
            &reader,
            fixture.request(),
            &budget(1, 1),
            &Cancellation::new(),
            || panic!("refused capture reached copy boundary"),
            |_| panic!("refused capture copied data")
        )
        .is_err()
    );
    assert!(reader.connection().is_autocommit());
}

pub(super) struct DataDropGate {
    entered: mpsc::Sender<()>,
    released: Mutex<mpsc::Receiver<()>>,
}

impl Drop for DataDropGate {
    fn drop(&mut self) {
        let _ = self.entered.send(());
        let _ = self
            .released
            .get_mut()
            .expect("drop gate")
            .recv_timeout(WATCHDOG);
    }
}

#[derive(Clone, Copy)]
enum Finish {
    Success,
    Error,
    Panic,
}

fn gated_data_drop(finish: Finish) {
    let fixture = Fixture::new();
    let measured = OwnedPayload::capture(
        &fixture.reader(),
        fixture.request(),
        &ample(),
        &Cancellation::new(),
    )
    .expect("measure");
    let charge = measured.charged_bytes();
    drop(measured);
    let budget = budget(charge, charge);
    let reader = fixture.reader();
    let request = fixture.request();
    std::thread::scope(|scope| {
        let (entered, dropping) = mpsc::channel();
        let (release, released) = mpsc::channel();
        let worker_budget = budget.clone();
        let worker = scope.spawn(move || {
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let result = OwnedPayload::capture_with(
                    &reader,
                    request,
                    &worker_budget,
                    &Cancellation::new(),
                    || Ok(()),
                    |data| {
                        data.drop_gate = Some(DataDropGate {
                            entered,
                            released: Mutex::new(released),
                        });
                        match finish {
                            Finish::Success => Ok(()),
                            Finish::Error => Err(Error::internal("capture after-copy failure")),
                            Finish::Panic => panic!("capture after-copy unwind"),
                        }
                    },
                );
                match finish {
                    Finish::Success => drop(result.expect("payload")),
                    Finish::Error => assert!(result.is_err()),
                    Finish::Panic => panic!("expected capture to unwind"),
                }
            }));
            assert!(
                reader.connection().is_autocommit(),
                "own transaction ended on every path"
            );
            outcome
        });
        dropping
            .recv_timeout(WATCHDOG)
            .expect("actual payload data destructor entered");
        assert!(
            budget.reserve(1).is_err(),
            "charge must stay until data is freed"
        );
        release.send(()).expect("release destructor");
        let outcome = worker.join().expect("worker");
        assert_eq!(outcome.is_err(), matches!(finish, Finish::Panic));
    });
    drop(budget.reserve(charge).expect("entire charge returned"));
}

#[test]
fn payload_data_is_freed_before_its_charge_on_success() {
    gated_data_drop(Finish::Success);
}

#[test]
fn payload_data_is_freed_before_its_charge_on_error() {
    gated_data_drop(Finish::Error);
}

#[test]
fn payload_data_is_freed_before_its_charge_on_unwind() {
    gated_data_drop(Finish::Panic);
}

#[test]
fn cancellation_after_preflight_releases_allowance_without_copying() {
    let fixture = Fixture::new();
    let measured = OwnedPayload::capture(
        &fixture.reader(),
        fixture.request(),
        &ample(),
        &Cancellation::new(),
    )
    .expect("measure");
    let charge = measured.charged_bytes();
    drop(measured);
    let budget = budget(charge, charge);
    let reader = fixture.reader();
    let control = Cancellation::new();
    assert!(
        OwnedPayload::capture_with(
            &reader,
            fixture.request(),
            &budget,
            &control,
            || {
                control.cancel();
                Ok(())
            },
            |_| panic!("cancelled capture began copying")
        )
        .is_err()
    );
    assert!(reader.connection().is_autocommit());
    drop(
        budget
            .reserve(charge)
            .expect("cancelled capture released allowance"),
    );
}

#[test]
fn sqlite_copy_error_releases_data_charge_and_only_its_own_transaction() {
    let fixture = Fixture::new();
    let measured = OwnedPayload::capture(
        &fixture.reader(),
        fixture.request(),
        &ample(),
        &Cancellation::new(),
    )
    .expect("measure");
    let charge = measured.charged_bytes();
    drop(measured);
    // Same byte length preflights successfully; decoding the invalid stored
    // BLOB as a text field fails after the other candidate data was copied.
    fixture
        .writer
        .connection()
        .execute(
            "UPDATE docs SET frontmatter_json=CAST(frontmatter_json AS BLOB) WHERE id=?1",
            [fixture.doc],
        )
        .expect("invalid stored type");
    let budget = budget(charge, charge);
    let reader = fixture.reader();
    for caller_owned in [false, true] {
        if caller_owned {
            reader
                .connection()
                .execute_batch("BEGIN")
                .expect("caller begins");
        }
        assert!(
            OwnedPayload::capture(&reader, fixture.request(), &budget, &Cancellation::new())
                .is_err()
        );
        assert_eq!(reader.connection().is_autocommit(), !caller_owned);
        drop(
            budget
                .reserve(charge)
                .expect("failed copy released full allowance"),
        );
        if caller_owned {
            reader
                .connection()
                .execute_batch("ROLLBACK")
                .expect("caller ends");
        }
    }
    fixture
        .writer
        .connection()
        .execute(
            "UPDATE docs SET frontmatter_json=CAST(frontmatter_json AS TEXT) WHERE id=?1",
            [fixture.doc],
        )
        .expect("repair");
    assert!(
        OwnedPayload::capture(&reader, fixture.request(), &budget, &Cancellation::new()).is_ok()
    );
}

#[test]
fn filter_metadata_is_charged_once_per_distinct_document() {
    let fixture = Fixture::new();
    let budget = ample();
    let without = OwnedPayload::capture(
        &fixture.reader(),
        PayloadRequest {
            include_filter_metadata: false,
            ..fixture.request()
        },
        &budget,
        &Cancellation::new(),
    )
    .expect("without filters");
    let with = OwnedPayload::capture(
        &fixture.reader(),
        fixture.request(),
        &budget,
        &Cancellation::new(),
    )
    .expect("filter metadata");
    let extra = std::mem::size_of::<(i64, repo::DocMeta)>() + "guide.md".len() + FRONTMATTER.len();
    assert_eq!(
        with.charged_bytes() - without.charged_bytes(),
        u64::try_from(extra).expect("charge")
    );
}

#[test]
fn empty_candidate_set_has_owned_identity_and_no_document_rows() {
    let fixture = Fixture::new();
    let payload = OwnedPayload::capture(
        &fixture.reader(),
        PayloadRequest {
            chunk_ids: &[],
            ..fixture.request()
        },
        &ample(),
        &Cancellation::new(),
    )
    .expect("empty payload");
    assert_eq!(payload.generation(), 1);
    assert!(
        payload
            .rerank_documents()
            .expect("empty text mode")
            .is_empty()
    );
    assert!(payload.citation(fixture.ids[0]).is_none());
    assert!(payload.doc_meta(fixture.doc).is_none());
}

#[test]
fn document_without_frontmatter_keeps_metadata_without_a_replacement_value() {
    let fixture = Fixture::new();
    fixture
        .writer
        .connection()
        .execute(
            "UPDATE docs SET frontmatter_json=NULL WHERE id=?1",
            [fixture.doc],
        )
        .expect("no frontmatter");
    let payload = OwnedPayload::capture(
        &fixture.reader(),
        fixture.request(),
        &ample(),
        &Cancellation::new(),
    )
    .expect("payload");
    let meta = payload
        .doc_meta(fixture.doc)
        .expect("document metadata still present");
    assert_eq!(meta.rel_path, "guide.md");
    assert!(meta.frontmatter_json.is_none());
    assert_eq!(
        payload.rerank_documents().expect("text"),
        [TEXTS[1], TEXTS[0]]
    );
}
