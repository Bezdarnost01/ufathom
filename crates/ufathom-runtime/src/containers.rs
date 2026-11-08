//! Engine containers and handles as plain `Pod` views.
//!
//! Sizes and alignments are those of a 64-bit build and are asserted below.
//! They match the common engine layout, but a build may differ; the code
//! generator compares the size a dump reports for a property with the size
//! of the view and falls back to raw bytes when they disagree.

use core::fmt;
use core::marker::PhantomData;

use ufathom_mem::{MemError, MemoryReader, MemoryReaderExt, Pod};

use crate::ptr::Ptr;

/// `TArray<T>`: a data pointer, an element count and a capacity.
///
/// Layout: data at 0, `num` at 8, `max` at 12. Size 16, alignment 8.
#[repr(C)]
pub struct TArray<T> {
    /// Address of the first element.
    pub data: Ptr<T>,
    /// Number of elements in use.
    pub num: i32,
    /// Number of elements allocated.
    pub max: i32,
}

impl<T> TArray<T> {
    /// An empty array with no allocation.
    pub const EMPTY: Self = Self {
        data: Ptr::NULL,
        num: 0,
        max: 0,
    };

    /// The element count, or 0 when it is negative.
    #[must_use]
    pub fn len(&self) -> usize {
        usize::try_from(self.num).unwrap_or(0)
    }

    /// `true` when there are no elements.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// `true` when the header is plausible: `0 <= num <= max`, and a non-null
    /// data pointer unless the array is empty.
    #[must_use]
    pub fn is_consistent(&self) -> bool {
        0 <= self.num && self.num <= self.max && (self.num == 0 || !self.data.is_null())
    }

    /// Address of element `index`, or `None` when out of range or the
    /// address overflows.
    #[must_use]
    pub fn element_addr(&self, index: usize) -> Option<u64> {
        if index >= self.len() {
            return None;
        }
        let offset = u64::try_from(index.checked_mul(size_of::<T>())?).ok()?;
        self.data.addr().checked_add(offset)
    }
}

impl<T: Pod> TArray<T> {
    /// Reads every element through `mem`.
    ///
    /// # Errors
    ///
    /// - [`MemError::InvalidFormat`] if the header is inconsistent.
    /// - Any error of the reader, including the limits of `read_vec`, so a
    ///   hostile count cannot allocate without bound.
    pub fn read_all<M: MemoryReader + ?Sized>(&self, mem: &M) -> Result<Vec<T>, MemError> {
        if !self.is_consistent() {
            return Err(MemError::InvalidFormat {
                format: "TArray",
                reason: format!(
                    "num {} max {} data {:#x}",
                    self.num,
                    self.max,
                    self.data.addr()
                ),
            });
        }
        if self.is_empty() {
            return Ok(Vec::new());
        }
        mem.read_vec::<T>(self.data.addr(), self.len())
    }
}

impl<T> Clone for TArray<T> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<T> Copy for TArray<T> {}
impl<T> Default for TArray<T> {
    fn default() -> Self {
        Self::EMPTY
    }
}
impl<T> PartialEq for TArray<T> {
    fn eq(&self, other: &Self) -> bool {
        self.data == other.data && self.num == other.num && self.max == other.max
    }
}
impl<T> Eq for TArray<T> {}
impl<T> fmt::Debug for TArray<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "TArray {{ data: {:#x}, num: {}, max: {} }}",
            self.data.addr(),
            self.num,
            self.max
        )
    }
}
// SAFETY: a pointer and two integers, valid for every bit pattern, with no padding.
unsafe impl<T: 'static> Pod for TArray<T> {}

/// `FString`: a `TArray<u16>` of UTF-16 code units, normally ending in a NUL.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FString {
    /// The characters.
    pub chars: TArray<u16>,
}

impl FString {
    /// Reads the string through `mem`. Reading stops at the first NUL and
    /// invalid UTF-16 is replaced.
    ///
    /// # Errors
    ///
    /// The errors of [`TArray::read_all`].
    pub fn read<M: MemoryReader + ?Sized>(&self, mem: &M) -> Result<String, MemError> {
        let units = self.chars.read_all(mem)?;
        let end = units.iter().position(|&u| u == 0).unwrap_or(units.len());
        Ok(String::from_utf16_lossy(units.get(..end).unwrap_or(&[])))
    }
}
// SAFETY: a transparent `TArray<u16>`.
unsafe impl Pod for FString {}

/// `TSet<T>`: an opaque view of the sparse array plus hash that the engine
/// uses.
///
/// Layout: 0x50 bytes, alignment 8 (a `TSparseArray` of 0x38 bytes, then the
/// inline-or-heap hash of 0x18 bytes). Only the size is part of the contract;
/// the element type is a marker.
#[repr(C)]
pub struct TSet<T> {
    raw: [u64; 10],
    marker: PhantomData<fn() -> T>,
}

/// `TMap<K, V>`: a `TSet` of key and value pairs, so the same 0x50 bytes with
/// alignment 8. The key and value types are markers.
#[repr(C)]
pub struct TMap<K, V> {
    raw: [u64; 10],
    marker: PhantomData<fn() -> (K, V)>,
}

