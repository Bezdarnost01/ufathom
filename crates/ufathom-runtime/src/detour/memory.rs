//! Memory queries, allocation near an address and writes into code pages.

use core::ffi::c_void;

use windows_sys::Win32::Foundation::GetLastError;
use windows_sys::Win32::System::Diagnostics::Debug::FlushInstructionCache;
use windows_sys::Win32::System::Memory::{
    MEM_COMMIT, MEM_FREE, MEM_RELEASE, MEM_RESERVE, MEMORY_BASIC_INFORMATION, PAGE_EXECUTE,
    PAGE_EXECUTE_READ, PAGE_EXECUTE_READWRITE, PAGE_EXECUTE_WRITECOPY, PAGE_GUARD, PAGE_NOACCESS,
    PAGE_READWRITE, VirtualAlloc, VirtualFree, VirtualProtect, VirtualQuery,
};
use windows_sys::Win32::System::Threading::GetCurrentProcess;

use super::DetourError;

/// Size of a page on x86-64 Windows.
pub(super) const PAGE_SIZE: usize = 0x1000;

/// Size of one trampoline block. The code in it is a few hundred bytes at
/// most; a page is the smallest unit the system commits.
pub(super) const BLOCK_SIZE: usize = PAGE_SIZE;

/// Windows hands out address space in 64 KiB units.
const ALLOCATION_GRANULARITY: usize = 0x1_0000;

/// Farthest a block may lie from an address it must reach with a signed
/// 32-bit displacement: 2 GiB minus a 64 KiB margin for the block itself.
const MAX_REACH: usize = 0x7FFF_0000;

/// Lowest address a block is placed at. The system keeps the first 64 KiB
/// unmapped.
const LOWEST_ADDRESS: usize = 0x1_0000;

/// Regions looked at, at most, while searching for free memory: a bound on
/// a loop over an address space that other threads change.
const MAX_QUERIES: usize = 1 << 16;

/// Regions walked, at most, to measure a run of executable memory.
const MAX_SPAN_REGIONS: usize = 16;

/// What `VirtualQuery` says about the pages at an address.
pub(super) struct Region {
    /// First byte of the run of pages with the same attributes.
    pub base: usize,
    /// Length of that run in bytes.
    pub size: usize,
    /// `MEM_COMMIT`, `MEM_RESERVE` or `MEM_FREE`.
    pub state: u32,
    /// The page protection, meaningful when committed.
    pub protect: u32,
}

/// Describes the pages at `address`, or `None` when the system refuses.
pub(super) fn query(address: usize) -> Option<Region> {
    // SAFETY: `MEMORY_BASIC_INFORMATION` is plain data; all zeroes is valid.
    let mut info: MEMORY_BASIC_INFORMATION = unsafe { core::mem::zeroed() };
    // SAFETY: `VirtualQuery` only reads the page tables; the buffer is valid
    // for the length passed.
    let written = unsafe {
        VirtualQuery(
            address as *const c_void,
            &mut info,
            size_of::<MEMORY_BASIC_INFORMATION>(),
        )
    };
    if written == 0 {
        return None;
    }
    Some(Region {
        base: info.BaseAddress as usize,
        size: info.RegionSize,
        state: info.State,
        protect: info.Protect,
    })
}

/// Whether a page protection allows execution and no guard or access fault.
pub(super) fn is_executable(protect: u32) -> bool {
    const EXECUTE: u32 =
        PAGE_EXECUTE | PAGE_EXECUTE_READ | PAGE_EXECUTE_READWRITE | PAGE_EXECUTE_WRITECOPY;
    protect & EXECUTE != 0 && protect & (PAGE_GUARD | PAGE_NOACCESS) == 0
}

/// How many bytes from `address` on, up to `max`, lie in committed,
/// executable pages. 0 when `address` itself does not.
pub(super) fn executable_span(address: usize, max: usize) -> usize {
    let end = address.saturating_add(max);
    let mut cursor = address;
    for _ in 0..MAX_SPAN_REGIONS {
        if cursor >= end {
            break;
        }
        let Some(region) = query(cursor) else { break };
        if region.state != MEM_COMMIT || !is_executable(region.protect) {
            break;
        }
        let next = region.base.saturating_add(region.size);
        if next <= cursor {
            break;
        }
        cursor = next;
    }
    cursor.min(end).saturating_sub(address)
}

fn distance(a: usize, b: usize) -> usize {
    a.abs_diff(b)
}

/// Whether a block at `candidate` reaches every anchor with a signed 32-bit
/// displacement from both its first and its last byte.
fn reaches(candidate: usize, anchors: &[usize]) -> bool {
    let last = candidate.saturating_add(BLOCK_SIZE);
    anchors.iter().all(|&anchor| {
        distance(anchor, candidate) <= MAX_REACH && distance(anchor, last) <= MAX_REACH
    })
}

