//! Checked reads of objects through a [`MemoryReader`].

use core::fmt;
use core::marker::PhantomData;

use ufathom_mem::{MemError, MemoryReader, MemoryReaderExt, Pod};

use crate::ptr::Ptr;
use crate::traits::{BitRef, FieldRef, IsA};

/// The base of every engine object, as far as the runtime needs it: an
/// opaque marker used where a function call takes "some object".
///
/// It has no fields. The generated SDK describes the real layout.
#[repr(C)]
pub struct UObjectBase {
    _opaque: [u8; 0],
}

/// A `T` at `addr`, read through a reader.
///
/// Nothing is read until [`read`](Self::read) or [`get`](Self::get) is
/// called, and every read is bounds-checked by the reader, so a stale or wrong
/// address is an error and never a fault.
pub struct ObjectRef<'m, T> {
    mem: &'m dyn MemoryReader,
    addr: u64,
    marker: PhantomData<fn() -> T>,
}

impl<'m, T> ObjectRef<'m, T> {
    /// Refers to the object at `addr`.
    #[must_use]
    pub fn new(mem: &'m dyn MemoryReader, addr: u64) -> Self {
        Self {
            mem,
            addr,
            marker: PhantomData,
        }
    }

    /// Refers to the object `ptr` points to. `None` for a null pointer.
    #[must_use]
    pub fn from_ptr(mem: &'m dyn MemoryReader, ptr: Ptr<T>) -> Option<Self> {
        (!ptr.is_null()).then(|| Self::new(mem, ptr.addr()))
    }

    /// The address.
    #[must_use]
    pub fn addr(&self) -> u64 {
        self.addr
    }

    /// The address as a typed pointer.
    #[must_use]
    pub fn ptr(&self) -> Ptr<T> {
        Ptr::new(self.addr)
    }

    /// Views the same object as one of its ancestors.
    #[must_use]
    pub fn upcast<B>(&self) -> ObjectRef<'m, B>
    where
        T: IsA<B>,
    {
        ObjectRef::new(self.mem, self.addr)
    }

    /// Reads one field. `T` must derive from (or be) the owner of the field.
    ///
    /// # Errors
    ///
    /// The error of the reader, or [`MemError::AddressOverflow`].
    pub fn get<Owner, F: Pod>(&self, field: FieldRef<Owner, F>) -> Result<F, MemError>
    where
        T: IsA<Owner>,
    {
        self.mem.read_pod(self.offset_addr(field.offset())?)
    }

    /// Reads one bit-field `bool`.
    ///
    /// # Errors
    ///
    /// The error of the reader, or [`MemError::AddressOverflow`].
    pub fn get_bit<Owner>(&self, bit: BitRef<Owner>) -> Result<bool, MemError>
    where
        T: IsA<Owner>,
    {
        let byte: u8 = self.mem.read_pod(self.offset_addr(bit.offset())?)?;
        Ok(byte & bit.mask() != 0)
    }

    fn offset_addr(&self, offset: u32) -> Result<u64, MemError> {
        self.addr
            .checked_add(u64::from(offset))
            .ok_or(MemError::AddressOverflow {
                addr: self.addr,
                len: u64::from(offset),
            })
    }
}

impl<T: Pod> ObjectRef<'_, T> {
    /// Reads the whole object as a `T`.
    ///
    /// # Errors
    ///
    /// The error of the reader.
    pub fn read(&self) -> Result<T, MemError> {
        self.mem.read_pod(self.addr)
    }
}

impl<T> Clone for ObjectRef<'_, T> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<T> Copy for ObjectRef<'_, T> {}
impl<T> fmt::Debug for ObjectRef<'_, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ObjectRef({:#x})", self.addr)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ufathom_mem::Snapshot;

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct Base {
        flags: u32,
        pad: u32,
    }
    // SAFETY: integers only.
    unsafe impl Pod for Base {}

    // SAFETY: reflexive.
    unsafe impl IsA<Base> for Base {}

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct Derived {
        base: Base,
        health: f32,
        bits: u8,
        pad: [u8; 3],
    }
    // SAFETY: integers, a float and bytes; no padding.
    unsafe impl Pod for Derived {}
    // SAFETY: `Derived` starts with a `Base`.
    unsafe impl IsA<Base> for Derived {}
    // SAFETY: reflexive.
    unsafe impl IsA<Derived> for Derived {}

    const FLAGS: FieldRef<Base, u32> = FieldRef::new(0);
    const HEALTH: FieldRef<Derived, f32> = FieldRef::new(8);
    const READY: BitRef<Derived> = BitRef::new(12, 0x02);
    const HIDDEN: BitRef<Derived> = BitRef::new(12, 0x01);

    fn mem() -> Snapshot {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&7u32.to_le_bytes());
        bytes.extend_from_slice(&0u32.to_le_bytes());
        bytes.extend_from_slice(&1.5f32.to_le_bytes());
        bytes.extend_from_slice(&[0x02, 0, 0, 0]);
        Snapshot::builder().region(0x4000, bytes).build().unwrap()
    }

    #[test]
    fn field_ref_reads_through_is_a() {
        let mem = mem();
        let obj = ObjectRef::<Derived>::new(&mem, 0x4000);
        assert_eq!(obj.get(HEALTH).unwrap(), 1.5);
        // `FLAGS` belongs to the base: allowed because `Derived: IsA<Base>`.
        assert_eq!(obj.get(FLAGS).unwrap(), 7);
        assert_eq!(obj.upcast::<Base>().get(FLAGS).unwrap(), 7);
        assert!(obj.get_bit(READY).unwrap());
        assert!(!obj.get_bit(HIDDEN).unwrap());
        let whole = obj.read().unwrap();
        assert_eq!(whole.base.flags, 7);
        assert_eq!(whole.health, 1.5);
        assert_eq!((whole.bits, whole.base.pad, whole.pad), (2, 0, [0; 3]));
    }

    #[test]
    fn bad_addresses_are_errors() {
        let mem = mem();
        assert!(ObjectRef::<Derived>::new(&mem, 0x9000).read().is_err());
        assert!(
            ObjectRef::<Derived>::new(&mem, u64::MAX - 2)
                .get(HEALTH)
                .is_err()
        );
        assert!(ObjectRef::<Derived>::from_ptr(&mem, Ptr::NULL).is_none());
        assert!(ObjectRef::<Derived>::from_ptr(&mem, Ptr::new(0x4000)).is_some());
    }
}
