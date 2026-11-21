//! The fast in-process read path must give the same answers as the system
//! call, also when its cache is stale: a page that is freed, protected or
//! guarded after a read validated it must produce an error, never a crash and
//! never a consumed guard page.
//!
//! The first read of a page validates it and caches the verdict. Every test
//! below reads once, changes the page, and reads again, so the second read
//! goes through the direct copy with a stale verdict.
#![cfg(windows)]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "test code may panic on failure; clippy.toml only exempts #[test] functions, not helpers"
)]

use std::ffi::c_void;
use std::ptr;

use ufathom_mem::{MemError, MemoryReader, SelfMemory};
use windows_sys::Win32::System::Memory::{
    MEM_COMMIT, MEM_DECOMMIT, MEM_RELEASE, MEM_RESERVE, MEMORY_BASIC_INFORMATION, PAGE_GUARD,
    PAGE_NOACCESS, PAGE_READONLY, PAGE_READWRITE, VirtualAlloc, VirtualFree, VirtualProtect,
    VirtualQuery,
};

const PAGE: usize = 4096;

/// An allocation released on drop.
struct Pages {
    base: *mut c_void,
}

impl Pages {
    fn new(pages: usize) -> Self {
        // SAFETY: plain allocation request; a null result is checked below.
        let base = unsafe {
            VirtualAlloc(
                ptr::null(),
                pages * PAGE,
                MEM_RESERVE | MEM_COMMIT,
                PAGE_READWRITE,
            )
        };
        assert!(!base.is_null());
        Self { base }
    }

    fn addr(&self) -> u64 {
        self.base as usize as u64
    }

    fn fill(&self, byte: u8) {
        // SAFETY: the first page is committed read-write and owned by `self`.
        unsafe { ptr::write_bytes(self.base.cast::<u8>(), byte, PAGE) };
    }

    fn protect(&self, flags: u32) {
        let mut old = 0;
        // SAFETY: the first page lies inside this allocation.
        let ok = unsafe { VirtualProtect(self.base, PAGE, flags, &mut old) };
        assert_ne!(ok, 0);
    }

    fn protection(&self) -> u32 {
        let mut info = MEMORY_BASIC_INFORMATION::default();
        // SAFETY: `info` is a valid out buffer of the size passed.
        let written = unsafe {
            VirtualQuery(
                self.base,
                &mut info,
                std::mem::size_of::<MEMORY_BASIC_INFORMATION>(),
            )
        };
        assert_ne!(written, 0);
        info.Protect
    }
}

impl Drop for Pages {
    fn drop(&mut self) {
        // SAFETY: `base` came from `VirtualAlloc` and is released once.
        unsafe { VirtualFree(self.base, 0, MEM_RELEASE) };
    }
}

fn assert_unreadable(result: Result<(), MemError>, what: &str) {
    match result {
        Err(MemError::Unreadable { .. } | MemError::AddressOverflow { .. }) => {}
        other => panic!("{what}: expected an unreadable/overflow error, got {other:?}"),
    }
}

#[test]
fn repeated_reads_of_a_page_agree_with_the_safe_reader() {
    let pages = Pages::new(1);
    pages.fill(0x3c);
    let (fast, safe) = (SelfMemory::new(), SelfMemory::safe());
    for len in [
        1usize, 2, 3, 4, 7, 8, 15, 16, 17, 31, 32, 33, 100, 1000, PAGE,
    ] {
        for offset in [0u64, 1, 5, (PAGE - len) as u64] {
            let (mut a, mut b) = (vec![0u8; len], vec![0u8; len]);
            let addr = pages.addr() + offset;
            if offset as usize + len > PAGE {
                continue;
            }
            fast.read_exact(addr, &mut a).unwrap();
            safe.read_exact(addr, &mut b).unwrap();
            assert_eq!(a, b, "len {len} offset {offset}");
            assert!(a.iter().all(|byte| *byte == 0x3c));
        }
    }
}

#[test]
fn a_page_freed_after_validation_is_an_error() {
    // Other tests in this file allocate too, and Windows gives the lowest
    // free address to the next allocation, so a freed page may be live again.
    // Retry until the read after the free fails.
    let mem = SelfMemory::new();
    for _ in 0..64 {
        let pages = Pages::new(1);
        let addr = pages.addr();
        let mut buf = [0u8; 16];
        mem.read_exact(addr, &mut buf).unwrap();
        drop(pages);
        let result = mem.read_exact(addr, &mut buf);
        if result.is_ok() {
            continue;
        }
        assert_unreadable(result, "freed after validation");
        // Whether this read faulted or was refused by the validation depends on
        // whether another test's fault cleared the cache first; both give an
        // error. `self_memory_race` checks the recovery deterministically.
        return;
    }
    panic!("reads of a freed page succeeded 64 times in a row");
}

#[test]
fn a_page_made_no_access_after_validation_is_an_error() {
    let pages = Pages::new(1);
    let mem = SelfMemory::new();
    let mut buf = [0u8; 8];
    mem.read_exact(pages.addr(), &mut buf).unwrap();
    pages.protect(PAGE_NOACCESS);
    assert_unreadable(mem.read_exact(pages.addr(), &mut buf), "no access");
    pages.protect(PAGE_READONLY);
    mem.read_exact(pages.addr(), &mut buf).unwrap();
}

#[test]
fn a_page_made_guard_after_validation_is_an_error_and_keeps_its_guard() {
    // The direct copy touches the page, the system raises the guard page
    // violation and clears the flag; the read must report an error and the
    // reader must put the flag back, so the reader never changes the target.
    let pages = Pages::new(1);
    pages.fill(0x42);
    let mem = SelfMemory::new();
    let mut buf = [0u8; 8];
    mem.read_exact(pages.addr(), &mut buf).unwrap();
    pages.protect(PAGE_READWRITE | PAGE_GUARD);
    for _ in 0..3 {
        assert_unreadable(mem.read_exact(pages.addr(), &mut buf), "stale guard page");
        assert_ne!(
            pages.protection() & PAGE_GUARD,
            0,
            "the read consumed the guard flag"
        );
    }
}

#[test]
fn a_read_that_runs_past_a_validated_page_is_an_error() {
    let pages = Pages::new(2);
    let mem = SelfMemory::new();
    let mut inside = [0u8; 32];
    let tail = pages.addr() + PAGE as u64 - 32;
    mem.read_exact(tail, &mut inside).unwrap();
    // Decommit the second page only: the first stays readable.
    // SAFETY: decommits the second page of this allocation, which nothing else uses.
    let ok = unsafe { VirtualFree(pages.base.cast::<u8>().add(PAGE).cast(), PAGE, MEM_DECOMMIT) };
    assert_ne!(ok, 0);
    let mut across = [0u8; 64];
    assert_unreadable(mem.read_exact(tail, &mut across), "into a decommitted page");
    mem.read_exact(tail, &mut inside).unwrap();
}

#[test]
fn every_thread_has_its_own_cache_but_the_same_answers() {
    let pages = Pages::new(1);
    pages.fill(0x99);
    let addr = pages.addr();
    let handles: Vec<_> = (0..4)
        .map(|_| {
            std::thread::spawn(move || {
                let mem = SelfMemory::new();
                let mut buf = [0u8; 64];
                for _ in 0..1000 {
                    mem.read_exact(addr, &mut buf).unwrap();
                    assert!(buf.iter().all(|byte| *byte == 0x99));
                }
            })
        })
        .collect();
    for handle in handles {
        handle.join().unwrap();
    }
}
