//! `FName` in the shapes engine builds use.
//!
//! The shape depends on build options, so a generated SDK aliases exactly one
//! of these as `FName`, chosen from the dump's `FNameLayout`.

use ufathom_mem::Pod;

/// `FName` of 4 bytes: a single index, with the number kept outside.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct FName4 {
    /// Index into the name table.
    pub comparison_index: u32,
}

/// `FName` of 8 bytes: index and instance number.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct FName8 {
    /// Index into the name table.
    pub comparison_index: u32,
    /// Instance number, one more than the suffix of `Name_N`; 0 for none.
    pub number: u32,
}

/// `FName` of 12 bytes on case-preserving builds, UE 4 order:
/// comparison index, display index, number.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct FName12 {
    /// Case-insensitive index.
    pub comparison_index: u32,
    /// Case-preserving index.
    pub display_index: u32,
    /// Instance number.
    pub number: u32,
}

/// `FName` of 12 bytes in the UE 5 order: comparison index, number, display
/// index.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct FName12Ue5 {
    /// Case-insensitive index.
    pub comparison_index: u32,
    /// Instance number.
    pub number: u32,
    /// Case-preserving index.
    pub display_index: u32,
}

/// An `FName` whose field order is none of the known shapes: `N` raw bytes.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FNameBytes<const N: usize>(pub [u8; N]);

impl<const N: usize> Default for FNameBytes<N> {
    fn default() -> Self {
        Self([0; N])
    }
}

// SAFETY: every field is a plain integer or byte array, valid for every bit pattern,
// and the structs have no padding.
unsafe impl Pod for FName4 {}
// SAFETY: as above.
unsafe impl Pod for FName8 {}
// SAFETY: as above.
unsafe impl Pod for FName12 {}
// SAFETY: as above.
unsafe impl Pod for FName12Ue5 {}
// SAFETY: a byte array.
unsafe impl<const N: usize> Pod for FNameBytes<N> {}

const _: () = assert!(size_of::<FName4>() == 4 && align_of::<FName4>() == 4);
const _: () = assert!(size_of::<FName8>() == 8 && align_of::<FName8>() == 4);
const _: () = assert!(size_of::<FName12>() == 12 && align_of::<FName12>() == 4);
const _: () = assert!(size_of::<FName12Ue5>() == 12 && align_of::<FName12Ue5>() == 4);
const _: () = assert!(size_of::<FNameBytes<16>>() == 16 && align_of::<FNameBytes<16>>() == 1);
const _: () = assert!(core::mem::offset_of!(FName12, display_index) == 4);
const _: () = assert!(core::mem::offset_of!(FName12, number) == 8);
const _: () = assert!(core::mem::offset_of!(FName12Ue5, number) == 4);
const _: () = assert!(core::mem::offset_of!(FName12Ue5, display_index) == 8);
