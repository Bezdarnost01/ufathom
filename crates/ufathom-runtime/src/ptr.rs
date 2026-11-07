//! A typed 64-bit address that is plain data.

use core::fmt;
use core::hash::{Hash, Hasher};
use core::marker::PhantomData;

use ufathom_mem::Pod;

/// A 64-bit address of a `T` in the address space of the target.
///
/// It is `Send + Sync + Copy` whatever `T` is, is never dereferenced by this
/// crate except through the explicit [`Ptr::as_ref`], and is valid for every
/// bit pattern, so it can sit inside a [`Pod`] struct read from remote memory.
#[repr(transparent)]
pub struct Ptr<T> {
    addr: u64,
    marker: PhantomData<fn() -> T>,
}

impl<T> Ptr<T> {
    /// The null pointer.
    pub const NULL: Self = Self::new(0);

    /// Wraps an address.
    #[must_use]
    pub const fn new(addr: u64) -> Self {
        Self {
            addr,
            marker: PhantomData,
        }
    }

    /// `true` if the address is zero.
    #[must_use]
    pub const fn is_null(self) -> bool {
        self.addr == 0
    }

    /// The raw address.
    #[must_use]
    pub const fn addr(self) -> u64 {
        self.addr
    }

    /// Reinterprets the address as pointing to a `U`. Checks nothing.
    #[must_use]
    pub const fn cast<U>(self) -> Ptr<U> {
        Ptr::new(self.addr)
    }

    /// The address of a live reference. Meaningful inside the target process.
    #[must_use]
    pub fn from_ref(value: &T) -> Self {
        Self::new(core::ptr::from_ref(value) as usize as u64)
    }

    /// Turns the address into a reference. Returns `None` for null, for an
    /// address that is not aligned for `T`, and for an address that does not
    /// fit in `usize`.
    ///
    /// # Safety
    ///
    /// This is for code running inside the target process only. The caller
    /// guarantees that, for the whole lifetime `'a`:
    ///
    /// - the address is the address of a live, initialised `T` in this
    ///   process (an address read from a remote reader or a stale snapshot is
    ///   not);
    /// - the memory is not freed or moved, and is not written through a path
    ///   that breaks Rust's aliasing rules. The engine may free any object at
    ///   any frame, so the reference must only be held while that cannot
    ///   happen, typically on the game thread within one call;
    /// - the bytes there are a valid `T`.
    #[must_use]
    pub unsafe fn as_ref<'a>(self) -> Option<&'a T> {
        let raw = usize::try_from(self.addr).ok()?;
        if raw == 0 || raw % align_of::<T>() != 0 {
            return None;
        }
        // SAFETY: the caller promises the address is a live `T` valid for `'a`
        // (see the function contract); null and misalignment were rejected above.
        Some(unsafe { &*(raw as *const T) })
    }
}

impl<T> Clone for Ptr<T> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<T> Copy for Ptr<T> {}
impl<T> Default for Ptr<T> {
    fn default() -> Self {
        Self::NULL
    }
}
impl<T> PartialEq for Ptr<T> {
    fn eq(&self, other: &Self) -> bool {
        self.addr == other.addr
    }
}
impl<T> Eq for Ptr<T> {}
impl<T> PartialOrd for Ptr<T> {
    fn partial_cmp(&self, other: &Self) -> Option<core::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl<T> Ord for Ptr<T> {
    fn cmp(&self, other: &Self) -> core::cmp::Ordering {
        self.addr.cmp(&other.addr)
    }
}
impl<T> Hash for Ptr<T> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.addr.hash(state);
    }
}
impl<T> fmt::Debug for Ptr<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Ptr({:#x})", self.addr)
    }
}

// SAFETY: a `u64` plus a zero-sized marker; every bit pattern is a valid address.
unsafe impl<T: 'static> Pod for Ptr<T> {}

const _: () = assert!(size_of::<Ptr<u8>>() == 8 && align_of::<Ptr<u8>>() == 8);

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_send_sync<T: Send + Sync + Copy>() {}

    #[test]
    fn is_send_sync_copy_for_any_target() {
        struct NotSend(#[allow(dead_code, reason = "only the type matters")] *const u8);
        assert_send_sync::<Ptr<NotSend>>();
        assert_send_sync::<Ptr<core::cell::Cell<u8>>>();
    }

    #[test]
    fn null_cast_and_addr() {
        let p = Ptr::<u32>::new(0x1000);
        assert_eq!(p.addr(), 0x1000);
        assert!(!p.is_null());
        assert!(Ptr::<u32>::NULL.is_null());
        assert_eq!(p.cast::<u8>().addr(), 0x1000);
    }

    #[test]
    fn as_ref_rejects_null_and_misaligned() {
        // SAFETY: null and misaligned addresses are rejected before any dereference.
        unsafe {
            assert!(Ptr::<u32>::NULL.as_ref().is_none());
            assert!(Ptr::<u32>::new(0x1001).as_ref().is_none());
        }
    }

    #[test]
    fn as_ref_reads_a_live_value() {
        let value = 0xABCD_u32;
        let p = Ptr::from_ref(&value);
        // SAFETY: `value` is a live `u32` for the whole use of the reference.
        let r = unsafe { p.as_ref() };
        assert_eq!(r, Some(&0xABCD));
    }
}
