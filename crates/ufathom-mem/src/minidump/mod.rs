//! Offline reader for full-memory minidump files.
//!
//! Every offset, size and count in the file is untrusted. All of them are
//! checked against the file length with checked arithmetic before use, and a
//! malformed file returns [`MemError::InvalidFormat`], never a panic.

mod bytes;
mod parse;

use std::fs::File;
use std::path::Path;

use memmap2::Mmap;

use crate::error::{MemError, range_end};
use crate::reader::{Addr, MemoryReader};
use crate::types::{MemoryRegion, ModuleInfo};

/// Where the dump bytes live.
enum Backing {
    Mapped(Mmap),
    Owned(Vec<u8>),
}

impl Backing {
    fn bytes(&self) -> &[u8] {
        match self {
            Self::Mapped(map) => map,
            Self::Owned(vec) => vec,
        }
    }
}

/// A contiguous run of dumped memory.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct DumpRange {
    pub(crate) start: Addr,
    pub(crate) len: u64,
    pub(crate) file_offset: u64,
}

/// Reads memory out of a minidump.
///
/// Understands `MemoryListStream`, `Memory64ListStream`, `ModuleListStream`
/// and `MemoryInfoListStream`. Reads are served from the memory-mapped file
/// and succeed only where the dump holds bytes: a hole, such as memory that was
/// not captured, is an error. Region permissions come from the memory info
/// list when present; without it, regions are the dumped ranges, marked
/// readable with unknown kind.
///
/// The file is mapped, so it must not be truncated or modified by another
/// program while the reader is alive.
pub struct MinidumpMemory {
    backing: Backing,
    ranges: Vec<DumpRange>,
    regions: Vec<MemoryRegion>,
    modules: Vec<ModuleInfo>,
}

impl MinidumpMemory {
    /// Opens and parses the minidump at `path`.
    ///
    /// # Errors
    ///
    /// [`MemError::Io`] if the file cannot be opened or mapped, and
    /// [`MemError::InvalidFormat`] if its contents are malformed.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, MemError> {
        let file = File::open(path.as_ref())?;
        if file.metadata()?.len() == 0 {
            return Err(MemError::invalid("minidump", "file is empty"));
        }
        // SAFETY: the mapping is read-only. If another program truncates or
        // rewrites the file while it is mapped, reads may fault; this is the
        // documented contract of the type, and the file is an offline artifact
        // that tools do not modify in place.
        let map = unsafe { Mmap::map(&file)? };
        Self::from_backing(Backing::Mapped(map))
    }

    /// Parses a minidump held in memory.
    ///
    /// # Errors
    ///
    /// [`MemError::InvalidFormat`] if the contents are malformed.
    pub fn from_bytes(bytes: impl Into<Vec<u8>>) -> Result<Self, MemError> {
        Self::from_backing(Backing::Owned(bytes.into()))
    }

    fn from_backing(backing: Backing) -> Result<Self, MemError> {
        let parsed = parse::parse(backing.bytes())?;
        Ok(Self {
            backing,
            ranges: parsed.ranges,
            regions: parsed.regions,
            modules: parsed.modules,
        })
    }

    /// Finds the dumped range that contains `addr`.
    fn find(&self, addr: Addr) -> Option<&DumpRange> {
        let after = self.ranges.partition_point(|range| range.start <= addr);
        let candidate = self.ranges.get(after.checked_sub(1)?)?;
        (addr - candidate.start < candidate.len).then_some(candidate)
    }
}

impl MemoryReader for MinidumpMemory {
    fn read_exact(&self, addr: Addr, buf: &mut [u8]) -> Result<(), MemError> {
        let end = range_end(addr, buf.len())?;
        let file = self.backing.bytes();
        let mut cursor = addr;
        let mut done = 0usize;
        while done < buf.len() {
            let fail = || MemError::Unreadable {
                addr: cursor,
                len: end - cursor,
            };
            let range = self.find(cursor).ok_or_else(fail)?;
            let offset_in_range = cursor - range.start;
            let available = range.len - offset_in_range;
            let take =
                usize::try_from(available.min((buf.len() - done) as u64)).map_err(|_| fail())?;
            let from = range
                .file_offset
                .checked_add(offset_in_range)
                .ok_or_else(fail)?;
            let from = usize::try_from(from).map_err(|_| fail())?;
            let source = file
                .get(from..)
                .and_then(|rest| rest.get(..take))
                .ok_or_else(fail)?;
            buf.get_mut(done..done + take)
                .ok_or_else(fail)?
                .copy_from_slice(source);
            done += take;
            cursor += take as u64;
        }
        Ok(())
    }

    fn regions(&self) -> Result<Vec<MemoryRegion>, MemError> {
        Ok(self.regions.clone())
    }

    fn modules(&self) -> Result<Vec<ModuleInfo>, MemError> {
        Ok(self.modules.clone())
    }
}

#[cfg(test)]
mod tests;
