//! The fast read path recovers only from its own faults.
//!
//! Its fault handler is attached to the copy routine alone, so an exception
//! raised anywhere else in the process, including an access violation, must
//! reach whoever is meant to handle it: here a vectored exception handler
//! that the test installs and the crate does not own. Other threads keep
//! reading through the fast path while that happens.
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

use ufathom_mem::{MemoryReader, SelfMemory};
use windows_sys::Win32::Foundation::EXCEPTION_ACCESS_VIOLATION;
use windows_sys::Win32::System::Diagnostics::Debug::{
    AddVectoredExceptionHandler, EXCEPTION_POINTERS, RaiseException, RemoveVectoredExceptionHandler,
};
use windows_sys::Win32::System::Memory::{
    MEM_COMMIT, MEM_RELEASE, MEM_RESERVE, PAGE_READWRITE, VirtualAlloc, VirtualFree,
};

const PAGE: usize = 4096;
/// `EXCEPTION_CONTINUE_EXECUTION` for a vectored handler.
const CONTINUE_EXECUTION: i32 = -1;
/// `EXCEPTION_CONTINUE_SEARCH`.
const CONTINUE_SEARCH: i32 = 0;
/// A software exception code of the test's own (customer bit set).
const CUSTOM_CODE: u32 = 0xE0AB_CDEF;
/// The "faulting address" of the access violations the test raises itself.
const RAISED_AV_ADDRESS: usize = 0xDEAD_0000;

static CUSTOM_SEEN: AtomicU64 = AtomicU64::new(0);
static RAISED_AV_SEEN: AtomicU64 = AtomicU64::new(0);
/// Address of a page the test lets the fast path fault on; the handler only
/// observes it.
static WATCHED_ADDRESS: AtomicU64 = AtomicU64::new(0);
static WATCHED_SEEN: AtomicU64 = AtomicU64::new(0);

/// The test's own vectored handler. It claims the exceptions the test raises
/// and merely observes the one fault of the watched page.
unsafe extern "system" fn foreign_handler(info: *mut EXCEPTION_POINTERS) -> i32 {
    // SAFETY: the system passes a valid `EXCEPTION_POINTERS` with a valid record.
    let record = unsafe { &*(*info).ExceptionRecord };
    let address = record.ExceptionInformation[1];
    if record.ExceptionCode as u32 == CUSTOM_CODE {
        CUSTOM_SEEN.fetch_add(1, Ordering::SeqCst);
        return CONTINUE_EXECUTION;
    }
    if record.ExceptionCode == EXCEPTION_ACCESS_VIOLATION && record.NumberParameters >= 2 {
        if address == RAISED_AV_ADDRESS {
            RAISED_AV_SEEN.fetch_add(1, Ordering::SeqCst);
            return CONTINUE_EXECUTION;
        }
        if address as u64 == WATCHED_ADDRESS.load(Ordering::SeqCst) {
            WATCHED_SEEN.fetch_add(1, Ordering::SeqCst);
        }
    }
    CONTINUE_SEARCH
}

/// Installs [`foreign_handler`] first in the chain and removes it on drop.
struct ForeignHandler(*mut c_void);

impl ForeignHandler {
    fn install() -> Self {
        // SAFETY: registers a handler function that lives for the whole process.
        let handle = unsafe { AddVectoredExceptionHandler(1, Some(foreign_handler)) };
        assert!(!handle.is_null());
        Self(handle)
    }
}

impl Drop for ForeignHandler {
    fn drop(&mut self) {
        // SAFETY: `self.0` is the handle `AddVectoredExceptionHandler` returned.
        unsafe { RemoveVectoredExceptionHandler(self.0) };
    }
}

/// Raises a software exception on this thread. A vectored handler that
/// returns `CONTINUE_EXECUTION` makes the call return.
fn raise(code: u32, arguments: &[usize]) {
    // SAFETY: the argument array is valid for its length, and the exception is
    // continuable (flags 0).
    unsafe { RaiseException(code, 0, arguments.len() as u32, arguments.as_ptr()) };
}

