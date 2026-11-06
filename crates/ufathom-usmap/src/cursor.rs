//! A bounds-checked little-endian reader over a byte slice.

use crate::error::UsmapError;

/// Reads from a slice and tracks the position. Every read is checked, so a
/// short or hostile input becomes [`UsmapError::Truncated`].
pub(crate) struct Cursor<'a> {
    bytes: &'a [u8],
    position: usize,
    /// Added to the position when reporting an offset, so that a body slice
    /// of a file still reports file offsets.
    base: usize,
}

impl<'a> Cursor<'a> {
    pub(crate) fn new(bytes: &'a [u8], base: usize) -> Self {
        Self {
            bytes,
            position: 0,
            base,
        }
    }

    /// The offset to put in an error for the next byte.
    pub(crate) fn offset(&self) -> usize {
        self.base.saturating_add(self.position)
    }

    /// Bytes not read yet.
    pub(crate) fn remaining(&self) -> usize {
        self.bytes.len().saturating_sub(self.position)
    }

    pub(crate) fn take(&mut self, count: usize) -> Result<&'a [u8], UsmapError> {
        let available = self.remaining();
        let start = self.position;
        let slice = start
            .checked_add(count)
            .and_then(|end| self.bytes.get(start..end))
            .ok_or(UsmapError::Truncated {
                offset: self.offset(),
                needed: count,
                available,
            })?;
        self.position = self.position.saturating_add(count);
        Ok(slice)
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N], UsmapError> {
        let offset = self.offset();
        let available = self.remaining();
        <[u8; N]>::try_from(self.take(N)?).map_err(|_| UsmapError::Truncated {
            offset,
            needed: N,
            available,
        })
    }

    pub(crate) fn u8(&mut self) -> Result<u8, UsmapError> {
        Ok(u8::from_le_bytes(self.array()?))
    }

    pub(crate) fn u16(&mut self) -> Result<u16, UsmapError> {
        Ok(u16::from_le_bytes(self.array()?))
    }

    pub(crate) fn u32(&mut self) -> Result<u32, UsmapError> {
        Ok(u32::from_le_bytes(self.array()?))
    }

    pub(crate) fn i32(&mut self) -> Result<i32, UsmapError> {
        Ok(i32::from_le_bytes(self.array()?))
    }

    pub(crate) fn i64(&mut self) -> Result<i64, UsmapError> {
        Ok(i64::from_le_bytes(self.array()?))
    }

    pub(crate) fn guid(&mut self) -> Result<[u8; 16], UsmapError> {
        self.array()
    }

    /// Reads a list length and rejects it when `count` elements of at least
    /// `min_element_size` bytes cannot fit in what is left. This runs before
    /// any allocation sized by the count.
    pub(crate) fn count(
        &mut self,
        what: &'static str,
        min_element_size: usize,
    ) -> Result<usize, UsmapError> {
        let offset = self.offset();
        let raw = self.u32()?;
        self.check_count(what, u64::from(raw), offset, min_element_size)
    }

    /// Same as [`count`](Self::count) for a count already read.
    pub(crate) fn check_count(
        &self,
        what: &'static str,
        count: u64,
        offset: usize,
        min_element_size: usize,
    ) -> Result<usize, UsmapError> {
        let available = self.remaining();
        let fits = usize::try_from(count).ok().filter(|&n| {
            n.checked_mul(min_element_size)
                .is_some_and(|b| b <= available)
        });
        fits.ok_or(UsmapError::CountTooLarge {
            what,
            count,
            offset,
            available,
        })
    }
}
