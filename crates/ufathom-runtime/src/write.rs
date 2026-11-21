//! Checked writes to fields of live game objects: [`LiveObject`].
//!
//! Only compiled with the `inproc` feature, and only for a
//! [`Session`] over [`SelfMemory`]. An external session (`ProcessMemory`,
//! a minidump, a snapshot) has no way to write: the capability does not exist
//! for those readers, so the dump path can never reach it.
//!
//! Every write goes through [`SelfMemory::write_exact`], the one write in
//! `ufathom-mem`, after three checks:
//!
//! 1. the object is non-null, readable and still sits in the object array slot
//!    its `InternalIndex` names (the check `InProcessExecutor` makes before a
//!    call), so a freed object or one whose slot was reused is refused;
//! 2. the field lies entirely inside the object, measured by the
//!    `PropertiesSize` of the object's own class (not of the static type
//!    `T`), so a write cannot spill past the object;
//! 3. `write_exact` itself, which refuses memory that is not committed
//!    read-write and never changes page protection.
//!
//! # Threads
//!
//! These checks narrow the window; they do not close it. The game keeps
//! running while a mod writes, so a write from another thread races with the
//! engine: the object can be freed between the check and the write, and the
//! engine can read or write the same bytes at the same moment (a torn `f32` is
//! unlikely, a torn struct is not). `modify` and `set_bit` are read, change,
//! write sequences and are **not atomic** against the game thread: an update
//! the engine makes in between is overwritten. Do writes on the game thread,
//! in [`GameThread::run`](crate::inproc::GameThread::run) or inside a hook
//! callback. The type system does not enforce this.
//!
//! A slot also has no per-object serial that the object itself could be
//! compared with, so "the same object as before" means "the same address,
//! still in the slot its index names". An object freed and re-created at the
//! same address in the same slot cannot be told apart.

use core::fmt;
use core::marker::PhantomData;

use ufathom_mem::{MemError, MemoryReaderExt, Pod, SelfMemory};

use crate::object::ObjectRef;
use crate::ptr::Ptr;
use crate::session::{Session, SessionError};
use crate::traits::{BitRef, FieldRef, IsA};

/// Why a write to a game object did not happen.
///
/// Whatever the cause, nothing was written.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum WriteError {
    /// The object is null, unreadable, or no longer in the object array slot
    /// its `InternalIndex` names: it was freed, or its slot was reused.
    #[error("object {addr:#x} is not alive")]
    DeadObject {
        /// The object address.
        addr: u64,
    },
    /// The field does not lie inside the object. This is a wrong offset, or a
    /// field of a type the object is not an instance of.
    #[error(
        "field at offset {offset:#x} ({size:#x} bytes) is outside object {addr:#x}, whose class is {class_size:#x} bytes"
    )]
    OutOfBounds {
        /// The object address.
        addr: u64,
        /// Offset of the field from the object start.
        offset: u32,
        /// Size of the write in bytes.
        size: usize,
        /// `PropertiesSize` of the object's class.
        class_size: usize,
    },
    /// A bit write was asked with an empty mask, which selects no bit.
    #[error("bit mask is zero")]
    EmptyMask,
    /// The object or its class could not be inspected.
    #[error("cannot inspect object {addr:#x}: {source}")]
    Inspect {
        /// The object address.
        addr: u64,
        /// The session's error.
        #[source]
        source: SessionError,
    },
    /// The memory access failed: the page is not writable, the address
    /// overflows, or the OS refused.
    #[error(transparent)]
    Memory(#[from] MemError),
}

/// A live game object that can be read and written, from
/// [`Session::live_object`].
///
/// It holds the session so that every write can check the object is still
/// alive. See the [module documentation](self) for the checks and for the
/// thread-safety caveats: do writes on the game thread.
pub struct LiveObject<'s, T> {
    session: &'s Session<SelfMemory>,
    addr: u64,
    marker: PhantomData<fn() -> T>,
}

impl Session<SelfMemory> {
    /// A handle on the object at `ptr` that can write its fields.
    ///
    /// Nothing is checked here: every call on the handle checks the object
    /// again, so a handle kept across frames is safe to use (it returns
    /// [`WriteError::DeadObject`] once the object is gone).
    pub fn live_object<T>(&self, ptr: Ptr<T>) -> LiveObject<'_, T> {
        LiveObject {
            session: self,
            addr: ptr.addr(),
            marker: PhantomData,
        }
    }
}

impl<'s, T> LiveObject<'s, T> {
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

