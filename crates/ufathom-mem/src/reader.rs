//! The [`MemoryReader`] trait and its typed helpers.
//!
//! This crate has no write path: nothing here can modify the target.

use std::mem::{MaybeUninit, size_of};
use std::ptr;
use std::slice;
use std::sync::Arc;

use crate::error::MemError;
use crate::pod::Pod;
use crate::types::{MemoryRegion, ModuleInfo};

/// An address in the target's address space.
pub type Addr = u64;

/// Largest element count accepted by [`MemoryReaderExt::read_vec`].
pub const MAX_READ_VEC_COUNT: usize = 1 << 24;

/// Largest total size in bytes accepted by [`MemoryReaderExt::read_vec`].
pub const MAX_READ_VEC_BYTES: usize = 256 << 20;

/// Largest `max` accepted by [`MemoryReaderExt::read_cstr`].
pub const MAX_CSTR_LEN: usize = 1 << 20;

/// Granule in which [`MemoryReaderExt::read_cstr`] reads. It never reads across
/// an aligned boundary of this size in one call, so a string that ends just
/// before an unmapped page can still be read.
const CSTR_CHUNK: usize = 4096;

/// Read-only access to the memory of a target.
///
/// Implementations must never fault, whatever address is requested, and must
/// never return a partial result: `read_exact` fills the whole buffer or
/// returns an error.
pub trait MemoryReader: Send + Sync {
    /// Fills `buf` with the bytes at `addr`.
    ///
    /// An empty buffer always succeeds. A range that overflows the 64-bit
    /// address space returns [`MemError::AddressOverflow`]. A range that is
    /// not entirely readable returns an error and leaves the contents of
    /// `buf` unspecified.
    fn read_exact(&self, addr: Addr, buf: &mut [u8]) -> Result<(), MemError>;

    /// Lists the committed regions of the target, sorted by base address.
    fn regions(&self) -> Result<Vec<MemoryRegion>, MemError>;

    /// Lists the modules loaded in the target.
    fn modules(&self) -> Result<Vec<ModuleInfo>, MemError>;
}

macro_rules! forward_reader {
    ($($ptr:ty),* $(,)?) => {
        $(
            impl<R: MemoryReader + ?Sized> MemoryReader for $ptr {
                fn read_exact(&self, addr: Addr, buf: &mut [u8]) -> Result<(), MemError> {
                    (**self).read_exact(addr, buf)
                }
                fn regions(&self) -> Result<Vec<MemoryRegion>, MemError> {
                    (**self).regions()
                }
                fn modules(&self) -> Result<Vec<ModuleInfo>, MemError> {
                    (**self).modules()
                }
            }
        )*
    };
}

forward_reader!(&R, Box<R>, Arc<R>);

/// Typed reads built on [`MemoryReader::read_exact`]. Implemented for every reader.
pub trait MemoryReaderExt: MemoryReader {
    /// Reads one value of a [`Pod`] type.
    ///
    /// The value is only created after every byte has been read.
    fn read_pod<T: Pod>(&self, addr: Addr) -> Result<T, MemError> {
        let mut value = MaybeUninit::<T>::zeroed();
        // SAFETY: `value` is zero-initialised, so all `size_of::<T>()` bytes are
        // initialised and may be viewed as `u8`. The slice is the only access to
        // `value` and is not used after `read_exact` returns.
        let bytes =
            unsafe { slice::from_raw_parts_mut(value.as_mut_ptr().cast::<u8>(), size_of::<T>()) };
        self.read_exact(addr, bytes)?;
        // SAFETY: `T: Pod` accepts every bit pattern, and all bytes are initialised.
        Ok(unsafe { value.assume_init() })
    }

    /// Reads one byte.
    fn read_u8(&self, addr: Addr) -> Result<u8, MemError> {
        self.read_pod(addr)
    }

    /// Reads a little-endian `u16`.
    fn read_u16(&self, addr: Addr) -> Result<u16, MemError> {
        self.read_pod(addr)
    }

