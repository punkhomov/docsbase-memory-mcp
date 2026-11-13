//! Post-connect setup must never become permission to index directly.
use std::cell::Cell;
use std::path::Path;

use crate::error::{Error, Result};
use crate::ipc::client::Client;
use crate::platform;

// A bound listener is sufficient: setup callbacks run only after native connect.
// No accepted-stream IO is needed to classify the constructor's result.
struct Peer {
    _listener: platform::BlockingListener,
    cache: tempfile::TempDir,
}
impl Peer {
    fn new() -> Self {
        let cache = tempfile::tempdir().unwrap();
        platform::fs::secure_dir(&cache.path().join("state")).unwrap();
        let listener = platform::bind_blocking(&platform::daemon_endpoint(cache.path())).unwrap();
        Self {
            _listener: listener,
            cache,
        }
    }
}

fn injected_setup(cache: &Path, fail_read: bool) -> Result<Option<Client>> {
    Client::connect_with_setup(
        cache,
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
        let error = injected_setup(peer.cache.path(), fail_read).err();
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
    for fail_read in [true, false] {
        let peer = Peer::new();
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("guide.md"), "replacementwidget").unwrap();
        let connection = injected_setup(peer.cache.path(), fail_read);
        let result = super::sync_with_connection(
            peer.cache.path(),
            root.path(),
            &crate::store::settings::standalone().unwrap(),
            connection,
            || {
                std::fs::write(
                    peer.cache.path().join("registry.db"),
                    b"unexpected direct indexing",
                )?;
                std::fs::create_dir(peer.cache.path().join("projects"))?;
                Ok(())
            },
        );
        let registry_created = peer.cache.path().join("registry.db").exists();
        let Err(error) = result else {
            panic!(
                "connected setup failure replayed CLI sync; registry_created={registry_created}"
            );
        };
        assert!(
            matches!(error.downcast_ref::<Error>(), Some(Error::Transport { .. })),
            "{error}"
        );
        assert!(error.to_string().contains(if fail_read {
            "injected read"
        } else {
            "injected write"
        }));
        for name in ["registry.db", "projects", "logs", "vectors.db"] {
            assert!(
                !peer.cache.path().join(name).exists(),
                "connected failure created {name}"
            );
        }
    }
}

#[test]
fn absent_connect_never_runs_setup_or_creates_state() {
    let cache = tempfile::tempdir().unwrap();
    let result = Client::connect_with_setup(
        cache.path(),
        |_| panic!("read setup without a connection"),
        |_| panic!("write setup without a connection"),
    );
    assert!(matches!(result, Ok(None)));
    assert!(matches!(Client::try_connect(cache.path()), Ok(None)));
    assert!(
        Client::connect(cache.path()).is_none(),
        "legacy absence adapter stays compatible"
    );
    assert_eq!(std::fs::read_dir(cache.path()).unwrap().count(), 0);
}

#[test]
fn successful_native_setup_keeps_read_before_write_order() {
    let peer = Peer::new();
    let stage = Cell::new(0);
    let result = Client::connect_with_setup(
        peer.cache.path(),
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
