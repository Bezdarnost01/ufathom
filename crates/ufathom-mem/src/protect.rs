//! Win32 page protection and allocation type values, decoded without any OS
//! call so that minidumps from Windows are understood on every platform.

use crate::types::{Perms, RegionKind};

pub(crate) const PAGE_NOACCESS: u32 = 0x01;
pub(crate) const PAGE_READONLY: u32 = 0x02;
pub(crate) const PAGE_READWRITE: u32 = 0x04;
pub(crate) const PAGE_WRITECOPY: u32 = 0x08;
pub(crate) const PAGE_EXECUTE: u32 = 0x10;
pub(crate) const PAGE_EXECUTE_READ: u32 = 0x20;
pub(crate) const PAGE_EXECUTE_READWRITE: u32 = 0x40;
pub(crate) const PAGE_EXECUTE_WRITECOPY: u32 = 0x80;
pub(crate) const PAGE_GUARD: u32 = 0x100;

/// `MEM_COMMIT` state value.
pub(crate) const MEM_COMMIT: u32 = 0x1000;
/// `MEM_PRIVATE` type value.
pub(crate) const MEM_PRIVATE: u32 = 0x2_0000;
/// `MEM_MAPPED` type value.
pub(crate) const MEM_MAPPED: u32 = 0x4_0000;
/// `MEM_IMAGE` type value.
pub(crate) const MEM_IMAGE: u32 = 0x100_0000;

/// Decodes a `Protect` field. Guarded and no-access pages are not readable.
pub(crate) fn perms_from_protect(protect: u32) -> Perms {
    if protect & (PAGE_GUARD | PAGE_NOACCESS) != 0 {
        return Perms::NONE;
    }
    let read = protect
        & (PAGE_READONLY
            | PAGE_READWRITE
            | PAGE_WRITECOPY
            | PAGE_EXECUTE_READ
            | PAGE_EXECUTE_READWRITE
            | PAGE_EXECUTE_WRITECOPY)
        != 0;
    let write = protect
        & (PAGE_READWRITE | PAGE_WRITECOPY | PAGE_EXECUTE_READWRITE | PAGE_EXECUTE_WRITECOPY)
        != 0;
    let execute = protect
        & (PAGE_EXECUTE | PAGE_EXECUTE_READ | PAGE_EXECUTE_READWRITE | PAGE_EXECUTE_WRITECOPY)
        != 0;
    Perms::new(read, write, execute)
}

/// Decodes a `Type` field.
pub(crate) fn kind_from_type(kind: u32) -> RegionKind {
    match kind {
        MEM_IMAGE => RegionKind::Image,
        MEM_MAPPED => RegionKind::Mapped,
        MEM_PRIVATE => RegionKind::Private,
        _ => RegionKind::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protection_flags_map_to_permissions() {
        assert_eq!(perms_from_protect(PAGE_READONLY), Perms::R);
        assert_eq!(perms_from_protect(PAGE_READWRITE), Perms::RW);
        assert_eq!(perms_from_protect(PAGE_EXECUTE_READ), Perms::RX);
        assert_eq!(perms_from_protect(PAGE_EXECUTE_READWRITE), Perms::RWX);
        assert_eq!(perms_from_protect(PAGE_NOACCESS), Perms::NONE);
        assert_eq!(perms_from_protect(PAGE_READWRITE | PAGE_GUARD), Perms::NONE);
        assert_eq!(
            perms_from_protect(PAGE_EXECUTE),
            Perms::new(false, false, true)
        );
    }

    #[test]
    fn types_map_to_kinds() {
        assert_eq!(kind_from_type(MEM_IMAGE), RegionKind::Image);
        assert_eq!(kind_from_type(MEM_MAPPED), RegionKind::Mapped);
        assert_eq!(kind_from_type(MEM_PRIVATE), RegionKind::Private);
        assert_eq!(kind_from_type(0), RegionKind::Unknown);
    }
}
