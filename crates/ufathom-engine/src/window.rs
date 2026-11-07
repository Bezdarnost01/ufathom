//! Reading several nearby fields of one target object with few reads.

use ufathom_mem::{Addr, MemoryReader};

use crate::error::EngineError;

/// Fields closer than this share one read.
const MERGE_GAP: u32 = 64;

/// Largest single read a window performs. Offsets come from a layout, which
/// may be a candidate still being tested.
const MAX_SEGMENT: u32 = 0x1_0000;

/// Bytes of one object read as a few segments, indexed by offset from the
/// object start.
#[derive(Debug)]
pub(crate) struct Window {
    addr: Addr,
    what: &'static str,
    segments: Vec<(u32, Vec<u8>)>,
}

impl Window {
    /// Reads the byte ranges `(offset, length)` of the object at `addr`.
    ///
    /// Ranges within [`MERGE_GAP`] bytes of each other are read together.
    pub(crate) fn read(
        mem: &dyn MemoryReader,
        addr: Addr,
        what: &'static str,
        spans: &[(u32, u32)],
    ) -> Result<Self, EngineError> {
        let mut sorted: Vec<(u32, u32)> = Vec::with_capacity(spans.len());
        for &(offset, len) in spans {
            let end = offset
                .checked_add(len)
                .ok_or_else(|| EngineError::InvalidLayout {
                    what,
                    reason: format!("field at {offset:#x} with length {len} overflows"),
                })?;
            sorted.push((offset, end));
        }
        sorted.sort_unstable();
        let mut merged: Vec<(u32, u32)> = Vec::new();
        for (start, end) in sorted {
            match merged.last_mut() {
                Some(last) if start <= last.1.saturating_add(MERGE_GAP) => {
                    last.1 = last.1.max(end);
                }
                _ => merged.push((start, end)),
            }
        }
        let mut segments = Vec::with_capacity(merged.len());
        for (start, end) in merged {
            let at = addr
                .checked_add(u64::from(start))
                .ok_or(EngineError::Inconsistent {
                    addr,
                    what,
                    reason: "field address overflows".to_owned(),
                })?;
            if end - start > MAX_SEGMENT {
                return Err(EngineError::InvalidLayout {
                    what,
                    reason: format!("a single read of {} bytes", end - start),
                });
            }
            let mut bytes = vec![0u8; (end - start) as usize];
            mem.read_exact(at, &mut bytes)
                .map_err(EngineError::read(at, what))?;
            segments.push((start, bytes));
        }
        Ok(Self {
            addr,
            what,
            segments,
        })
    }

    /// The bytes at `offset`, or an error when the range was not read.
    pub(crate) fn bytes(&self, offset: u32, len: u32) -> Result<&[u8], EngineError> {
        let end = offset
            .checked_add(len)
            .ok_or_else(|| self.outside(offset, len))?;
        for (start, bytes) in &self.segments {
            let segment_end = start.saturating_add(bytes.len() as u32);
            if offset >= *start && end <= segment_end {
                let from = (offset - start) as usize;
                return bytes
                    .get(from..from + len as usize)
                    .ok_or_else(|| self.outside(offset, len));
            }
        }
        Err(self.outside(offset, len))
    }

    fn outside(&self, offset: u32, len: u32) -> EngineError {
        EngineError::Inconsistent {
            addr: self.addr,
            what: self.what,
            reason: format!("field {offset:#x}+{len} was not part of the read"),
        }
    }

    pub(crate) fn u8(&self, offset: u32) -> Result<u8, EngineError> {
        Ok(self.bytes(offset, 1)?.first().copied().unwrap_or(0))
    }

    pub(crate) fn u16(&self, offset: u32) -> Result<u16, EngineError> {
        let bytes: [u8; 2] = self
            .bytes(offset, 2)?
            .try_into()
            .map_err(|_| self.outside(offset, 2))?;
        Ok(u16::from_le_bytes(bytes))
    }

    pub(crate) fn u32(&self, offset: u32) -> Result<u32, EngineError> {
        let bytes: [u8; 4] = self
            .bytes(offset, 4)?
            .try_into()
            .map_err(|_| self.outside(offset, 4))?;
        Ok(u32::from_le_bytes(bytes))
    }

    pub(crate) fn u64(&self, offset: u32) -> Result<u64, EngineError> {
        let bytes: [u8; 8] = self
            .bytes(offset, 8)?
            .try_into()
            .map_err(|_| self.outside(offset, 8))?;
        Ok(u64::from_le_bytes(bytes))
    }
}
