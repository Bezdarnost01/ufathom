//! Bounds-checked little-endian access to untrusted bytes.

use crate::error::MemError;

/// A byte buffer whose every access is checked.
#[derive(Clone, Copy)]
pub(super) struct Bytes<'a>(pub(super) &'a [u8]);

impl<'a> Bytes<'a> {
    pub(super) fn len(&self) -> u64 {
        self.0.len() as u64
    }

    /// The `len` bytes at `offset`.
    pub(super) fn slice(&self, offset: u64, len: u64, what: &str) -> Result<&'a [u8], MemError> {
        let end = offset.checked_add(len);
        let range = end.and_then(|end| {
            let start = usize::try_from(offset).ok()?;
            let end = usize::try_from(end).ok()?;
            self.0.get(start..end)
        });
        range.ok_or_else(|| {
            MemError::invalid(
                "minidump",
                format!(
                    "{what} at {offset:#x} with length {len:#x} is outside the file ({:#x} bytes)",
                    self.len()
                ),
            )
        })
    }

    /// A sub-buffer, for reading fields relative to a stream.
    pub(super) fn sub(&self, offset: u64, len: u64, what: &str) -> Result<Bytes<'a>, MemError> {
        self.slice(offset, len, what).map(Bytes)
    }

    pub(super) fn u32_at(&self, offset: u64, what: &str) -> Result<u32, MemError> {
        let raw = self.slice(offset, 4, what)?;
        let array: [u8; 4] = raw.try_into().map_err(|_| short(what, offset))?;
        Ok(u32::from_le_bytes(array))
    }

    pub(super) fn u64_at(&self, offset: u64, what: &str) -> Result<u64, MemError> {
        let raw = self.slice(offset, 8, what)?;
        let array: [u8; 8] = raw.try_into().map_err(|_| short(what, offset))?;
        Ok(u64::from_le_bytes(array))
    }
}

fn short(what: &str, offset: u64) -> MemError {
    MemError::invalid("minidump", format!("{what} at {offset:#x} is truncated"))
}