    /// Reads a little-endian `u32`.
    fn read_u32(&self, addr: Addr) -> Result<u32, MemError> {
        self.read_pod(addr)
    }

    /// Reads a little-endian `i32`.
    fn read_i32(&self, addr: Addr) -> Result<i32, MemError> {
        self.read_pod(addr)
    }

    /// Reads a little-endian `u64`.
    fn read_u64(&self, addr: Addr) -> Result<u64, MemError> {
        self.read_pod(addr)
    }

    /// Reads `count` consecutive values of a [`Pod`] type.
    ///
    /// `count` is checked against [`MAX_READ_VEC_COUNT`] and the total size
    /// against [`MAX_READ_VEC_BYTES`] before anything is allocated, so a
    /// hostile count from target data cannot exhaust memory.
    fn read_vec<T: Pod>(&self, addr: Addr, count: usize) -> Result<Vec<T>, MemError> {
        if count > MAX_READ_VEC_COUNT {
            return Err(MemError::LimitExceeded {
                what: "element count",
                requested: count as u64,
                limit: MAX_READ_VEC_COUNT as u64,
            });
        }
        let byte_len = count
            .checked_mul(size_of::<T>())
            .filter(|bytes| *bytes <= MAX_READ_VEC_BYTES)
            .ok_or(MemError::LimitExceeded {
                what: "byte length",
                requested: (count as u64).saturating_mul(size_of::<T>() as u64),
                limit: MAX_READ_VEC_BYTES as u64,
            })?;
        let mut values: Vec<T> = Vec::new();
        values
            .try_reserve_exact(count)
            .map_err(|_| MemError::LimitExceeded {
                what: "allocation",
                requested: byte_len as u64,
                limit: MAX_READ_VEC_BYTES as u64,
            })?;
        // SAFETY: capacity is at least `count` elements. Zero bytes are a valid
        // `T` because `T: Pod`, so after zero-filling `set_len(count)` exposes
        // only initialised, valid values.
        unsafe {
            ptr::write_bytes(values.as_mut_ptr(), 0, count);
            values.set_len(count);
        }
        // SAFETY: `values` owns `count * size_of::<T>() == byte_len` initialised
        // bytes, and the slice is the only access until it is dropped below.
        let bytes =
            unsafe { slice::from_raw_parts_mut(values.as_mut_ptr().cast::<u8>(), byte_len) };
        self.read_exact(addr, bytes)?;
        Ok(values)
    }

    /// Reads a NUL-terminated string and returns its bytes without the terminator.
    ///
    /// At most `max` bytes of content are accepted, so `max + 1` bytes may be
    /// examined. Reading proceeds in chunks that stop at 4 KiB boundaries, and
    /// falls back to byte reads inside a chunk that is only partly readable, so
    /// a terminator that sits just before an unmapped page is still found.
    ///
    /// # Errors
    ///
    /// [`MemError::LimitExceeded`] if `max` is above [`MAX_CSTR_LEN`] or no
    /// terminator occurs within `max` bytes. Read errors are returned as is when
    /// memory becomes unreadable before a terminator is found.
    fn read_cstr(&self, addr: Addr, max: usize) -> Result<Vec<u8>, MemError> {
        if max > MAX_CSTR_LEN {
            return Err(MemError::LimitExceeded {
                what: "C string max length",
                requested: max as u64,
                limit: MAX_CSTR_LEN as u64,
            });
        }
        let mut out = Vec::new();
        let mut cursor = addr;
        let mut budget = max + 1;
        let mut chunk = [0u8; CSTR_CHUNK];
        while budget > 0 {
            let to_boundary = CSTR_CHUNK - (cursor % CSTR_CHUNK as u64) as usize;
            let want = budget.min(to_boundary);
            let Some(buf) = chunk.get_mut(..want) else {
                break;
            };
            let (got, failure) = match self.read_exact(cursor, buf) {
                Ok(()) => (want, None),
                Err(err @ MemError::Unreadable { .. }) if want > 1 => {
                    (read_prefix(self, cursor, buf), Some(err))
                }
                Err(err) => return Err(err),
            };
            let readable = buf.get(..got).unwrap_or_default();
            if let Some(end) = readable.iter().position(|byte| *byte == 0) {
                out.extend_from_slice(readable.get(..end).unwrap_or_default());
                return Ok(out);
            }
            out.extend_from_slice(readable);
            if let Some(err) = failure {
                return Err(err);
            }
            budget -= want;
            cursor = cursor.wrapping_add(want as u64);
        }
        Err(MemError::LimitExceeded {
            what: "C string length",
            requested: max as u64 + 1,
            limit: max as u64,
        })
    }
}