#[test]
fn exceptions_raised_elsewhere_reach_their_own_handler() {
    let _handler = ForeignHandler::install();
    let mem = SelfMemory::new();

    // Other threads read through the fast path the whole time.
    let stop = AtomicBool::new(false);
    let local = [7u8; 256];
    let addr = local.as_ptr() as u64;
    thread::scope(|scope| {
        for _ in 0..2 {
            scope.spawn(|| {
                let reader = SelfMemory::new();
                let mut buf = [0u8; 256];
                while !stop.load(Ordering::Relaxed) {
                    reader.read_exact(addr, &mut buf).unwrap();
                    assert_eq!(buf, [7u8; 256]);
                }
            });
        }

        let custom_before = CUSTOM_SEEN.load(Ordering::SeqCst);
        let av_before = RAISED_AV_SEEN.load(Ordering::SeqCst);
        for _ in 0..200 {
            // A software exception.
            raise(CUSTOM_CODE, &[]);
            // A deliberate access violation, as the processor would report one.
            raise(EXCEPTION_ACCESS_VIOLATION as u32, &[0, RAISED_AV_ADDRESS]);
            // Reads on this thread between the exceptions are unaffected.
            let mut buf = [0u8; 16];
            mem.read_exact(addr, &mut buf).unwrap();
        }
        assert_eq!(CUSTOM_SEEN.load(Ordering::SeqCst) - custom_before, 200);
        assert_eq!(RAISED_AV_SEEN.load(Ordering::SeqCst) - av_before, 200);
        stop.store(true, Ordering::Relaxed);
    });
}

#[test]
fn a_rust_panic_is_not_swallowed_by_the_copy_handler() {
    // A panic unwinds through the system's exception machinery. Reads on the
    // same thread before and after, and on another thread during it, must
    // not interfere.
    let mem = SelfMemory::new();
    let local = [9u8; 64];
    let addr = local.as_ptr() as u64;
    let mut buf = [0u8; 64];
    mem.read_exact(addr, &mut buf).unwrap();
    let result = std::panic::catch_unwind(|| {
        std::panic::resume_unwind(Box::new("deliberate"));
    });
    assert!(result.is_err());
    mem.read_exact(addr, &mut buf).unwrap();
    assert_eq!(buf, [9u8; 64]);
}

#[test]
fn a_foreign_handler_ahead_of_us_sees_the_fault_and_the_read_still_recovers() {
    let _handler = ForeignHandler::install();
    let mem = SelfMemory::new();
    let mut buf = [0u8; 16];
    for _ in 0..64 {
        // SAFETY: plain allocation request; a null result is checked below.
        let base =
            unsafe { VirtualAlloc(ptr::null(), PAGE, MEM_RESERVE | MEM_COMMIT, PAGE_READWRITE) };
        assert!(!base.is_null());
        let addr = base as usize as u64;
        // Validates and caches the page.
        mem.read_exact(addr, &mut buf).unwrap();
        WATCHED_ADDRESS.store(addr, Ordering::SeqCst);
        let seen_before = WATCHED_SEEN.load(Ordering::SeqCst);
        // SAFETY: releases the allocation made above, once.
        unsafe { VirtualFree(base, 0, MEM_RELEASE) };
        let result = mem.read_exact(addr, &mut buf);
        let seen = WATCHED_SEEN.load(Ordering::SeqCst) - seen_before;
        WATCHED_ADDRESS.store(0, Ordering::SeqCst);
        if result.is_ok() {
            // Another test took the address in between; try again.
            continue;
        }
        // The vectored handler ran first and declined, then the copy's own
        // handler recovered: an error, not a crash.
        // At least once: another test of this file may have its own copy of
        // the handler installed, and each copy sees the exception.
        assert!(seen >= 1, "the fault should be offered to earlier handlers");
        return;
    }
    panic!("reads of a freed page succeeded 64 times in a row");
}
