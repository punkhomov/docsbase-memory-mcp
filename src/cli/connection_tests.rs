//! Post-connect setup must never become permission to index directly.
use std::cell::Cell;
use std::path::Path;

use crate::error::{Error, Result};
use crate::ipc::client::Client;
use crate::platform;

// A bound listener is sufficient: setup callbacks run only after native connect.
// No accepted-stream IO is needed to classify the constructor's result.
struct Peer {
    listener: platform::BlockingListener,
    cache: tempfile::TempDir,
    transport: crate::test_transport::TestTransport,
}
impl Peer {
    fn new() -> Self {
        let cache = tempfile::tempdir().unwrap();
        platform::fs::secure_dir(&cache.path().join("state")).unwrap();
        let transport = crate::test_transport::TestTransport::new(8);
        let listener =
            platform::bind_blocking(&transport, &platform::daemon_endpoint(cache.path())).unwrap();
        Self {
            listener,
            cache,
            transport,
        }
    }
}

fn injected_setup(
    transport: &crate::platform::Transport,
    cache: &Path,
    fail_read: bool,
) -> Result<Option<Client>> {
    Client::connect_with_setup(
        transport,
        cache,
        crate::ipc::settings::ClientSpec::DEFAULTS.ipc_io_timeout(),
        |_| {
            if fail_read {
                Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "injected read timeout setup failure",
                ))
            } else {
                Ok(())
            }
        },
        |_| {
            Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "injected write timeout setup failure",
            ))
        },
    )
}

#[test]
fn setup_errors_after_native_connect_remain_transport_errors() {
    for fail_read in [true, false] {
        let peer = Peer::new();
        let error = injected_setup(&peer.transport, peer.cache.path(), fail_read).err();
        let error = error.expect("post-connect setup error collapsed into absence");
        assert!(matches!(error, Error::Transport { .. }));
        assert!(error.to_string().contains(if fail_read {
            "injected read"
        } else {
            "injected write"
        }));
    }
}

#[test]
fn connected_setup_error_cannot_replay_cli_sync_or_create_index_state() {
    use std::io::{BufRead, Write};
    let _env = crate::cli::test_env::env_guard();
    for fail_read in [true, false] {
        let peer = Peer::new();
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("guide.md"), "replacementwidget").unwrap();
        let cache = peer.cache.path().to_path_buf();
        let listener = peer.listener;
        let server = std::thread::spawn(move || {
            let stream = listener.accept().unwrap();
            let mut reader = std::io::BufReader::new(stream);
            let mut request = String::new();
            reader.read_line(&mut request).unwrap();
            assert!(request.contains("Hello"));
            if !fail_read {
                reader
                    .get_mut()
                    .write_all(b"invalid handshake response\n")
                    .unwrap();
                let shutdown = reader.get_mut().shutdown();
                // The malformed reply deliberately makes the client close.
                // Darwin may observe that close before this fixture's half-close.
                #[cfg(unix)]
                if let Err(error) = shutdown {
                    assert_eq!(error.kind(), std::io::ErrorKind::NotConnected);
                }
                #[cfg(windows)]
                shutdown.unwrap();
            }
        });
        let cache_for_command = cache.clone();
        let target = root.path().to_path_buf();
        let result = super::command::run(
            crate::config::Config::default(),
            move |mut context| async move {
                context.cache = cache_for_command;
                super::sync_command::run_in(
                    context,
                    Some(target),
                    crate::store::settings::standalone().unwrap(),
                    std::time::Instant::now(),
                )
                .await
            },
        );
        server.join().unwrap();
        let error = result
            .err()
            .expect("actual connected Sync setup must not replay direct");
        assert!(matches!(error, Error::Transport { .. }), "{error}");
        assert!(error.to_string().contains(if fail_read {
            "connection closed"
        } else {
            "bad response"
        }));
        for name in ["registry.db", "projects", "logs", "vectors.db"] {
            assert!(
                !cache.join(name).exists(),
                "connected failure created {name}"
            );
        }
    }
}

#[test]
fn absent_connect_never_runs_setup_or_creates_state() {
    let cache = tempfile::tempdir().unwrap();
    let transport = crate::test_transport::TestTransport::new(1);
    let result = Client::connect_with_setup(
        &transport,
        cache.path(),
        crate::ipc::settings::ClientSpec::DEFAULTS.ipc_io_timeout(),
        |_| panic!("read setup without a connection"),
        |_| panic!("write setup without a connection"),
    );
    assert!(matches!(result, Ok(None)));
    assert!(matches!(
        Client::try_connect(&transport, cache.path()),
        Ok(None)
    ));
    assert!(
        Client::connect(&transport, cache.path()).is_none(),
        "legacy absence adapter stays compatible"
    );
    assert_eq!(std::fs::read_dir(cache.path()).unwrap().count(), 0);
}

#[test]
fn successful_native_setup_keeps_read_before_write_order() {
    let peer = Peer::new();
    let stage = Cell::new(0);
    let result = Client::connect_with_setup(
        &peer.transport,
        peer.cache.path(),
        crate::ipc::settings::ClientSpec::DEFAULTS.ipc_io_timeout(),
        |_| {
            assert_eq!(stage.replace(1), 0);
            Ok(())
        },
        |_| {
            assert_eq!(stage.replace(2), 1);
            Ok(())
        },
    )
    .unwrap();
    assert!(result.is_some());
    assert_eq!(stage.get(), 2);
    drop(result);
}
