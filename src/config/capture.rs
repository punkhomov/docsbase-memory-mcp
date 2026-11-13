//! Shared byte I/O only. Each caller retains its own source/admission policy.

use std::io::Read;
use std::path::Path;

#[derive(Debug, thiserror::Error)]
pub(crate) enum ReadFailure {
    #[error("read failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("source is over the trusted byte limit")]
    LimitExceeded,
    #[error("trusted byte budget overflow")]
    BudgetOverflow,
}

/// Consume at most budget+1 bytes. Metadata never determines buffer sizing.
pub(crate) fn read_bounded(reader: impl Read, budget: u64) -> Result<Vec<u8>, ReadFailure> {
    let maximum = budget.checked_add(1).ok_or(ReadFailure::BudgetOverflow)?;
    let _ = usize::try_from(maximum).map_err(|_| ReadFailure::BudgetOverflow)?;
    let mut bytes = Vec::new();
    reader.take(maximum).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > budget {
        return Err(ReadFailure::LimitExceeded);
    }
    Ok(bytes)
}

/// Legacy/diagnostic adapter: follows paths as before; observer policy is separate.
pub(crate) fn read_file(path: &Path, budget: u64) -> Result<Vec<u8>, ReadFailure> {
    read_file_with(path, budget, || {})
}
fn read_file_with(
    path: &Path,
    budget: u64,
    after_metadata: impl FnOnce(),
) -> Result<Vec<u8>, ReadFailure> {
    if std::fs::metadata(path).is_ok_and(|metadata| metadata.len() > budget) {
        return Err(ReadFailure::LimitExceeded);
    }
    after_metadata();
    read_bounded(std::fs::File::open(path)?, budget)
}

#[cfg(test)]
mod tests {
    use std::io::{self, Read};

    use super::*;

    struct CountingRead {
        remaining: usize,
        consumed: usize,
    }
    impl Read for CountingRead {
        fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
            let len = bytes.len().min(self.remaining);
            bytes[..len].fill(b' ');
            self.remaining -= len;
            self.consumed += len;
            Ok(len)
        }
    }
    #[test]
    fn unknown_length_stream_consumes_at_most_trusted_budget_plus_one() {
        let mut stream = CountingRead {
            remaining: 100,
            consumed: 0,
        };
        assert!(matches!(
            read_bounded(&mut stream, 7),
            Err(ReadFailure::LimitExceeded)
        ));
        assert_eq!(stream.consumed, 8);
    }
    #[test]
    fn complete_boundary_input_is_retained_without_truncation() {
        for len in [0, 6, 7] {
            let mut stream = CountingRead {
                remaining: len,
                consumed: 0,
            };
            assert_eq!(read_bounded(&mut stream, 7).unwrap().len(), len);
            assert_eq!(stream.consumed, len);
        }
    }
    #[test]
    fn overflowing_budget_is_rejected_before_any_read() {
        let mut stream = CountingRead {
            remaining: 100,
            consumed: 0,
        };
        assert!(matches!(
            read_bounded(&mut stream, u64::MAX),
            Err(ReadFailure::BudgetOverflow)
        ));
        assert_eq!(stream.consumed, 0);
    }
    #[test]
    fn growth_after_metadata_admission_cannot_bypass_the_read_bound() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, b" ").unwrap();
        let result = read_file_with(&path, 7, || {
            std::fs::write(&path, b"                ").unwrap();
        });
        assert!(matches!(result, Err(ReadFailure::LimitExceeded)));
    }
    #[test]
    fn oversized_metadata_is_only_an_early_rejection() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, b"                ").unwrap();
        let result = read_file_with(&path, 7, || {
            panic!("oversized metadata must reject before reading")
        });
        assert!(matches!(result, Err(ReadFailure::LimitExceeded)));
    }
}