macro_rules! opaque_view {
    ($name:ident<$($p:ident),+>) => {
        impl<$($p),+> $name<$($p),+> {
            /// An all-zero (empty) view.
            pub const EMPTY: Self = Self { raw: [0; 10], marker: PhantomData };

            /// The raw words of the view.
            #[must_use]
            pub const fn raw(&self) -> &[u64; 10] {
                &self.raw
            }
        }
        impl<$($p),+> Clone for $name<$($p),+> {
            fn clone(&self) -> Self {
                *self
            }
        }
        impl<$($p),+> Copy for $name<$($p),+> {}
        impl<$($p),+> Default for $name<$($p),+> {
            fn default() -> Self {
                Self::EMPTY
            }
        }
        impl<$($p),+> fmt::Debug for $name<$($p),+> {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(concat!(stringify!($name), " { .. }"))
            }
        }
        // SAFETY: plain words, valid for every bit pattern, no padding.
        unsafe impl<$($p: 'static),+> Pod for $name<$($p),+> {}
    };
}
opaque_view!(TSet<T>);
opaque_view!(TMap<K, V>);

/// `FText`: opaque, 0x18 bytes, alignment 8 (a shared pointer to the text data
/// and flags).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FText {
    raw: [u64; 3],
}
// SAFETY: plain words.
unsafe impl Pod for FText {}

/// `FWeakObjectPtr`: an object array index and the serial number that tells
/// whether the slot still holds the same object.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct FWeakObjectPtr {
    /// Index in the global object array.
    pub object_index: i32,
    /// Serial number of the slot when the pointer was made.
    pub object_serial: i32,
}
// SAFETY: two integers.
unsafe impl Pod for FWeakObjectPtr {}

/// `FLazyObjectPtr`: a weak pointer, the tag at the last test and the GUID of
/// the target. 28 bytes, alignment 4 (as measured on a UE 4.25 build).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct FLazyObjectPtr {
    /// The weak pointer part.
    pub weak: FWeakObjectPtr,
    /// Tag at the last test of the weak pointer.
    pub tag_at_last_test: i32,
    /// The GUID, as four words.
    pub guid: [u32; 4],
}
// SAFETY: integers only.
unsafe impl Pod for FLazyObjectPtr {}

/// `FSoftObjectPath`: an asset path name and a sub path. `N` is the build's
/// `FName`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FSoftObjectPath<N> {
    /// Name of the asset.
    pub asset_path_name: N,
    /// Path of the sub object.
    pub sub_path_string: FString,
}
// SAFETY: fields are `Pod`; the layout is asserted for the known `FName` shapes.
// Alignment gaps are never read by this crate.
unsafe impl<N: Pod> Pod for FSoftObjectPath<N> {}

/// `FSoftObjectPtr`: a weak pointer, a tag and a path. Size 40 with an 8-byte
/// `FName`, 48 with a 12-byte one; alignment 8.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FSoftObjectPtr<N> {
    /// Cached weak pointer to the loaded object.
    pub weak: FWeakObjectPtr,
    /// Tag at the last test of the weak pointer.
    pub tag_at_last_test: i32,
    /// The asset path.
    pub path: FSoftObjectPath<N>,
}
// SAFETY: as for `FSoftObjectPath`.
unsafe impl<N: Pod> Pod for FSoftObjectPtr<N> {}

/// `FScriptDelegate`: a weak pointer to the object and the function name.
/// `N` is the build's `FName`. Alignment 4.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FScriptDelegate<N> {
    /// The bound object.
    pub object: FWeakObjectPtr,
    /// The bound function.
    pub function_name: N,
}
// SAFETY: fields are `Pod` with no padding for 4-aligned names.
unsafe impl<N: Pod> Pod for FScriptDelegate<N> {}

/// A multicast delegate stored inline: an array of bindings. Size 16.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FMulticastInlineDelegate<N: 'static> {
    /// The bindings.
    pub invocation_list: TArray<FScriptDelegate<N>>,
}
// SAFETY: a transparent-like single `TArray`.
unsafe impl<N: Pod> Pod for FMulticastInlineDelegate<N> {}

/// A sparse multicast delegate: an empty wrapper of one byte.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FMulticastSparseDelegate {
    raw: [u8; 1],
}
// SAFETY: a byte.
unsafe impl Pod for FMulticastSparseDelegate {}

/// `FScriptInterface`: the object pointer and the pointer to the interface
/// vtable part. Size 16.
#[repr(C)]
pub struct FScriptInterface<T> {
    /// The object.
    pub object: Ptr<T>,
    /// The interface sub-object address inside the object.
    pub interface: Ptr<u8>,
}
impl<T> Clone for FScriptInterface<T> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<T> Copy for FScriptInterface<T> {}
impl<T> Default for FScriptInterface<T> {
    fn default() -> Self {
        Self {
            object: Ptr::NULL,
            interface: Ptr::NULL,
        }
    }
}
impl<T> fmt::Debug for FScriptInterface<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "FScriptInterface {{ object: {:#x}, interface: {:#x} }}",
            self.object.addr(),
            self.interface.addr()
        )
    }
}
// SAFETY: two pointers.
unsafe impl<T: 'static> Pod for FScriptInterface<T> {}

