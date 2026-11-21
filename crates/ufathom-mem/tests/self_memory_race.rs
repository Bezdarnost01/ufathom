//! Reads that race with the memory they read must never crash.
//!
//! One thread frees, re-allocates and re-protects a page in a loop while
//! other threads read it in a loop. Every read must return either the whole
//! range with correct bytes, or an error. A crash of this process is the
//! failure the fast read path exists to rule out, so passing means the
//! process survived as well as the assertions.
//!
//! This is the only test in its file: it uses a fixed address, which no other
//! test may allocate, and it counts faults, which no other test may cause.
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
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use ufathom_mem::{MemError, MemoryReader, SelfMemory, fast_read_faults};
use windows_sys::Win32::System::Memory::{
    MEM_COMMIT, MEM_RELEASE, MEM_RESERVE, MEMORY_BASIC_INFORMATION, PAGE_GUARD, PAGE_NOACCESS,
    PAGE_READONLY, PAGE_READWRITE, VirtualAlloc, VirtualFree, VirtualProtect, VirtualQuery,
};

const PAGE: usize = 4096;
/// Pages in the raced allocation.
const PAGES: usize = 2;
/// The byte the writer fills pages with. Fresh pages are zero.
const PATTERN: u8 = 0xA5;
/// How long each racing phase runs.
const PHASE: Duration = Duration::from_millis(600);
/// Threads reading in each phase.
const READERS: usize = 3;

/// Reserves and frees `PAGES` pages and returns the address the system gave,
/// which is free again and can be asked for by address.
fn pick_address() -> usize {
    // SAFETY: plain allocation request; a null result is checked below.
    let base = unsafe {
        VirtualAlloc(
            ptr::null(),
            PAGES * PAGE,
            MEM_RESERVE | MEM_COMMIT,
            PAGE_READWRITE,
        )
    };
    assert!(!base.is_null());
    // SAFETY: `base` came from `VirtualAlloc` just above and is released once.
    unsafe { VirtualFree(base, 0, MEM_RELEASE) };
    base as usize
}

/// Commits the pages at `address` and fills them. `false` if the address is
/// not available.
fn commit_and_fill(address: usize) -> bool {
    // SAFETY: asks for a specific address; failure is a null result.
    let base = unsafe {
        VirtualAlloc(
            address as *const c_void,
            PAGES * PAGE,
            MEM_RESERVE | MEM_COMMIT,
            PAGE_READWRITE,
        )
    };
    if base.is_null() {
        return false;
    }
    // SAFETY: the pages were just committed read-write at `base`.
    unsafe { ptr::write_bytes(base.cast::<u8>(), PATTERN, PAGES * PAGE) };
    true
}

fn release(address: usize) {
    // SAFETY: releases an allocation made at exactly this base, or does nothing
    // if there is none.
    unsafe { VirtualFree(address as *mut c_void, 0, MEM_RELEASE) };
}

fn protection_at(address: usize) -> u32 {
    let mut info = MEMORY_BASIC_INFORMATION::default();
    // SAFETY: `info` is a valid out buffer of the size passed.
    let written = unsafe {
        VirtualQuery(
            address as *const c_void,
            &mut info,
            std::mem::size_of::<MEMORY_BASIC_INFORMATION>(),
        )
    };
    assert_ne!(written, 0);
    info.Protect
}

#[derive(Default)]
struct Tally {
    ok: AtomicU64,
    err: AtomicU64,
}

/// Reads random ranges of the raced pages until `stop`. Every success must
/// hold only bytes the writer could have put there (zero from a fresh page,
/// or the pattern); every failure must be an unreadable-memory error.
fn read_loop(address: usize, stop: &AtomicBool, tally: &Tally, seed: u64) {
    let mem = SelfMemory::new();
    let mut state = seed;
    let mut buf = vec![0u8; PAGES * PAGE];
    while !stop.load(Ordering::Relaxed) {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        let len = 1 + (state as usize) % (PAGES * PAGE);
        let offset = ((state >> 20) as usize) % (PAGES * PAGE - len + 1);
        let slice = &mut buf[..len];
        match mem.read_exact((address + offset) as u64, slice) {
            Ok(()) => {
                assert!(
                    slice.iter().all(|byte| *byte == 0 || *byte == PATTERN),
                    "a successful read returned bytes nobody wrote"
                );
                tally.ok.fetch_add(1, Ordering::Relaxed);
            }
            Err(MemError::Unreadable { .. }) => {
                tally.err.fetch_add(1, Ordering::Relaxed);
            }
            Err(other) => panic!("unexpected error {other:?}"),
        }
    }
}

