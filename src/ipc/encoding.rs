//! Fixed output storage, reserved before JSON serialization or native copying.
use std::alloc::{Layout, alloc_zeroed, dealloc};
use std::io::{self, Write};
use std::ptr::NonNull;

use serde::Serialize;

/// Exactly the requested Rust-visible region; allocator bookkeeping is separate.
#[allow(unsafe_code)] // Fixed fallible allocation is shared with owned native slots.
pub(crate) struct Region {
    pointer: NonNull<u8>,
    layout: Layout,
}

#[allow(unsafe_code)] // Unique allocation ownership; no shared mutable references.
unsafe impl Send for Region {}

#[allow(unsafe_code)] // All pointers stay within the owned checked allocation.
impl Region {
    pub(crate) fn new(capacity: usize) -> io::Result<Self> {
        if capacity == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "zero buffer capacity",
            ));
        }
        let layout = Layout::array::<u8>(capacity).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "buffer capacity exceeds allocation range",
            )
        })?;
        // SAFETY: positive checked layout; null is handled before ownership.
        let pointer = NonNull::new(unsafe { alloc_zeroed(layout) })
            .ok_or_else(|| io::Error::from(io::ErrorKind::OutOfMemory))?;
        Ok(Self { pointer, layout })
    }
    pub(crate) fn bytes(&self) -> &[u8] {
        // SAFETY: allocation is initialized, live and exactly layout.size bytes.
        unsafe { std::slice::from_raw_parts(self.pointer.as_ptr(), self.layout.size()) }
    }
    fn bytes_mut(&mut self) -> &mut [u8] {
        // SAFETY: unique Rust access; this region has not been submitted to FFI.
        unsafe { std::slice::from_raw_parts_mut(self.pointer.as_ptr(), self.layout.size()) }
    }
}

#[allow(unsafe_code)] // Only this owner releases its positive original layout.
impl Drop for Region {
    fn drop(&mut self) {
        // SAFETY: pointer/layout match allocation, and ownership is unique.
        unsafe { dealloc(self.pointer.as_ptr(), self.layout) };
    }
}

pub(crate) struct Frame {
    storage: Region,
    length: usize,
}
impl Frame {
    pub(crate) fn new(max_json_bytes: usize) -> io::Result<Self> {
        let capacity = max_json_bytes.checked_add(1).ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "frame capacity overflow")
        })?;
        Ok(Self {
            storage: Region::new(capacity)?,
            length: 0,
        })
    }
    pub(crate) fn encode(&mut self, value: &impl Serialize) -> io::Result<()> {
        self.length = 0;
        if let Err(error) = serde_json::to_writer(&mut *self, value) {
            self.length = 0;
            return Err(io::Error::new(io::ErrorKind::InvalidData, error));
        }
        let index = self.length;
        self.storage.bytes_mut()[index] = b'\n';
        self.length += 1;
        Ok(())
    }
    pub(crate) fn bytes(&self) -> &[u8] {
        &self.storage.bytes()[..self.length]
    }
}
impl Write for Frame {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let end = self.length.checked_add(bytes.len()).ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidData, "encoded frame exceeds limit")
        })?;
        if end >= self.storage.bytes().len() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "encoded frame exceeds limit",
            ));
        }
        self.storage.bytes_mut()[self.length..end].copy_from_slice(bytes);
        self.length = end;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

pub(crate) fn encode(value: &impl Serialize) -> crate::error::Result<Frame> {
    let mut frame = Frame::new(super::protocol::MAX_FRAME_BYTES).map_err(|error| {
        crate::error::Error::internal_with_source("reserve encoded IPC frame", error)
    })?;
    frame.encode(value).map_err(|error| {
        crate::error::Error::internal_with_source("encode capped IPC frame", error)
    })?;
    Ok(frame)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn serialization_reserves_exact_capacity_and_reuses_it_after_escaped_overflow() {
        let mut frame = Frame::new(8).unwrap();
        let address = frame.storage.bytes().as_ptr();
        assert_eq!(frame.storage.bytes().len(), 9);
        frame.encode(&"123456").unwrap();
        assert_eq!(frame.bytes(), b"\"123456\"\n");
        assert!(frame.encode(&"\n\n\n\n").is_err());
        assert_eq!(frame.bytes(), []);
        frame.encode(&0).unwrap();
        assert_eq!(frame.bytes(), b"0\n");
        assert_eq!(frame.storage.bytes().as_ptr(), address);
    }
    #[test]
    fn invalid_allocation_sizes_fail_before_copying() {
        assert!(Region::new(0).is_err());
        assert!(Region::new(usize::MAX).is_err());
        assert!(Frame::new(usize::MAX).is_err());
    }
}
