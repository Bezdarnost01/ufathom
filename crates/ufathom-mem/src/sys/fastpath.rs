//! The fast in-process read: a validated-extent cache in front of the
//! fault-guarded copy.
//!
//! A read of the current process goes through three gates:
//!
//! 1. **Cheap rejects.** Null-page, kernel and non-canonical addresses never
//!    reach the copy.
//! 2. **Validation.** The range must lie in committed, readable, non-guard
//!    memory according to `VirtualQuery`. The verdict is cached per thread as
//!    an *extent*, a run of such memory, so the next read in it costs a binary
//!    search and no system call. This is what keeps the fast path from
//!    touching a guard page: the first touch of a guard page clears its guard
//!    flag, which `ReadProcessMemory` never does, and a stack that loses its
//!    guard page cannot grow.
//! 3. **The guarded copy** (`guarded::guarded_copy`), which converts a fault
//!    into an error. It is what makes a stale cache entry harmless: a page
//!    freed after validation faults inside the guarded routine, the fault is
//!    caught, every cache is invalidated, and the read is retried through the
//!    operating system's copy call, which reports the real error.
//!
//! A cache entry is therefore only a hint. Memory safety never depends on it;
//! it saves system calls and keeps guard pages untouched.
//!
//! The one case the cache cannot cover is a page that gains `PAGE_GUARD`
//! after it was validated. Touching it raises a guard page violation inside
//! the guarded copy. The read fails like any other and the page's guard flag
//! is set again immediately ([`restore_guard`]); the window in which another
//! thread could touch the page unguarded is a few microseconds, and a stack
//! guard page does not appear on committed memory.

use std::cell::RefCell;
use std::ffi::c_void;
use std::mem::size_of;
use std::sync::atomic::{AtomicU64, Ordering};

use windows_sys::Win32::System::Memory::{
    MEM_COMMIT, MEMORY_BASIC_INFORMATION, PAGE_GUARD, PAGE_NOACCESS, VirtualProtect, VirtualQuery,
};

use super::guarded::{self, CopyFault};
use crate::protect::perms_from_protect;

/// Extents remembered per thread. When the table is full it is cleared; the
/// cost of a clear is one validation per extent used again.
const MAX_EXTENTS: usize = 1024;

/// `VirtualQuery` calls spent validating one read. A read that spans more
/// distinct regions than this goes to the operating system's copy call.
const MAX_REGION_QUERIES: usize = 16;

/// Lowest address read directly. Everything below is the reserved null range.
const MIN_USER_ADDRESS: u64 = 0x1_0000;

/// One past the highest canonical user-mode address on x86-64.
const USER_ADDRESS_LIMIT: u64 = 0x8000_0000_0000;

/// Page size on x86-64, used to find the page of a guard fault.
const PAGE_BYTES: u64 = 4096;

/// Bumped on every recovered fault, so every thread drops its cached extents.
static GENERATION: AtomicU64 = AtomicU64::new(0);

/// What [`try_read`] did.
pub(crate) enum Outcome {
    /// The buffer was filled.
    Done,
    /// The range was not proven readable, or a fault was recovered: the caller
    /// must use the operating system's copy call, which also produces the error.
    UseOsCall,
}

/// A run of validated readable memory: `[start, end)`.
#[derive(Clone, Copy)]
struct Extent {
    start: u64,
    end: u64,
}

impl Extent {
    const EMPTY: Self = Self { start: 0, end: 0 };

    fn covers(&self, addr: u64, end: u64) -> bool {
        self.start <= addr && end <= self.end
    }
}

/// Sorted, non-overlapping extents of one thread.
struct Table {
    generation: u64,
    len: usize,
    /// The extent that satisfied the last lookup. Reads of one object, or of
    /// neighbouring objects, hit it without a search.
    last: Extent,
    extents: [Extent; MAX_EXTENTS],
}

impl Table {
    const fn new() -> Self {
        Self {
            generation: 0,
            len: 0,
            last: Extent::EMPTY,
            extents: [Extent::EMPTY; MAX_EXTENTS],
        }
    }

    fn clear(&mut self) {
        self.len = 0;
        self.last = Extent::EMPTY;
    }

