//! Strings and arrays owned by Rust, passed to game functions as inputs.
//!
//! `ProcessEvent` copies an input parameter block shallowly and never frees
//! input parameters: the caller owns them. So an input `FString` or `TArray`
//! may point at memory Rust allocated, as long as it outlives the call.
//!
//! Never use these for an out parameter (passed by non-const reference): the
//! engine would write to it and free or grow the buffer with its own
//! allocator. Pass an empty value there and let the engine allocate.

use std::sync::{Mutex, PoisonError};

use crate::containers::{FString, TArray};
use crate::ptr::Ptr;
use ufathom_mem::Pod;

/// Buffers of live `FStringBuf` and `TArrayBuf` values, as `[start, end)`,
/// so the engine is never asked to free one of them.
static RUST_OWNED: Mutex<Vec<(u64, u64)>> = Mutex::new(Vec::new());

/// Records a buffer as owned by Rust. Empty buffers are not recorded.
fn register<T>(items: &[T]) {
    let Some(range) = range_of(items) else { return };
    RUST_OWNED
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .push(range);
}

/// Forgets a buffer recorded by [`register`].
fn unregister<T>(items: &[T]) {
    let Some(range) = range_of(items) else { return };
    let mut owned = RUST_OWNED.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(at) = owned.iter().position(|&r| r == range) {
        owned.swap_remove(at);
    }
}

fn range_of<T>(items: &[T]) -> Option<(u64, u64)> {
    let bytes = core::mem::size_of_val(items) as u64;
    (bytes != 0).then(|| {
        let start = items.as_ptr() as usize as u64;
        (start, start.saturating_add(bytes))
    })
}

/// `true` when `addr` lies in the buffer of a live `FStringBuf` or
/// `TArrayBuf`.
pub(crate) fn is_rust_owned(addr: u64) -> bool {
    RUST_OWNED
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .iter()
        .any(|&(start, end)| addr >= start && addr < end)
}

/// A UTF-16, NUL-terminated string owned by Rust, viewed as an [`FString`].
///
/// ```
/// let name = ufathom_runtime::FStringBuf::new("Hello");
/// let view = name.as_fstring();
/// assert_eq!(view.chars.num, 6); // five characters and the NUL
/// ```
#[derive(Debug, PartialEq, Eq)]
pub struct FStringBuf {
    units: Vec<u16>,
}

impl FStringBuf {
    /// Encodes `text` as UTF-16 with a terminating NUL.
    #[must_use]
    pub fn new(text: &str) -> Self {
        let mut units: Vec<u16> = text.encode_utf16().collect();
        units.push(0);
        register(&units);
        Self { units }
    }

    /// The string as an `FString` input parameter. It points into `self`,
    /// so `self` must outlive every call it is passed to.
    #[must_use]
    pub fn as_fstring(&self) -> FString {
        let len = i32::try_from(self.units.len()).unwrap_or(i32::MAX);
        FString {
            chars: TArray {
                data: Ptr::new(self.units.as_ptr() as usize as u64),
                num: len,
                max: len,
            },
        }
    }

    /// The UTF-16 code units, the NUL included.
    #[must_use]
    pub fn units(&self) -> &[u16] {
        &self.units
    }
}

impl Clone for FStringBuf {
    fn clone(&self) -> Self {
        let units = self.units.clone();
        register(&units);
        Self { units }
    }
}

impl Drop for FStringBuf {
    fn drop(&mut self) {
        unregister(&self.units);
    }
}

impl From<&str> for FStringBuf {
    fn from(text: &str) -> Self {
        Self::new(text)
    }
}

/// An array of `T` owned by Rust, viewed as a [`TArray`].
#[derive(Debug, PartialEq, Eq)]
pub struct TArrayBuf<T> {
    items: Vec<T>,
}

impl<T: Pod> TArrayBuf<T> {
    /// Takes the elements.
    #[must_use]
    pub fn new(items: Vec<T>) -> Self {
        register(&items);
        Self { items }
    }

    /// The array as a `TArray` input parameter. It points into `self`, so
    /// `self` must outlive every call it is passed to.
    #[must_use]
    pub fn as_tarray(&self) -> TArray<T> {
        let len = i32::try_from(self.items.len()).unwrap_or(i32::MAX);
        TArray {
            data: if self.items.is_empty() {
                Ptr::NULL
            } else {
                Ptr::new(self.items.as_ptr() as usize as u64)
            },
            num: len,
            max: len,
        }
    }

    /// The elements.
    #[must_use]
    pub fn items(&self) -> &[T] {
        &self.items
    }
}

impl<T: Clone> Clone for TArrayBuf<T> {
    fn clone(&self) -> Self {
        let items = self.items.clone();
        register(&items);
        Self { items }
    }
}

impl<T> Drop for TArrayBuf<T> {
    fn drop(&mut self) {
        unregister(&self.items);
    }
}

impl<T: Pod> From<Vec<T>> for TArrayBuf<T> {
    fn from(items: Vec<T>) -> Self {
        Self::new(items)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mem::SelfMemory;

    #[test]
    fn fstring_buf_reads_back_through_memory() {
        let buf = FStringBuf::new("Grüße 🎮");
        let view = buf.as_fstring();
        assert_eq!(view.chars.len(), buf.units().len());
        assert!(view.chars.is_consistent());
        assert_eq!(view.read(&SelfMemory::new()).unwrap(), "Grüße 🎮");
        assert_eq!(FStringBuf::from("").as_fstring().chars.num, 1);
    }

    #[test]
    fn tarray_buf_reads_back_through_memory() {
        let buf = TArrayBuf::new(vec![1u32, 2, 3]);
        let view = buf.as_tarray();
        assert_eq!(view.read_all(&SelfMemory::new()).unwrap(), vec![1, 2, 3]);
        let empty = TArrayBuf::<u32>::new(Vec::new()).as_tarray();
        assert!(empty.data.is_null() && empty.is_consistent());
    }
}