/// Runs `writer` against `READERS` reading threads for [`PHASE`].
fn race(address: usize, writer: impl FnOnce(&AtomicBool) + Send) -> (u64, u64) {
    let stop = AtomicBool::new(false);
    let tally = Tally::default();
    thread::scope(|scope| {
        for n in 0..READERS {
            let (stop, tally) = (&stop, &tally);
            scope.spawn(move || read_loop(address, stop, tally, 0x9e37_79b9 + n as u64 * 7919));
        }
        scope.spawn(|| writer(&stop));
        thread::sleep(PHASE);
        stop.store(true, Ordering::Relaxed);
    });
    (
        tally.ok.load(Ordering::Relaxed),
        tally.err.load(Ordering::Relaxed),
    )
}

#[test]
fn reads_racing_with_free_and_reprotect_never_crash() {
    let address = pick_address();
    let mem = SelfMemory::new();
    let mut buf = [0u8; 64];

    // A guard page that was never validated is refused without being touched,
    // so no fault is recovered and the guard flag survives.
    assert!(commit_and_fill(address));
    let mut old = 0;
    // SAFETY: the first page of the allocation committed above.
    let ok = unsafe {
        VirtualProtect(
            address as *const c_void,
            PAGE,
            PAGE_READWRITE | PAGE_GUARD,
            &mut old,
        )
    };
    assert_ne!(ok, 0);
    let before = fast_read_faults();
    for _ in 0..3 {
        assert!(mem.read_exact(address as u64, &mut buf).is_err());
    }
    assert_eq!(fast_read_faults(), before, "the page should not be touched");
    assert_ne!(protection_at(address) & PAGE_GUARD, 0);
    release(address);

    // Deterministic recovery: validate, free, read. Retried only because
    // another thread of this process could take the address in between.
    let mut recovered = false;
    for _ in 0..100 {
        if !commit_and_fill(address) {
            continue;
        }
        mem.read_exact(address as u64, &mut buf).unwrap();
        release(address);
        let before = fast_read_faults();
        let result = mem.read_exact(address as u64, &mut buf);
        if result.is_err() && fast_read_faults() > before {
            recovered = true;
            break;
        }
    }
    assert!(
        !cfg!(target_arch = "x86_64") || recovered,
        "no fault was recovered"
    );

    // Phase one: free and re-allocate while reading.
    let faults_before = fast_read_faults();
    let started = Instant::now();
    let (ok, err) = race(address, |stop| {
        while !stop.load(Ordering::Relaxed) {
            if commit_and_fill(address) {
                thread::yield_now();
                release(address);
            }
        }
    });
    eprintln!(
        "free race: {ok} reads succeeded, {err} failed, {} faults recovered, {:?}",
        fast_read_faults() - faults_before,
        started.elapsed()
    );
    assert!(ok > 0 && err > 0, "the race should produce both outcomes");
    release(address);

    // Phase two: change protection while reading.
    assert!(commit_and_fill(address));
    let faults_before = fast_read_faults();
    let (ok, err) = race(address, |stop| {
        let flags = [
            PAGE_READWRITE,
            PAGE_NOACCESS,
            PAGE_READONLY,
            PAGE_READWRITE | PAGE_GUARD,
        ];
        let mut round = 0usize;
        while !stop.load(Ordering::Relaxed) {
            let mut old = 0;
            // SAFETY: both pages of the allocation committed above.
            unsafe {
                VirtualProtect(
                    address as *const c_void,
                    PAGES * PAGE,
                    flags[round % flags.len()],
                    &mut old,
                );
            }
            round += 1;
        }
    });
    eprintln!(
        "protection race: {ok} reads succeeded, {err} failed, {} faults recovered",
        fast_read_faults() - faults_before
    );
    assert!(ok > 0 && err > 0, "the race should produce both outcomes");
    release(address);
}
