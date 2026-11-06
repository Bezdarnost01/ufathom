//! An in-memory address space, used by tests and by synthetic fixtures.

use crate::error::{MemError, range_end};
use crate::reader::{Addr, MemoryReader};
use crate::types::{MemoryRegion, ModuleInfo, Perms, RegionKind};

/// A region together with its bytes.
#[derive(Debug, Clone)]
struct Mapped {
    info: MemoryRegion,
    data: Vec<u8>,
}

/// An immutable address space assembled with [`Snapshot::builder`].
///
/// Regions are sorted by base address and never overlap. A read may span
/// adjacent regions, but a hole anywhere in the range, or a region without read
/// permission, makes the whole read fail.
#[derive(Debug, Clone, Default)]
pub struct Snapshot {
    regions: Vec<Mapped>,
    modules: Vec<ModuleInfo>,
}

impl Snapshot {
    /// Starts building a snapshot.
    pub fn builder() -> SnapshotBuilder {
        SnapshotBuilder::default()
    }

    /// Finds the region that contains `addr`.
    fn find(&self, addr: Addr) -> Option<&Mapped> {
        let after = self.regions.partition_point(|m| m.info.base <= addr);
        let candidate = self.regions.get(after.checked_sub(1)?)?;
        candidate.info.contains(addr).then_some(candidate)
    }
}

impl MemoryReader for Snapshot {
    fn read_exact(&self, addr: Addr, buf: &mut [u8]) -> Result<(), MemError> {
        let end = range_end(addr, buf.len())?;
        let mut cursor = addr;
        let mut done = 0usize;
        while done < buf.len() {
            let fail = || MemError::Unreadable {
                addr: cursor,
                len: end - cursor,
            };
            let mapped = self
                .find(cursor)
                .filter(|m| m.info.perms.read)
                .ok_or_else(fail)?;
            let offset = usize::try_from(cursor - mapped.info.base).map_err(|_| fail())?;
            let source = mapped.data.get(offset..).ok_or_else(fail)?;
            let take = source.len().min(buf.len() - done);
            let from = source.get(..take).ok_or_else(fail)?;
            buf.get_mut(done..done + take)
                .ok_or_else(fail)?
                .copy_from_slice(from);
            done += take;
            cursor += take as u64;
        }
        Ok(())
    }

    fn regions(&self) -> Result<Vec<MemoryRegion>, MemError> {
        Ok(self.regions.iter().map(|m| m.info.clone()).collect())
    }

    fn modules(&self) -> Result<Vec<ModuleInfo>, MemError> {
        Ok(self.modules.clone())
    }
}

/// A region waiting to be validated by [`SnapshotBuilder::build`].
#[derive(Debug)]
struct PendingRegion {
    base: Addr,
    data: Vec<u8>,
    perms: Perms,
    kind: RegionKind,
}

/// Builder for [`Snapshot`]. Problems are reported by [`SnapshotBuilder::build`],
/// so calls can be chained without handling errors.
#[derive(Debug, Default)]
pub struct SnapshotBuilder {
    pending: Vec<PendingRegion>,
    modules: Vec<ModuleInfo>,
}

impl SnapshotBuilder {
    /// Adds a readable and writable private region holding `data`.
    #[must_use]
    pub fn region(self, base: Addr, data: impl Into<Vec<u8>>) -> Self {
        self.region_with(base, data, Perms::RW, RegionKind::Private)
    }

    /// Adds a region with explicit permissions and kind.
    #[must_use]
    pub fn region_with(
        mut self,
        base: Addr,
        data: impl Into<Vec<u8>>,
        perms: Perms,
        kind: RegionKind,
    ) -> Self {
        self.pending.push(PendingRegion {
            base,
            data: data.into(),
            perms,
            kind,
        });
        self
    }

    /// Adds a zero-filled readable and writable private region of `size` bytes.
    #[must_use]
    pub fn zeroed(self, base: Addr, size: usize) -> Self {
        self.region(base, vec![0u8; size])
    }