    /// The same object as a read-only [`ObjectRef`].
    #[must_use]
    pub fn read_only(&self) -> ObjectRef<'s, T> {
        self.session.object(self.ptr())
    }

    /// Writes one field. `T` must derive from (or be) the owner of the field.
    ///
    /// The value's bytes are written as they are, so a [`UBool`](crate::UBool)
    /// or an enum newtype stores exactly the raw representation it holds and
    /// reads back unchanged. Use [`set_bit`](Self::set_bit) for a bit-field
    /// `bool`.
    ///
    /// # Errors
    ///
    /// [`WriteError::DeadObject`] if the object is gone,
    /// [`WriteError::OutOfBounds`] if the field is not inside the object,
    /// [`WriteError::Memory`] if the page is not writable.
    pub fn set<Owner, F: Pod>(&self, field: FieldRef<Owner, F>, value: F) -> Result<(), WriteError>
    where
        T: IsA<Owner>,
    {
        let at = self.checked_addr(field.offset(), size_of::<F>())?;
        self.session
            .memory()
            .write_exact(at, pod_bytes(&value))
            .map_err(WriteError::Memory)
    }

    /// Reads a field, changes it with `change` and writes it back. Returns the
    /// value the field had before.
    ///
    /// The object is checked before `change` runs, and again before the
    /// write. The three steps are **not atomic** against the game thread: if
    /// the engine changes the field after the read, that change is lost. Run it
    /// on the game thread.
    ///
    /// # Errors
    ///
    /// As [`set`](Self::set), and [`WriteError::Memory`] if the read fails.
    /// On error `change` may or may not have run, and nothing was written.
    pub fn modify<Owner, F: Pod>(
        &self,
        field: FieldRef<Owner, F>,
        change: impl FnOnce(&mut F),
    ) -> Result<F, WriteError>
    where
        T: IsA<Owner>,
    {
        let at = self.checked_addr(field.offset(), size_of::<F>())?;
        let before: F = self.session.memory().read_pod(at)?;
        let mut after = before;
        change(&mut after);
        self.set(field, after)?;
        Ok(before)
    }

    /// Sets or clears one bit-field `bool`.
    ///
    /// Reads the byte that holds the bit, changes only the bits in the mask,
    /// and writes that one byte back, so neighbouring bits keep the value they
    /// had at the read. Like [`modify`](Self::modify) it is **not atomic**
    /// against the game thread: a neighbouring bit the engine flips in
    /// between is overwritten with its old value.
    ///
    /// # Errors
    ///
    /// As [`set`](Self::set), and [`WriteError::EmptyMask`] for a mask of 0.
    pub fn set_bit<Owner>(&self, bit: BitRef<Owner>, value: bool) -> Result<(), WriteError>
    where
        T: IsA<Owner>,
    {
        if bit.mask() == 0 {
            return Err(WriteError::EmptyMask);
        }
        let at = self.checked_addr(bit.offset(), 1)?;
        let byte: u8 = self.session.memory().read_pod(at)?;
        let updated = if value {
            byte | bit.mask()
        } else {
            byte & !bit.mask()
        };
        self.session
            .memory()
            .write_exact(at, &[updated])
            .map_err(WriteError::Memory)
    }

    /// The address of `offset` in the object, after checking that the object
    /// is alive and that `size` bytes from `offset` lie inside it.
    fn checked_addr(&self, offset: u32, size: usize) -> Result<u64, WriteError> {
        let object = Ptr::<T>::new(self.addr);
        if !self.session.is_valid(object) {
            return Err(WriteError::DeadObject { addr: self.addr });
        }
        let inspect = |source| WriteError::Inspect {
            addr: self.addr,
            source,
        };
        let class = self.session.class_of(object).map_err(inspect)?;
        let class_size = self.session.params_size(class).map_err(inspect)?;
        let inside = usize::try_from(offset)
            .ok()
            .and_then(|offset| offset.checked_add(size))
            .is_some_and(|end| end <= class_size);
        if !inside {
            return Err(WriteError::OutOfBounds {
                addr: self.addr,
                offset,
                size,
                class_size,
            });
        }
        self.addr
            .checked_add(u64::from(offset))
            .ok_or(WriteError::Memory(MemError::AddressOverflow {
                addr: self.addr,
                len: u64::from(offset),
            }))
    }
}

impl<T> Clone for LiveObject<'_, T> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<T> Copy for LiveObject<'_, T> {}
impl<T> fmt::Debug for LiveObject<'_, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "LiveObject({:#x})", self.addr)
    }
}

/// The bytes of a [`Pod`] value, for writing it.
fn pod_bytes<T: Pod>(value: &T) -> &[u8] {
    // SAFETY: the pointer is valid for `size_of::<T>()` bytes for the lifetime
    // of the borrow, and `u8` has no alignment requirement. Primitive `Pod`
    // types have no padding, and generated structs spell their padding out as
    // explicit byte arrays, so every byte of the view is initialised.
    unsafe { core::slice::from_raw_parts(core::ptr::from_ref(value).cast::<u8>(), size_of::<T>()) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pod_bytes_is_the_raw_representation() {
        assert_eq!(pod_bytes(&0x0102_0304u32), &0x0102_0304u32.to_le_bytes());
        assert_eq!(pod_bytes(&crate::UBool(0x80)), &[0x80]);
        assert_eq!(pod_bytes(&[1u8, 2, 3]), &[1, 2, 3]);
        assert_eq!(pod_bytes(&1.5f32), &1.5f32.to_le_bytes());
    }
}