/// One step of the search for free memory.
enum Step {
    /// A 64 KiB-aligned place where a block fits.
    Candidate(usize),
    /// Nothing here; look further.
    Skip,
    /// This direction is exhausted.
    Done,
}

/// Looks at the region at or above `cursor` and moves `cursor` past it.
fn step_up(cursor: &mut usize, anchors: &[usize]) -> Step {
    let Some(region) = query(*cursor) else {
        return Step::Done;
    };
    let Some(next) = region.base.checked_add(region.size) else {
        return Step::Done;
    };
    // Going up, the lowest anchor is the one that runs out of reach first.
    let lowest = anchors.iter().copied().min().unwrap_or(0);
    if region.base > lowest.saturating_add(MAX_REACH) {
        return Step::Done;
    }
    *cursor = next;
    if region.state != MEM_FREE {
        return Step::Skip;
    }
    let Some(candidate) = region
        .base
        .max(LOWEST_ADDRESS)
        .checked_next_multiple_of(ALLOCATION_GRANULARITY)
    else {
        return Step::Done;
    };
    if candidate.saturating_add(BLOCK_SIZE) <= next && reaches(candidate, anchors) {
        Step::Candidate(candidate)
    } else {
        Step::Skip
    }
}

/// Looks at the region at or below `cursor` and moves `cursor` before it.
fn step_down(cursor: &mut usize, anchors: &[usize]) -> Step {
    let Some(region) = query(*cursor) else {
        return Step::Done;
    };
    let Some(end) = region.base.checked_add(region.size) else {
        return Step::Done;
    };
    // Going down, the highest anchor is the one that runs out of reach first.
    let highest = anchors.iter().copied().max().unwrap_or(0);
    if end < highest.saturating_sub(MAX_REACH) {
        return Step::Done;
    }
    match region.base.checked_sub(1) {
        Some(before) => *cursor = before,
        None => return Step::Done,
    }
    if region.state != MEM_FREE {
        return Step::Skip;
    }
    let Some(top) = end.checked_sub(BLOCK_SIZE) else {
        return Step::Skip;
    };
    let candidate = top & !(ALLOCATION_GRANULARITY - 1);
    if candidate >= region.base && candidate >= LOWEST_ADDRESS && reaches(candidate, anchors) {
        Step::Candidate(candidate)
    } else {
        Step::Skip
    }
}

/// A page the detour owns, close to its target: the trampoline and the relay.
///
/// Dropping it releases the memory. [`into_raw`](Self::into_raw) gives the
/// ownership up when the block must outlive the value.
pub(super) struct NearBlock {
    base: usize,
}

impl NearBlock {
    /// Commits a read-write block that reaches every address in `anchors`
    /// with a signed 32-bit displacement, nearest to the first anchor.
    ///
    /// # Errors
    ///
    /// [`DetourError::NoNearMemory`] when no free address space within reach
    /// could be allocated.
    pub(super) fn allocate_near(anchors: &[usize]) -> Result<Self, DetourError> {
        let Some(&home) = anchors.first() else {
            return Err(DetourError::NoNearMemory { address: 0 });
        };
        let (mut up, mut down) = (home, home);
        let (mut up_open, mut down_open) = (true, true);
        for _ in 0..MAX_QUERIES {
            if !up_open && !down_open {
                break;
            }
            for upward in [true, false] {
                let open = if upward { &mut up_open } else { &mut down_open };
                if !*open {
                    continue;
                }
                let step = if upward {
                    step_up(&mut up, anchors)
                } else {
                    step_down(&mut down, anchors)
                };
                match step {
                    Step::Candidate(candidate) => {
                        // SAFETY: reserving and committing fresh pages at a
                        // free address; nothing existing is touched.
                        let memory = unsafe {
                            VirtualAlloc(
                                candidate as *const c_void,
                                BLOCK_SIZE,
                                MEM_COMMIT | MEM_RESERVE,
                                PAGE_READWRITE,
                            )
                        };
                        if memory as usize == candidate {
                            return Ok(Self { base: candidate });
                        }
                    }
                    Step::Skip => {}
                    Step::Done => *open = false,
                }
            }
        }
        Err(DetourError::NoNearMemory { address: home })
    }

    /// Address of the first byte.
    pub(super) fn base(&self) -> usize {
        self.base
    }

    /// Copies `code` to the start of the block. The block must still be
    /// writable (before [`make_executable`](Self::make_executable)).
    pub(super) fn fill(&self, code: &[u8]) -> Result<(), DetourError> {
        if code.len() > BLOCK_SIZE {
            return Err(DetourError::Unrelocatable {
                address: self.base,
                reason: "the relocated code does not fit in a trampoline block".to_owned(),
            });
        }
        // SAFETY: the block is committed, read-write and `BLOCK_SIZE` long;
        // nothing else has its address yet.
        unsafe { core::ptr::copy_nonoverlapping(code.as_ptr(), self.base as *mut u8, code.len()) };
        Ok(())
    }