    /// Drops everything if a fault happened since this table was filled.
    fn sync(&mut self) {
        let generation = GENERATION.load(Ordering::Relaxed);
        if self.generation != generation {
            self.clear();
            self.generation = generation;
        }
    }

    fn find(&mut self, addr: u64, end: u64) -> bool {
        if self.last.covers(addr, end) {
            return true;
        }
        let live = self.extents.get(..self.len).unwrap_or_default();
        let after = live.partition_point(|extent| extent.start <= addr);
        let Some(candidate) = after.checked_sub(1).and_then(|index| live.get(index)) else {
            return false;
        };
        if candidate.covers(addr, end) {
            self.last = *candidate;
            return true;
        }
        false
    }

    /// Adds `extent`, replacing any extent it overlaps.
    fn insert(&mut self, extent: Extent) {
        let live = self.extents.get(..self.len).unwrap_or_default();
        // Sorted and disjoint, so starts and ends both ascend.
        let first = live.partition_point(|old| old.end <= extent.start);
        let past = live.partition_point(|old| old.start < extent.end);
        let past = past.max(first);
        if self.len - (past - first) + 1 > MAX_EXTENTS {
            self.clear();
            if let Some(slot) = self.extents.first_mut() {
                *slot = extent;
                self.len = 1;
            }
        } else {
            // The shifted range ends at `len + 1 - (past - first)`, at most `MAX_EXTENTS`.
            self.extents.copy_within(past..self.len, first + 1);
            if let Some(slot) = self.extents.get_mut(first) {
                *slot = extent;
            }
            self.len = self.len - (past - first) + 1;
        }
        self.last = extent;
    }
}

thread_local! {
    // No destructor and no lazy initialisation: nothing to run when a thread
    // exits, so nothing can point into this module after it is unloaded.
    static TABLE: RefCell<Table> = const { RefCell::new(Table::new()) };
}

/// Fills `buf` from `addr` with the guarded copy when the range is proven
/// readable. `addr + buf.len()` must not overflow and `buf` must not be empty.
pub(crate) fn try_read(addr: u64, buf: &mut [u8]) -> Outcome {
    let Some(end) = addr.checked_add(buf.len() as u64) else {
        return Outcome::UseOsCall;
    };
    if addr < MIN_USER_ADDRESS || end > USER_ADDRESS_LIMIT || !guarded::unwind_registered() {
        return Outcome::UseOsCall;
    }
    let Ok(source) = usize::try_from(addr) else {
        return Outcome::UseOsCall;
    };
    if !is_validated(addr, end) {
        return Outcome::UseOsCall;
    }
    match guarded::guarded_copy(source, buf) {
        Ok(()) => Outcome::Done,
        Err(fault) => {
            recover(fault);
            Outcome::UseOsCall
        }
    }
}

/// Whether `[addr, end)` is inside a validated extent, validating and caching
/// one if it is not already.
fn is_validated(addr: u64, end: u64) -> bool {
    let cached = TABLE.with(|table| {
        table.try_borrow_mut().is_ok_and(|mut table| {
            table.sync();
            table.find(addr, end)
        })
    });
    if cached {
        return true;
    }
    let Some(extent) = validate(addr, end) else {
        return false;
    };
    TABLE.with(|table| {
        if let Ok(mut table) = table.try_borrow_mut() {
            table.insert(extent);
        }
    });
    true
}

/// Walks the regions that cover `[addr, end)` with `VirtualQuery`. Returns the
/// extent from the start of the first region to the end of the last one, only
/// if every byte is committed, readable and not guarded.
fn validate(addr: u64, end: u64) -> Option<Extent> {
    let mut cursor = addr;
    let mut start = None;
    for _ in 0..MAX_REGION_QUERIES {
        let mut info = MEMORY_BASIC_INFORMATION::default();
        // SAFETY: `info` is a valid out buffer of the size passed, and the
        // address is only a value for the kernel to look up.
        let written = unsafe {
            VirtualQuery(
                usize::try_from(cursor).ok()? as *const c_void,
                &mut info,
                size_of::<MEMORY_BASIC_INFORMATION>(),
            )
        };
        if written == 0 || info.State != MEM_COMMIT || !perms_from_protect(info.Protect).read {
            return None;
        }
        let base = info.BaseAddress as usize as u64;
        let region_end = base.checked_add(info.RegionSize as u64)?;
        if region_end <= cursor {
            return None;
        }
        let first = *start.get_or_insert(base);
        if region_end >= end {
            return Some(Extent {
                start: first,
                end: region_end,
            });
        }
        cursor = region_end;
    }
    None
}