    /// Adds a module that has no path.
    #[must_use]
    pub fn module(mut self, name: &str, base: Addr, size: u64) -> Self {
        self.modules.push(ModuleInfo {
            name: name.to_owned(),
            path: None,
            base,
            size,
        });
        self
    }

    /// Adds a module with a full path. The name is the last path component.
    #[must_use]
    pub fn module_with_path(mut self, path: &str, base: Addr, size: u64) -> Self {
        self.modules.push(ModuleInfo {
            name: crate::types::file_name(path).to_owned(),
            path: Some(path.to_owned()),
            base,
            size,
        });
        self
    }

    /// Validates the regions and produces the snapshot.
    ///
    /// # Errors
    ///
    /// [`MemError::InvalidRegion`] for an empty region, a region whose end does
    /// not fit in 64 bits, or two overlapping regions.
    pub fn build(self) -> Result<Snapshot, MemError> {
        let Self {
            mut pending,
            mut modules,
        } = self;
        pending.sort_by_key(|region| region.base);
        let mut regions: Vec<Mapped> = Vec::with_capacity(pending.len());
        for region in pending {
            let size = region.data.len() as u64;
            let invalid = |reason: &str| MemError::InvalidRegion {
                base: region.base,
                size,
                reason: reason.to_owned(),
            };
            if size == 0 {
                return Err(invalid("region is empty"));
            }
            if region.base.checked_add(size).is_none() {
                return Err(invalid("region end overflows the address space"));
            }
            if let Some(prev) = regions.last()
                && prev.info.end().is_none_or(|end| end > region.base)
            {
                return Err(invalid(&format!(
                    "overlaps region at {:#x}",
                    prev.info.base
                )));
            }
            regions.push(Mapped {
                info: MemoryRegion {
                    base: region.base,
                    size,
                    perms: region.perms,
                    kind: region.kind,
                },
                data: region.data,
            });
        }
        modules.sort_by(|a, b| a.base.cmp(&b.base).then_with(|| a.name.cmp(&b.name)));
        Ok(Snapshot { regions, modules })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MemoryReaderExt;

    #[test]
    fn reads_within_one_region() {
        let snap = Snapshot::builder()
            .region(0x1000, [1u8, 2, 3, 4])
            .build()
            .unwrap();
        let mut buf = [0u8; 2];
        snap.read_exact(0x1001, &mut buf).unwrap();
        assert_eq!(buf, [2, 3]);
    }

    #[test]
    fn reads_span_adjacent_regions_in_any_insertion_order() {
        let snap = Snapshot::builder()
            .region(0x1004, [5u8, 6, 7, 8])
            .region(0x1000, [1u8, 2, 3, 4])
            .region(0x1008, [9u8, 10])
            .build()
            .unwrap();
        let mut buf = [0u8; 10];
        snap.read_exact(0x1000, &mut buf).unwrap();
        assert_eq!(buf, [1, 2, 3, 4, 5, 6, 7, 8, 9, 10]);
        let regions = snap.regions().unwrap();
        let bases: Vec<_> = regions.iter().map(|r| r.base).collect();
        assert_eq!(bases, [0x1000, 0x1004, 0x1008]);
    }

    #[test]
    fn holes_and_edges_are_errors() {
        let snap = Snapshot::builder()
            .region(0x1000, [1u8; 4])
            .region(0x1005, [2u8; 4])
            .build()
            .unwrap();
        let mut buf = [0u8; 6];
        // Spans the one byte hole at 0x1004.
        let err = snap.read_exact(0x1000, &mut buf).unwrap_err();
        assert!(
            matches!(err, MemError::Unreadable { addr: 0x1004, .. }),
            "{err}"
        );
        // Starts before the first region.
        assert!(snap.read_exact(0xfff, &mut buf[..2]).is_err());
        // Runs past the end of the last region.
        assert!(snap.read_exact(0x1008, &mut buf[..2]).is_err());
        // Whole read inside the second region still works.
        assert!(snap.read_exact(0x1005, &mut buf[..4]).is_ok());
    }

    #[test]
    fn empty_read_always_succeeds() {
        let snap = Snapshot::default();
        assert!(snap.read_exact(0x1234, &mut []).is_ok());
        assert!(snap.read_exact(u64::MAX, &mut []).is_ok());
    }

    #[test]
    fn overflow_near_the_top_of_the_address_space() {
        let snap = Snapshot::builder()
            .region(u64::MAX - 0xf, [7u8; 0xf])
            .build()
            .unwrap();
        let mut buf = [0u8; 4];
        snap.read_exact(u64::MAX - 0xf, &mut buf).unwrap();
        assert_eq!(buf, [7; 4]);
        assert!(matches!(
            snap.read_exact(u64::MAX, &mut buf[..1]),
            Err(MemError::AddressOverflow { .. })
        ));
        assert!(matches!(
            snap.read_exact(u64::MAX - 1, &mut buf),
            Err(MemError::AddressOverflow { .. })
        ));
        // The last mapped byte is u64::MAX - 1 and can be read on its own.
        assert!(snap.read_exact(u64::MAX - 1, &mut buf[..1]).is_ok());
        assert!(snap.read_exact(u64::MAX - 2, &mut buf[..2]).is_ok());
    }

    #[test]
    fn overlap_is_rejected() {
        let overlapping = Snapshot::builder()
            .region(0x1000, [0u8; 0x10])
            .region(0x1008, [0u8; 0x10])
            .build();
        assert!(matches!(overlapping, Err(MemError::InvalidRegion { .. })));
        let contained = Snapshot::builder()
            .region(0x1000, [0u8; 0x100])
            .region(0x1010, [0u8; 4])
            .build();
        assert!(matches!(contained, Err(MemError::InvalidRegion { .. })));
        let identical = Snapshot::builder()
            .region(0x1000, [0u8; 4])
            .region(0x1000, [0u8; 4])
            .build();
        assert!(identical.is_err());
        let touching = Snapshot::builder()
            .region(0x1000, [0u8; 0x10])
            .region(0x1010, [0u8; 0x10])
            .build();
        assert!(touching.is_ok());
    }

    #[test]
    fn empty_and_overflowing_regions_are_rejected() {
        assert!(
            Snapshot::builder()
                .region(0x1000, Vec::new())
                .build()
                .is_err()
        );
        assert!(
            Snapshot::builder()
                .region(u64::MAX - 1, [0u8; 4])
                .build()
                .is_err()
        );
    }

    #[test]
    fn unreadable_regions_fail_reads_but_are_listed() {
        let snap = Snapshot::builder()
            .region_with(0x1000, [1u8; 8], Perms::NONE, RegionKind::Private)
            .region_with(0x2000, [2u8; 8], Perms::RX, RegionKind::Image)
            .build()
            .unwrap();
        assert!(snap.read_u8(0x1000).is_err());
        assert_eq!(snap.read_u8(0x2000).unwrap(), 2);
        let regions = snap.regions().unwrap();
        assert_eq!(regions.len(), 2);
        assert_eq!(regions[1].perms, Perms::RX);
        assert_eq!(regions[1].kind, RegionKind::Image);
    }

    #[test]
    fn modules_are_sorted_and_named() {
        let snap = Snapshot::builder()
            .module_with_path(r"C:\game\game.exe", 0x2000, 0x100)
            .module("a.dll", 0x1000, 0x10)
            .build()
            .unwrap();
        let modules = snap.modules().unwrap();
        assert_eq!(modules[0].name, "a.dll");
        assert_eq!(modules[1].name, "game.exe");
        assert_eq!(modules[1].path.as_deref(), Some(r"C:\game\game.exe"));
    }
}
