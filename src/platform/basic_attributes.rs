//! Fixed-width basic attribute profiles of the migration control journal.
//! The caller supplies metadata from an open file handle.
use std::fs::Metadata;

#[cfg(unix)]
pub(crate) const PROFILE: &str = "unix-stat-v1";
#[cfg(windows)]
pub(crate) const PROFILE: &str = "windows-attributes-v1";

#[cfg(unix)]
pub(crate) type Encoded = [[u8; 4]; 3];
#[cfg(windows)]
pub(crate) type Encoded = [[u8; 4]; 1];

#[cfg(unix)]
pub(crate) fn encode(metadata: &Metadata) -> Encoded {
    use std::os::unix::fs::MetadataExt;
    unix_stat(metadata.uid(), metadata.gid(), metadata.mode())
}

#[cfg(unix)]
fn unix_stat(uid: u32, gid: u32, mode: u32) -> Encoded {
    [uid, gid, mode & 0o7777].map(u32::to_le_bytes)
}

#[cfg(windows)]
pub(crate) fn encode(metadata: &Metadata) -> Encoded {
    use std::os::windows::fs::MetadataExt;
    [metadata.file_attributes().to_le_bytes()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn journal_layout_preserves_uid_gid_and_special_mode_bits() {
        // Distinct IDs keep a field swap observable even on hosts with uid == gid.
        let bytes = unix_stat(0x1234_5678, 0x8765_4321, 0o104_640);
        assert_eq!(PROFILE, "unix-stat-v1");
        assert_eq!(
            bytes.as_flattened(),
            &[
                0x78, 0x56, 0x34, 0x12, 0x21, 0x43, 0x65, 0x87, 0xa0, 0x09, 0x00, 0x00
            ]
        );
    }

    #[cfg(windows)]
    #[test]
    fn journal_layout_contains_the_native_win32_mask() {
        use std::os::windows::fs::MetadataExt;
        let file = tempfile::tempfile().unwrap();
        let metadata = file.metadata().unwrap();
        assert_eq!(PROFILE, "windows-attributes-v1");
        assert_eq!(
            u32::from_le_bytes(encode(&metadata)[0]),
            metadata.file_attributes()
        );
    }
}
