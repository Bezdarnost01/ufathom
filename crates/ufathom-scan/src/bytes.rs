//! Bounds-checked little-endian reads from a byte slice.

use crate::error::ScanError;

/// A byte slice with a name for error messages. Every accessor returns an
/// error instead of panicking when the data is too short.
#[derive(Clone, Copy)]
pub(crate) struct Le<'a> {
    data: &'a [u8],
    what: &'static str,
}

impl<'a> Le<'a> {
    /// Wraps `data`; `what` names the structure in errors.
    pub(crate) fn new(data: &'a [u8], what: &'static str) -> Self {
        Self { data, what }
    }

    /// The `len` bytes at `offset`.
    pub(crate) fn slice(&self, offset: usize, len: usize) -> Result<&'a [u8], ScanError> {
        offset
            .checked_add(len)
            .and_then(|end| self.data.get(offset..end))
            .ok_or_else(|| {
                ScanError::malformed(
                    self.what,
                    format!(
                        "{len} bytes at offset {offset:#x} run past its {:#x} bytes",
                        self.data.len()
                    ),
                )
            })
    }

    /// A little-endian `u16`.
    pub(crate) fn u16(&self, offset: usize) -> Result<u16, ScanError> {
        let bytes = self.slice(offset, 2)?;
        Ok(u16::from_le_bytes([
            bytes.first().copied().unwrap_or(0),
            bytes.get(1).copied().unwrap_or(0),
        ]))
    }

    /// A little-endian `u32`.
    pub(crate) fn u32(&self, offset: usize) -> Result<u32, ScanError> {
        let bytes = self.slice(offset, 4)?;
        let mut word = [0u8; 4];
        word.copy_from_slice(bytes);
        Ok(u32::from_le_bytes(word))
    }

    /// A little-endian `u64`.
    pub(crate) fn u64(&self, offset: usize) -> Result<u64, ScanError> {
        let bytes = self.slice(offset, 8)?;
        let mut word = [0u8; 8];
        word.copy_from_slice(bytes);
        Ok(u64::from_le_bytes(word))
    }
}