impl<R: MemoryReader + ?Sized> MemoryReaderExt for R {}

/// Reads byte by byte until the first failure and returns how many bytes of
/// `buf` were filled. Used only on the error path of a chunk read.
fn read_prefix<R: MemoryReader + ?Sized>(reader: &R, addr: Addr, buf: &mut [u8]) -> usize {
    let mut filled = 0;
    for (offset, slot) in buf.iter_mut().enumerate() {
        let Some(at) = addr.checked_add(offset as u64) else {
            break;
        };
        let mut one = [0u8; 1];
        if reader.read_exact(at, &mut one).is_err() {
            break;
        }
        *slot = one[0];
        filled = offset + 1;
    }
    filled
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use crate::Snapshot;

    fn snapshot(base: Addr, bytes: &[u8]) -> Snapshot {
        Snapshot::builder().region(base, bytes).build().unwrap()
    }

    #[test]
    fn typed_reads_are_little_endian() {
        let snap = snapshot(
            0x1000,
            &[
                1, 0x34, 0x12, 0x78, 0x56, 0x34, 0x12, 0xfe, 0xff, 0xff, 0xff, 8, 7, 6, 5, 4, 3, 2,
                1,
            ],
        );
        assert_eq!(snap.read_u8(0x1000).unwrap(), 1);
        assert_eq!(snap.read_u16(0x1001).unwrap(), 0x1234);
        assert_eq!(snap.read_u32(0x1003).unwrap(), 0x1234_5678);
        assert_eq!(snap.read_i32(0x1007).unwrap(), -2);
        assert_eq!(snap.read_u64(0x100b).unwrap(), 0x0102_0304_0506_0708);
        assert_eq!(snap.read_pod::<[u8; 3]>(0x1000).unwrap(), [1, 0x34, 0x12]);
    }

    #[test]
    fn read_pod_fails_without_a_value_on_unmapped() {
        let snap = snapshot(0x1000, &[0; 4]);
        assert!(snap.read_u64(0x1000).is_err());
        assert!(snap.read_u32(0x0ffe).is_err());
    }

    #[test]
    fn read_vec_reads_elements() {
        let snap = snapshot(0x2000, &[1, 0, 0, 0, 2, 0, 0, 0, 3, 0, 0, 0]);
        assert_eq!(snap.read_vec::<u32>(0x2000, 3).unwrap(), [1, 2, 3]);
        assert!(snap.read_vec::<u32>(0x2000, 4).is_err());
        assert!(snap.read_vec::<u32>(0x2000, 0).unwrap().is_empty());
    }

    /// Reader that records every request, to prove limits fire before any read.
    struct CountingReader(AtomicUsize);

    impl MemoryReader for CountingReader {
        fn read_exact(&self, _addr: Addr, buf: &mut [u8]) -> Result<(), MemError> {
            self.0.fetch_add(1, Ordering::SeqCst);
            buf.fill(0xaa);
            Ok(())
        }
        fn regions(&self) -> Result<Vec<MemoryRegion>, MemError> {
            Ok(Vec::new())
        }
        fn modules(&self) -> Result<Vec<ModuleInfo>, MemError> {
            Ok(Vec::new())
        }
    }

    #[test]
    fn read_vec_rejects_huge_counts_before_reading_or_allocating() {
        let reader = CountingReader(AtomicUsize::new(0));
        for count in [usize::MAX, usize::MAX / 8 + 1, MAX_READ_VEC_COUNT + 1] {
            assert!(matches!(
                reader.read_vec::<u64>(0, count),
                Err(MemError::LimitExceeded { .. })
            ));
        }
        // Within the count limit but above the byte limit.
        assert!(matches!(
            reader.read_vec::<[u8; 64]>(0, MAX_READ_VEC_COUNT),
            Err(MemError::LimitExceeded { .. })
        ));
        assert_eq!(reader.0.load(Ordering::SeqCst), 0);
        let vec = reader.read_vec::<u8>(0, 8).unwrap();
        assert_eq!(vec, [0xaa; 8]);
    }

    #[test]
    fn read_cstr_finds_terminator() {
        let snap = snapshot(0x3000, b"hello\0world\0");
        assert_eq!(snap.read_cstr(0x3000, 64).unwrap(), b"hello");
        assert_eq!(snap.read_cstr(0x3006, 64).unwrap(), b"world");
        assert_eq!(snap.read_cstr(0x3005, 64).unwrap(), b"");
    }

    #[test]
    fn read_cstr_ends_just_before_an_unmapped_page() {
        // Region ends at the page boundary 0x5000; the next page is a hole.
        let text = b"tail_string_\0";
        let mut page = vec![b'a'; 0x1000];
        let start = page.len() - text.len();
        page[start..].copy_from_slice(text);
        let snap = snapshot(0x4000, &page);
        let got = snap.read_cstr(0x4000 + start as u64, 4096).unwrap();
        assert_eq!(got, b"tail_string_");

        // Terminator is the very last mapped byte.
        let mut page = vec![b'b'; 0x1000];
        page[0xfff] = 0;
        let snap = snapshot(0x4000, &page);
        assert_eq!(snap.read_cstr(0x4ffe, 4096).unwrap(), b"b");
    }

    #[test]
    fn read_cstr_without_terminator_before_the_hole_errors() {
        let snap = snapshot(0x4000, &[b'x'; 0x1000]);
        assert!(matches!(
            snap.read_cstr(0x4ff0, 4096),
            Err(MemError::Unreadable { .. })
        ));
    }

    #[test]
    fn read_cstr_is_bounded_by_max() {
        let snap = snapshot(0x4000, &[b'x'; 0x1000]);
        assert!(matches!(
            snap.read_cstr(0x4000, 10),
            Err(MemError::LimitExceeded { .. })
        ));
        assert!(matches!(
            snap.read_cstr(0x4000, MAX_CSTR_LEN + 1),
            Err(MemError::LimitExceeded { .. })
        ));
        // Exactly `max` bytes of content followed by the terminator is accepted.
        let snap = snapshot(0x4000, b"abc\0");
        assert_eq!(snap.read_cstr(0x4000, 3).unwrap(), b"abc");
        assert!(snap.read_cstr(0x4000, 2).is_err());
    }

    #[test]
    fn read_cstr_crossing_pages_inside_a_region() {
        let mut data = vec![b'z'; 0x2000];
        data[0x1800] = 0;
        let snap = snapshot(0x10000, &data);
        assert_eq!(
            snap.read_cstr(0x10ff0, 0x2000).unwrap().len(),
            0x1800 - 0xff0
        );
    }

    #[test]
    fn read_cstr_at_unmapped_start_or_overflow_errors() {
        let snap = snapshot(0x4000, b"a\0");
        assert!(snap.read_cstr(0x9000, 16).is_err());
        assert!(snap.read_cstr(u64::MAX, 16).is_err());
    }

    #[test]
    fn forwarding_impls_work() {
        let snap = Arc::new(snapshot(0x10, b"\x2a"));
        let boxed: Box<dyn MemoryReader> = Box::new(Arc::clone(&snap));
        assert_eq!(boxed.read_u8(0x10).unwrap(), 42);
        assert_eq!((*snap).read_u8(0x10).unwrap(), 42);
    }
}
