//! Plain data describing a target address space.

use crate::Addr;

/// Access rights of a memory region.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Perms {
    /// Pages can be read. Guarded and no-access pages report `false`.
    pub read: bool,
    /// Pages can be written.
    pub write: bool,
    /// Pages can be executed.
    pub execute: bool,
}

impl Perms {
    /// No access.
    pub const NONE: Self = Self::new(false, false, false);
    /// Read only.
    pub const R: Self = Self::new(true, false, false);
    /// Read and write.
    pub const RW: Self = Self::new(true, true, false);
    /// Read and execute.
    pub const RX: Self = Self::new(true, false, true);
    /// Read, write and execute.
    pub const RWX: Self = Self::new(true, true, true);

    /// Builds a permission set.
    pub const fn new(read: bool, write: bool, execute: bool) -> Self {
        Self {
            read,
            write,
            execute,
        }
    }
}

/// What backs a memory region.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum RegionKind {
    /// An executable image such as an EXE, DLL or shared object.
    Image,
    /// A mapped file or shared section that is not an image.
    Mapped,
    /// Private memory such as heap, stack or committed allocations.
    Private,
    /// The source does not say.
    #[default]
    Unknown,
}

/// A contiguous range of address space with uniform attributes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MemoryRegion {
    /// First address of the region.
    pub base: Addr,
    /// Size in bytes.
    pub size: u64,
    /// Access rights.
    pub perms: Perms,
    /// What backs the region.
    pub kind: RegionKind,
}

impl MemoryRegion {
    /// Exclusive end address, or `None` if it does not fit in 64 bits.
    pub fn end(&self) -> Option<Addr> {
        self.base.checked_add(self.size)
    }

    /// Whether `addr` lies inside the region.
    pub fn contains(&self, addr: Addr) -> bool {
        addr >= self.base && addr - self.base < self.size
    }
}

/// A module (executable image) loaded in the target.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModuleInfo {
    /// File name, such as `game.exe`.
    pub name: String,
    /// Full path when the source knows it.
    pub path: Option<String>,
    /// Load address.
    pub base: Addr,
    /// Size of the mapped image in bytes.
    pub size: u64,
}

/// A running process, as listed by [`crate::list_processes`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProcessInfo {
    /// Process identifier.
    pub pid: u32,
    /// Executable file name.
    pub name: String,
    /// Full image path, or `None` when the system refused to reveal it.
    pub path: Option<String>,
}

/// Returns the last component of a Windows or Unix path.
pub(crate) fn file_name(path: &str) -> &str {
    path.rsplit(['\\', '/']).next().unwrap_or(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn region_contains_handles_edges() {
        let r = MemoryRegion {
            base: u64::MAX - 0xf,
            size: 0xf,
            perms: Perms::R,
            kind: RegionKind::Private,
        };
        assert!(r.contains(u64::MAX - 1));
        assert!(!r.contains(u64::MAX));
        assert!(!r.contains(u64::MAX - 0x10));
        assert_eq!(r.end(), Some(u64::MAX));
        // A region that reaches the very last byte has no representable end.
        let top = MemoryRegion { size: 0x10, ..r };
        assert!(top.contains(u64::MAX));
        assert_eq!(top.end(), None);
    }

    #[test]
    fn file_name_splits_both_separators() {
        assert_eq!(file_name(r"C:\a\b\game.exe"), "game.exe");
        assert_eq!(file_name("/usr/lib/libc.so.6"), "libc.so.6");
        assert_eq!(file_name("plain"), "plain");
    }
}
