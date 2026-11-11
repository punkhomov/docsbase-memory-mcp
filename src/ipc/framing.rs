//! Bounded NDJSON state retained across cancelled asynchronous reads.

use tokio::io::{AsyncBufRead, AsyncBufReadExt};

use super::protocol::MAX_FRAME_BYTES;

/// One frame; malformed terminal frames make the caller close its connection.
#[derive(Debug)]
pub(crate) enum Frame {
    Line(Vec<u8>),
    Eof,
    Incomplete,
    TooLarge,
}

/// Pending bytes belong to the connection, not a disposable read future.
pub(crate) struct FrameReader<R> {
    inner: R,
    pending: Vec<u8>,
}

impl<R: AsyncBufRead + Unpin> FrameReader<R> {
    #[must_use]
    pub(crate) fn new(inner: R) -> Self {
        Self {
            inner,
            pending: Vec::new(),
        }
    }

    /// Reads one capped frame. Dropping this future preserves partial input.
    ///
    /// # Errors
    /// Returns underlying IO errors; the caller owns the connection policy.
    pub(crate) async fn read_frame(&mut self) -> std::io::Result<Frame> {
        loop {
            let available = self.inner.fill_buf().await?;
            if available.is_empty() {
                return Ok(if self.pending.is_empty() {
                    Frame::Eof
                } else {
                    Frame::Incomplete
                });
            }
            let remaining = MAX_FRAME_BYTES + 2 - self.pending.len();
            let available = &available[..available.len().min(remaining)];
            let newline = available.iter().position(|byte| *byte == b'\n');
            let consumed = newline.map_or(available.len(), |offset| offset + 1);
            // No cancellation point between retaining and consuming the bytes.
            self.pending.extend_from_slice(&available[..consumed]);
            self.inner.consume(consumed);
            if self.pending.len() > MAX_FRAME_BYTES + 1 {
                return Ok(Frame::TooLarge);
            }
            if newline.is_some() {
                return Ok(Frame::Line(std::mem::take(&mut self.pending)));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::future::{Future as _, poll_fn};
    use std::task::Poll;

    use tokio::io::{AsyncWriteExt, BufReader};

    use super::*;

    #[tokio::test]
    async fn fragmented_frame_survives_cancelled_read() {
        let (mut peer, stream) = tokio::io::duplex(64);
        let mut reader = FrameReader::new(BufReader::new(stream));
        peer.write_all(b"{\"query\":").await.expect("prefix");
        let mut pending = Box::pin(reader.read_frame());
        poll_fn(|cx| {
            assert!(pending.as_mut().poll(cx).is_pending());
            Poll::Ready(())
        })
        .await;
        drop(pending);
        assert!(reader.inner.buffer().is_empty(), "prefix was consumed");
        peer.write_all(b"\"widgets\"}\n").await.expect("suffix");
        match reader.read_frame().await.expect("resumed frame") {
            Frame::Line(line) => assert_eq!(line, b"{\"query\":\"widgets\"}\n"),
            frame => panic!("expected complete frame, got {frame:?}"),
        }
    }
}