/// Handles a fault the guarded copy recovered from.
fn recover(fault: CopyFault) {
    GENERATION.fetch_add(1, Ordering::Relaxed);
    if fault.is_guard_page() {
        restore_guard(fault.address);
    }
}

/// Sets `PAGE_GUARD` again on the page a guard fault just cleared.
///
/// Reading a guard page raises the violation and removes the flag. The
/// fast path avoids guard pages by validating first, so this only runs when a
/// page became a guard page after it was validated. A failed protection call
/// is ignored: the read has already failed, and there is nothing better to do.
fn restore_guard(address: u64) {
    let page = address & !(PAGE_BYTES - 1);
    let Ok(at) = usize::try_from(page) else {
        return;
    };
    let mut info = MEMORY_BASIC_INFORMATION::default();
    // SAFETY: `info` is a valid out buffer of the size passed.
    let written = unsafe {
        VirtualQuery(
            at as *const c_void,
            &mut info,
            size_of::<MEMORY_BASIC_INFORMATION>(),
        )
    };
    if written == 0 || info.State != MEM_COMMIT || info.Protect & (PAGE_GUARD | PAGE_NOACCESS) != 0
    {
        return;
    }
    let mut previous = 0u32;
    // SAFETY: `at` is the start of a committed page of this process, whose
    // current protection was just read; this only adds the guard flag to it.
    unsafe {
        VirtualProtect(
            at as *const c_void,
            PAGE_BYTES as usize,
            info.Protect | PAGE_GUARD,
            &mut previous,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn extent(start: u64, end: u64) -> Extent {
        Extent { start, end }
    }

    #[test]
    fn table_finds_covering_extent_only() {
        let mut table = Table::new();
        table.insert(extent(0x1000, 0x2000));
        table.insert(extent(0x5000, 0x9000));
        table.insert(extent(0x3000, 0x4000));
        assert!(table.find(0x1000, 0x2000));
        assert!(table.find(0x1800, 0x1810));
        assert!(table.find(0x3000, 0x4000));
        assert!(table.find(0x8ff0, 0x9000));
        assert!(!table.find(0x1ff0, 0x2010), "crosses the end");
        assert!(!table.find(0x2000, 0x2008), "gap");
        assert!(!table.find(0xfff, 0x1004), "starts before");
        assert!(!table.find(0x9000, 0x9001));
    }

    #[test]
    fn table_replaces_overlapping_extents_and_stays_sorted() {
        let mut table = Table::new();
        for n in 0..10u64 {
            table.insert(extent(n * 0x2000, n * 0x2000 + 0x1000));
        }
        // Swallows extents 2..=5 and the front of 6.
        table.insert(extent(0x4000, 0xd000));
        assert!(table.find(0x4000, 0xd000));
        assert!(table.find(0x0, 0x1000));
        assert!(table.find(0xe000, 0xf000));
        let live = &table.extents[..table.len];
        assert!(live.windows(2).all(|pair| pair[0].end <= pair[1].start));
        assert_eq!(table.len, 2 + 1 + 3);
    }

    #[test]
    fn full_table_clears_and_keeps_the_new_extent() {
        let mut table = Table::new();
        for n in 0..MAX_EXTENTS as u64 + 5 {
            table.insert(extent(n * 0x2000, n * 0x2000 + 0x1000));
        }
        let last = MAX_EXTENTS as u64 + 4;
        assert!(table.find(last * 0x2000, last * 0x2000 + 0x1000));
        assert!(table.len <= MAX_EXTENTS);
    }

    #[test]
    fn generation_bump_drops_extents() {
        let mut table = Table::new();
        table.sync();
        table.insert(extent(0x1000, 0x2000));
        assert!(table.find(0x1000, 0x1008));
        GENERATION.fetch_add(1, Ordering::Relaxed);
        table.sync();
        assert!(!table.find(0x1000, 0x1008));
    }
}