#[allow(dead_code, reason = "holds compile-time layout assertions only")]
mod layout_checks {
    use super::*;
    use crate::name::{FName8, FName12};
    use core::mem::offset_of;

    const fn check(size: usize, align: usize, actual_size: usize, actual_align: usize) {
        assert!(size == actual_size && align == actual_align);
    }

    const _: () = check(16, 8, size_of::<TArray<u8>>(), align_of::<TArray<u8>>());
    const _: () = check(16, 8, size_of::<FString>(), align_of::<FString>());
    const _: () = check(0x50, 8, size_of::<TSet<u8>>(), align_of::<TSet<u8>>());
    const _: () = check(
        0x50,
        8,
        size_of::<TMap<u8, u8>>(),
        align_of::<TMap<u8, u8>>(),
    );
    const _: () = check(0x18, 8, size_of::<FText>(), align_of::<FText>());
    const _: () = check(
        8,
        4,
        size_of::<FWeakObjectPtr>(),
        align_of::<FWeakObjectPtr>(),
    );
    const _: () = check(
        28,
        4,
        size_of::<FLazyObjectPtr>(),
        align_of::<FLazyObjectPtr>(),
    );
    const _: () = check(
        40,
        8,
        size_of::<FSoftObjectPtr<FName8>>(),
        align_of::<FSoftObjectPtr<FName8>>(),
    );
    const _: () = check(
        48,
        8,
        size_of::<FSoftObjectPtr<FName12>>(),
        align_of::<FSoftObjectPtr<FName12>>(),
    );
    const _: () = check(
        16,
        4,
        size_of::<FScriptDelegate<FName8>>(),
        align_of::<FScriptDelegate<FName8>>(),
    );
    const _: () = check(
        20,
        4,
        size_of::<FScriptDelegate<FName12>>(),
        align_of::<FScriptDelegate<FName12>>(),
    );
    const _: () = check(
        16,
        8,
        size_of::<FMulticastInlineDelegate<FName8>>(),
        align_of::<FMulticastInlineDelegate<FName8>>(),
    );
    const _: () = check(
        1,
        1,
        size_of::<FMulticastSparseDelegate>(),
        align_of::<FMulticastSparseDelegate>(),
    );
    const _: () = check(
        16,
        8,
        size_of::<FScriptInterface<u8>>(),
        align_of::<FScriptInterface<u8>>(),
    );
    const _: () = assert!(offset_of!(TArray<u8>, num) == 8 && offset_of!(TArray<u8>, max) == 12);
    const _: () = assert!(offset_of!(FWeakObjectPtr, object_serial) == 4);
}

#[cfg(test)]
mod tests {
    use super::*;
    use ufathom_mem::Snapshot;

    fn snapshot() -> Snapshot {
        let mut data = vec![0u8; 0x100];
        for (i, v) in [1u32, 2, 3].iter().enumerate() {
            data[0x10 + i * 4..0x14 + i * 4].copy_from_slice(&v.to_le_bytes());
        }
        for (i, u) in "Hi\0".encode_utf16().enumerate() {
            data[0x40 + i * 2..0x42 + i * 2].copy_from_slice(&u.to_le_bytes());
        }
        Snapshot::builder().region(0x1000, data).build().unwrap()
    }

    #[test]
    fn tarray_reads_elements_and_checks_header() {
        let mem = snapshot();
        let arr = TArray::<u32> {
            data: Ptr::new(0x1010),
            num: 3,
            max: 4,
        };
        assert_eq!(arr.read_all(&mem).unwrap(), vec![1, 2, 3]);
        assert_eq!(arr.element_addr(2), Some(0x1018));
        assert_eq!(arr.element_addr(3), None);
        let bad = TArray::<u32> {
            data: Ptr::new(0x1010),
            num: 5,
            max: 4,
        };
        assert!(matches!(
            bad.read_all(&mem),
            Err(MemError::InvalidFormat { .. })
        ));
        let negative = TArray::<u32> {
            data: Ptr::NULL,
            num: -1,
            max: 0,
        };
        assert!(negative.read_all(&mem).is_err());
        assert!(TArray::<u32>::EMPTY.read_all(&mem).unwrap().is_empty());
    }

    #[test]
    fn tarray_unreadable_data_is_an_error() {
        let mem = snapshot();
        let arr = TArray::<u32> {
            data: Ptr::new(0x9000),
            num: 1,
            max: 1,
        };
        assert!(arr.read_all(&mem).is_err());
    }

    #[test]
    fn fstring_reads_up_to_nul() {
        let mem = snapshot();
        let s = FString {
            chars: TArray {
                data: Ptr::new(0x1040),
                num: 3,
                max: 3,
            },
        };
        assert_eq!(s.read(&mem).unwrap(), "Hi");
    }
}
