//! Type relations and typed field offsets.

use core::fmt;
use core::marker::PhantomData;

/// Marks that `Self` starts with a valid `Base` at offset 0.
///
/// Generated classes implement it for themselves and for every ancestor, so
/// a field declared on an ancestor can be read through a derived object.
///
/// # Safety
///
/// The implementor must really begin with a `Base` at offset 0: same layout
/// for the first `size_of::<Base>()` bytes, and every field offset declared on
/// `Base` valid in `Self`. Safe code relies on this to read a field of `Base`
/// from an address that holds a `Self`.
pub unsafe trait IsA<Base> {}

/// The offset of a field of type `Field` inside an `Owner`.
///
/// Used for reads through a [`MemoryReader`](crate::MemoryReader) with
/// [`ObjectRef::get`](crate::ObjectRef::get). It carries no address, so a
/// `FieldRef` is valid whatever the Rust layout of `Owner` is.
pub struct FieldRef<Owner, Field> {
    offset: u32,
    marker: PhantomData<fn() -> (Owner, Field)>,
}

impl<Owner, Field> FieldRef<Owner, Field> {
    /// A field at `offset` bytes from the start of the owner.
    #[must_use]
    pub const fn new(offset: u32) -> Self {
        Self {
            offset,
            marker: PhantomData,
        }
    }

    /// Offset in bytes from the start of the owner.
    #[must_use]
    pub const fn offset(self) -> u32 {
        self.offset
    }
}

impl<Owner, Field> Clone for FieldRef<Owner, Field> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<Owner, Field> Copy for FieldRef<Owner, Field> {}
impl<Owner, Field> PartialEq for FieldRef<Owner, Field> {
    fn eq(&self, other: &Self) -> bool {
        self.offset == other.offset
    }
}
impl<Owner, Field> Eq for FieldRef<Owner, Field> {}
impl<Owner, Field> fmt::Debug for FieldRef<Owner, Field> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "FieldRef({:#x})", self.offset)
    }
}

/// A single bit of a bit-field `bool` of an `Owner`: the byte offset from the
/// start of the owner and the mask inside that byte.
pub struct BitRef<Owner> {
    offset: u32,
    mask: u8,
    marker: PhantomData<fn() -> Owner>,
}

impl<Owner> BitRef<Owner> {
    /// The bit selected by `mask` in the byte at `offset`.
    #[must_use]
    pub const fn new(offset: u32, mask: u8) -> Self {
        Self {
            offset,
            mask,
            marker: PhantomData,
        }
    }

    /// Offset in bytes of the byte holding the bit.
    #[must_use]
    pub const fn offset(self) -> u32 {
        self.offset
    }

    /// Mask of the bit inside that byte.
    #[must_use]
    pub const fn mask(self) -> u8 {
        self.mask
    }
}

impl<Owner> Clone for BitRef<Owner> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<Owner> Copy for BitRef<Owner> {}
impl<Owner> PartialEq for BitRef<Owner> {
    fn eq(&self, other: &Self) -> bool {
        self.offset == other.offset && self.mask == other.mask
    }
}
impl<Owner> Eq for BitRef<Owner> {}
impl<Owner> fmt::Debug for BitRef<Owner> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "BitRef({:#x}, {:#04x})", self.offset, self.mask)
    }
}
