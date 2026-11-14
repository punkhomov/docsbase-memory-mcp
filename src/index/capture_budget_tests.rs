use super::*;
use std::sync::Mutex;
#[test]
fn many_legal_files_refuse_next_capture_before_any_following_copy_and_release_charge() {
    let root = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    for n in 0..64 {
        std::fs::write(
            root.path().join(format!("item-{n:02}.md")),
            format!("# Heading {n}\n\n{}\n", "retained words ".repeat(70)),
        )
        .unwrap();
    }
    let mut db = Db::open(cache.path()).unwrap();
    let project = crate::daemon::registry::ensure_project(&mut db, root.path()).unwrap();
    let mut policy = crate::search::settings::standalone()
        .unwrap()
        .into_settings();
    policy.max_request_payload_bytes = 2 * 1024 * 1024;
    policy.max_retained_payload_bytes = 4 * 1024 * 1024;
    let budget = crate::search::budget::PayloadBudget::new(policy.clone()).unwrap();
    let mut account = budget.reserve(0).unwrap();
    let operation = crate::search::operation::SearchOperation::with_timeout(
        std::time::Instant::now(),
        std::time::Duration::from_secs(60),
        crate::hybrid::work::Cancellation::new(),
    )
    .unwrap();
    let events = Mutex::new(Vec::new());
    let observer = |event| events.lock().unwrap().push(event);
    let mut capture = CaptureBudget {
        account: &mut account,
        operation: &operation,
        observed: Some(&observer),
    };
    let result =
        capture_full_accounted(&db, &project, &Config::default(), false, Some(&mut capture));
    assert!(matches!(result, Err(Error::Admission { .. })));
    let events = events.into_inner().unwrap();
    let failed = events
        .iter()
        .position(|event| matches!(event, CaptureEvent::Refused(_)))
        .unwrap();
    assert_eq!(
        failed + 1,
        events.len(),
        "failed next reservation must have NO following allocation/copy event"
    );
    assert!(
        events
            .iter()
            .any(|event| matches!(event, CaptureEvent::Copy("actual bounded file read")))
    );
    assert!(events.iter().any(|event| matches!(
        event,
        CaptureEvent::Copy("hash/frontmatter/chunks/headings/JSON/apply workspace")
    )));
    let successful: Vec<_> = events
        .iter()
        .filter_map(|event| match event {
            CaptureEvent::Reserved(bytes) => Some(*bytes),
            _ => None,
        })
        .collect();
    assert!(
        successful
            .iter()
            .all(|bytes| *bytes <= policy.max_request_payload_bytes)
    );
    let CaptureEvent::Refused(unchanged) = events[failed] else {
        unreachable!()
    };
    assert_eq!(unchanged, account.bytes());
    let row:(String,i64,Option<String>,i64)=db.connection().query_row("SELECT status,index_attempt,index_outcome_json,index_generation FROM projects WHERE id=?1",[project.id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).unwrap();
    assert_eq!(row.1, 0);
    assert!(row.2.is_none());
    assert_eq!(row.3, 0);
    assert!(!crate::daemon::lifecycle::index_dir(cache.path(), project.id).exists());
    drop(account);
    assert!(
        budget
            .reserve(
                policy
                    .max_retained_payload_bytes
                    .min(policy.max_request_payload_bytes)
            )
            .is_ok()
    );
}
