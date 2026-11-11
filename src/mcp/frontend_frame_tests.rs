//! Tests the real stdin wrapper, including delimiter boundaries within one read.

use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::CappedRead;
use crate::ipc::protocol::MAX_FRAME_BYTES;

#[tokio::test]
async fn delimited_oversize_frame_is_rejected_in_one_poll() {
    tokio::time::timeout(Duration::from_secs(300), async {
        let mut payload = vec![b'x'; MAX_FRAME_BYTES + 1];
        payload.push(b'\n');
        let (mut input, stream) = tokio::io::duplex(payload.len());
        input
            .write_all(&payload)
            .await
            .expect("complete frame ready");
        let mut capped = CappedRead::new(stream, MAX_FRAME_BYTES);
        let mut bytes = vec![0; payload.len()];
        let error = capped
            .read(&mut bytes)
            .await
            .expect_err("oversized delimited frame must be refused");
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    })
    .await
    .expect("frame watchdog");
}

#[tokio::test]
async fn exact_limit_frame_preserves_acceptance_of_next_valid_frame() {
    tokio::time::timeout(Duration::from_secs(300), async {
        let mut payload = vec![b'x'; MAX_FRAME_BYTES];
        payload.extend_from_slice(b"\nok\n");
        let (mut input, stream) = tokio::io::duplex(payload.len());
        input.write_all(&payload).await.expect("valid frames ready");
        let mut capped = CappedRead::new(stream, MAX_FRAME_BYTES);
        let mut bytes = vec![0; payload.len()];
        let read = capped
            .read(&mut bytes)
            .await
            .expect("valid frames accepted");
        assert_eq!(read, payload.len());
        assert_eq!(bytes, payload);
    })
    .await
    .expect("exact limit watchdog");
}

#[tokio::test]
async fn multiple_valid_frames_do_not_share_one_length_budget() {
    tokio::time::timeout(Duration::from_secs(300), async {
        let payload = b"12345678\nabcdef\n";
        let (mut input, stream) = tokio::io::duplex(payload.len());
        input.write_all(payload).await.expect("valid frames ready");
        let mut capped = CappedRead::new(stream, 8);
        let mut bytes = [0; 64];
        let read = capped.read(&mut bytes).await.expect("per-frame budget");
        assert_eq!(&bytes[..read], payload);
    })
    .await
    .expect("multiple frames watchdog");
}

#[tokio::test]
async fn fragmented_oversize_frame_is_rejected_before_delimiter_reset() {
    tokio::time::timeout(Duration::from_secs(300), async {
        let (mut input, stream) = tokio::io::duplex(64);
        let mut capped = CappedRead::new(stream, 8);
        let mut bytes = [0; 64];
        input.write_all(b"12345678").await.expect("prefix");
        assert_eq!(capped.read(&mut bytes).await.expect("at limit"), 8);
        input
            .write_all(b"9\n")
            .await
            .expect("oversize suffix and delimiter");
        let error = capped
            .read(&mut bytes)
            .await
            .expect_err("oversize must not reset");
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    })
    .await
    .expect("fragmented frame watchdog");
}