    /// Switches the block from read-write to read-execute and flushes the
    /// instruction cache. No page is ever writable and executable at once.
    pub(super) fn make_executable(&self) -> Result<(), DetourError> {
        let mut old = 0u32;
        // SAFETY: the block is this value's own committed allocation.
        let ok = unsafe {
            VirtualProtect(
                self.base as *const c_void,
                BLOCK_SIZE,
                PAGE_EXECUTE_READ,
                &mut old,
            )
        };
        if ok == 0 {
            return Err(os_error("VirtualProtect"));
        }
        flush_instruction_cache(self.base, BLOCK_SIZE);
        Ok(())
    }

    /// Keeps the memory allocated and returns its address.
    pub(super) fn into_raw(self) -> usize {
        let base = self.base;
        core::mem::forget(self);
        base
    }
}

impl Drop for NearBlock {
    fn drop(&mut self) {
        // SAFETY: the block is this value's own allocation and no code in it
        // ever ran (`into_raw` was not called).
        unsafe { release(self.base) };
    }
}

/// Releases a block allocated by [`NearBlock::allocate_near`].
///
/// # Safety
///
/// `base` came from [`NearBlock::into_raw`] and is released once. No thread
/// executes code in the block or will return into it.
pub(super) unsafe fn release(base: usize) {
    // SAFETY: the caller owns the allocation.
    unsafe { VirtualFree(base as *mut c_void, 0, MEM_RELEASE) };
}

/// Makes the processor see freshly written code.
pub(super) fn flush_instruction_cache(address: usize, len: usize) {
    // SAFETY: flushes a range of this process; the range need not be valid
    // for the call to be harmless.
    unsafe { FlushInstructionCache(GetCurrentProcess(), address as *const c_void, len) };
}

/// The last operating system error as a [`DetourError`].
pub(super) fn os_error(call: &'static str) -> DetourError {
    // SAFETY: no preconditions.
    let code = unsafe { GetLastError() };
    DetourError::Os { call, code }
}

/// Why [`write_code_if`] did not write.
#[derive(Clone, Copy, Debug)]
pub(super) enum WriteFailure {
    /// The bytes in place were not the ones expected.
    Changed,
    /// A protection change failed with this `GetLastError`.
    Os(u32),
    /// The request was malformed.
    Length,
}

/// Restores the protection of the first `count` pages from `first`.
fn restore_pages(first: usize, saved: &[u32], count: usize) {
    for (index, &old) in saved.iter().enumerate().take(count) {
        let mut ignored = 0u32;
        // SAFETY: restores the protection read from the same page earlier.
        unsafe {
            VirtualProtect(
                (first + index * PAGE_SIZE) as *const c_void,
                PAGE_SIZE,
                old,
                &mut ignored,
            );
        }
    }
}

/// Replaces `expected` with `new` at `address` if the code there is still
/// `expected`, then restores the page protection and flushes the
/// instruction cache.
///
/// Makes no allocation and takes no lock, so it is safe to call while other
/// threads of the process are suspended.
///
/// # Safety
///
/// `address..address + new.len()` is mapped code of this process. Every
/// other thread that could execute it is suspended, or `new` and `expected`
/// differ only in ways a running thread tolerates.
pub(super) unsafe fn write_code_if(
    address: usize,
    expected: &[u8],
    new: &[u8],
) -> Result<(), WriteFailure> {
    let len = new.len();
    if len == 0 || len != expected.len() || len > PAGE_SIZE {
        return Err(WriteFailure::Length);
    }
    let first = address & !(PAGE_SIZE - 1);
    let last = address.saturating_add(len - 1) & !(PAGE_SIZE - 1);
    let pages = (last - first) / PAGE_SIZE + 1;
    // A write of at most one page touches one or two pages.
    let mut saved = [0u32; 2];
    let mut changed = 0usize;
    for (index, old) in saved.iter_mut().enumerate().take(pages) {
        // SAFETY: the caller promises the range is mapped.
        let ok = unsafe {
            VirtualProtect(
                (first + index * PAGE_SIZE) as *const c_void,
                PAGE_SIZE,
                PAGE_EXECUTE_READWRITE,
                old,
            )
        };
        if ok == 0 {
            // SAFETY: no preconditions.
            let code = unsafe { GetLastError() };
            restore_pages(first, &saved, changed);
            return Err(WriteFailure::Os(code));
        }
        changed = index + 1;
    }
    // SAFETY: the range is mapped and now readable and writable; the other
    // threads are suspended (contract).
    let current = unsafe { core::slice::from_raw_parts(address as *const u8, len) };
    let result = if current == expected {
        // SAFETY: as above; the source is a distinct Rust slice.
        unsafe { core::ptr::copy_nonoverlapping(new.as_ptr(), address as *mut u8, len) };
        flush_instruction_cache(address, len);
        Ok(())
    } else {
        Err(WriteFailure::Changed)
    };
    restore_pages(first, &saved, changed);
    result
}
