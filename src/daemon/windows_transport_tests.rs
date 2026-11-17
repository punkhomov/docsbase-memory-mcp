//! Actual admitted Windows IPC; candidate observations precede cleanup.
use super::search_phase_tests::Fixture;
use super::*;
use serde::Serialize;
use std::io::{BufRead, Read, Write};
use std::sync::atomic::AtomicU8;
use std::sync::mpsc as std_mpsc;

const WATCHDOG: Duration = Duration::from_secs(30);

#[derive(Debug, Serialize)]
struct Prefix {
    bytes_read: usize,
    peek_ok: bool,
    peek_error: u32,
    unread_native_bytes: u32,
}

fn exchange(
    reader: &mut std::io::BufReader<crate::platform::BlockingStream>,
    request: &Request,
) -> std::result::Result<Response, String> {
    reader
        .get_mut()
        .write_all(&protocol::encode(request).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    let mut frame = Vec::new();
    reader
        .by_ref()
        .take(protocol::MAX_FRAME_BYTES as u64 + 2)
        .read_until(b'\n', &mut frame)
        .map_err(|e| e.to_string())?;
    protocol::decode_response(&frame).map_err(|e| e.to_string())
}

#[allow(unsafe_code)] // Read-only PeekNamedPipe borrows the live client's handle.
fn prefix(stream: &mut crate::platform::BlockingStream) -> std::result::Result<Prefix, String> {
    use windows_sys::Win32::Foundation::GetLastError;
    use windows_sys::Win32::System::Pipes::PeekNamedPipe;
    let mut byte = [0];
    stream.read_exact(&mut byte).map_err(|e| e.to_string())?;
    if byte != *b"{" {
        return Err("response did not start with a JSON object".into());
    }
    let mut available = 0;
    // SAFETY: stream owns this connected pipe throughout this synchronous query;
    // no data buffer is supplied and the available output points to a live DWORD.
    let ok = unsafe {
        PeekNamedPipe(
            stream.test_native_handle(),
            std::ptr::null_mut(),
            0,
            std::ptr::null_mut(),
            &raw mut available,
            std::ptr::null_mut(),
        )
    };
    let error = if ok == 0 {
        // SAFETY: immediate thread-local error capture after the failed query.
        unsafe { GetLastError() }
    } else {
        0
    };
    Ok(Prefix {
        bytes_read: byte.len(),
        peek_ok: ok != 0,
        peek_error: error,
        unread_native_bytes: available,
    })
}

fn evidence(name: &str, value: &impl Serialize) -> std::result::Result<(), String> {
    let Some(directory) = std::env::var_os("DOCSBASE_TRANSPORT155_EVIDENCE_DIR") else {
        return Ok(());
    };
    let directory = PathBuf::from(directory);
    std::fs::create_dir_all(&directory).map_err(|e| e.to_string())?;
    let path = directory.join(name);
    let mut file = std::fs::File::create(path).map_err(|e| e.to_string())?;
    serde_json::to_writer(&mut file, value).map_err(|e| e.to_string())?;
    file.write_all(b"\n").map_err(|e| e.to_string())?;
    file.sync_all().map_err(|e| e.to_string())
}

#[test]
#[allow(clippy::too_many_lines)] // One ordered native observation/cleanup protocol; assertions follow cleanup.
fn stalled_get_doc_retains_original_connection_until_native_retirement() {
    let fixture = Fixture::new(HybridStages::default());
    let document_bytes = usize::try_from(Config::default().max_file_size)
        .unwrap()
        .min(protocol::MAX_FRAME_BYTES / 2);
    std::fs::write(
        fixture.project.canonical_root.join("guide.md"),
        vec![b'x'; document_bytes],
    )
    .unwrap();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let slots = Arc::new(tokio::sync::Semaphore::new(1));
    let (release, proceed) = std_mpsc::channel();
    let (entered, observed) = tokio::sync::oneshot::channel();
    let (finished, done) = tokio::sync::oneshot::channel();
    let endpoint = crate::platform::daemon_endpoint(&fixture.shared.cache);
    let cwd = fixture.project.canonical_root.clone();
    // 0 setup, 1 waiting for explicit release, 2 draining, 3 returned,
    // 4 controller/observer channel lost (failed exchange, never auto-drain).
    let reader_phase = Arc::new(AtomicU8::new(0));
    let peer_phase = reader_phase.clone();
    let mut client_thread = None;
    let observations = runtime.block_on(async {
        let listener = crate::platform::bind(&endpoint).unwrap();
        client_thread = Some(std::thread::spawn(move || {
            let result = (|| {
                let stream =
                    crate::platform::connect_blocking(&endpoint).map_err(|e| e.to_string())?;
                // Capacity1 prevents hidden read-ahead from consuming the large
                // response before the single-byte/native-unread snapshot.
                let mut reader = std::io::BufReader::with_capacity(1, stream);
                let hello = exchange(
                    &mut reader,
                    &Request::Hello {
                        protocol_version: protocol::PROTOCOL_VERSION,
                        build_id: protocol::build_id(),
                        client: "transport155-application-test".into(),
                    },
                )?;
                if !matches!(hello, Response::Hello { .. }) {
                    return Err("actual Hello failed".to_owned());
                }
                let registered = exchange(
                    &mut reader,
                    &Request::RegisterSession {
                        pid: std::process::id(),
                        cwd,
                    },
                )?;
                if !matches!(registered, Response::ToolResult { .. }) {
                    return Err("actual registration failed".to_owned());
                }
                if !reader.buffer().is_empty() {
                    return Err("unexpected bytes before get_doc".to_owned());
                }
                reader
                    .get_mut()
                    .write_all(
                        &protocol::encode(&Request::CallTool {
                            name: "get_doc".into(),
                            args: json!({"path":"guide.md"}),
                        })
                        .map_err(|e| e.to_string())?,
                    )
                    .map_err(|e| e.to_string())?;
                let first = prefix(reader.get_mut())?;
                peer_phase.store(1, Ordering::Release);
                if entered.send(first).is_err() {
                    peer_phase.store(4, Ordering::Release);
                    return Err("prefix observer closed before explicit release".into());
                }
                // Only the controller releases this gate AFTER candidate sync.
                // A wedged controller is contained by the independent process
                // watchdog, never a client timeout falling through to cleanup.
                if proceed.recv().is_err() {
                    peer_phase.store(4, Ordering::Release);
                    return Err("controller closed before explicit reader release".into());
                }
                peer_phase.store(2, Ordering::Release);
                let mut response = vec![b'{'];
                reader
                    .take(protocol::MAX_FRAME_BYTES as u64 + 2)
                    .read_to_end(&mut response)
                    .map_err(|e| e.to_string())?;
                let decoded = protocol::decode_response(&response).map_err(|e| e.to_string())?;
                match decoded {
                    Response::ToolResult { value }
                        if value["content"].as_str().map(str::len) == Some(document_bytes) =>
                    {
                        Ok(response.len())
                    }
                    _ => Err("cleanup did not consume the complete real get_doc reply".into()),
                }
            })();
            if peer_phase.load(Ordering::Acquire) == 2 {
                peer_phase.store(3, Ordering::Release);
            }
            let _ = finished.send(result.clone());
            result
        }));
        let stream = listener.accept().await.unwrap();
        let permit = slots.clone().acquire_owned().await.unwrap();
        let (shutdown, _) = tokio::sync::watch::channel(false);
        let shared = fixture.shared.clone();
        let counter = shared.activity.counter.clone();
        let events = shared.activity.events.clone();
        let mut handler = tokio::spawn(handle_admitted_connection(
            shared,
            stream,
            counter.clone(),
            events,
            shutdown,
            permit,
        ));
        let prefix = tokio::time::timeout(WATCHDOG, observed).await;
        fixture.shared.workers.shutdown();
        let joined = tokio::time::timeout(WATCHDOG, &mut handler).await;
        let phase = reader_phase.load(Ordering::Acquire);
        let candidate = json!({
            "schema":1,"phase":"candidate-before-cleanup",
            "prefix":prefix.as_ref().ok().and_then(|value| value.as_ref().ok()),
            "handler_returned":matches!(joined, Ok(Ok(()))),
            "original_available_slots":slots.available_permits(),
            "original_activity":counter.load(Ordering::SeqCst),
            "document_bytes":document_bytes,
            "reader_gate_phase":phase,
            "cleanup_started":matches!(phase, 2 | 3),
        });
        let saved = evidence("frozen-candidate.json", &candidate);
        // No assertion precedes release/drain/join: preserve failure ownership.
        let _ = release.send(());
        if !handler.is_finished() {
            handler.abort();
            let _ = handler.await;
        }
        let cleanup = tokio::time::timeout(WATCHDOG, done).await;
        (candidate, saved, cleanup)
    });
    let client = client_thread.take().unwrap().join();
    let cleanup = json!({
        "schema":1,"phase":"cleanup",
        "reader_gate_phase":reader_phase.load(Ordering::Acquire),
        "client_returned":client.as_ref().is_ok_and(std::result::Result::is_ok),
        "actual_frame_bytes":client.as_ref().ok().and_then(|result| result.as_ref().ok()),
        "original_available_slots":slots.available_permits(),
        "original_activity":fixture.shared.activity.counter.load(Ordering::SeqCst),
    });
    evidence("cleanup.json", &cleanup).unwrap();
    observations.1.unwrap();
    assert!(matches!(observations.2, Ok(Ok(Ok(_)))), "{cleanup}");
    assert!(client.is_ok_and(|result| result.is_ok()), "{cleanup}");
    let candidate = observations.0;
    assert_eq!(
        candidate["reader_gate_phase"], 1,
        "candidate requires an explicitly gated reader: {candidate}"
    );
    assert_eq!(candidate["cleanup_started"], false, "{candidate}");
    assert_eq!(candidate["handler_returned"], true, "{candidate}");
    assert_eq!(candidate["prefix"]["bytes_read"], 1, "{candidate}");
    assert_eq!(candidate["prefix"]["peek_ok"], true, "{candidate}");
    assert!(
        candidate["prefix"]["unread_native_bytes"].as_u64().unwrap() > 0,
        "native fixture prerequisite: {candidate}"
    );
    assert_eq!(
        candidate["original_available_slots"], 0,
        "cancelled logical handler must retain original physical connection charge while reply remains unread: {candidate}"
    );
    assert!(
        candidate["original_activity"].as_u64().unwrap() > 0,
        "retirement must remain charged: {candidate}"
    );
}
