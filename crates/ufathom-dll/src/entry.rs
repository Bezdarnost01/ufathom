//! `DllMain` and the entry point of the worker thread.

use std::ffi::c_void;
use std::panic::catch_unwind;
use std::sync::atomic::{AtomicUsize, Ordering};

use windows_sys::Win32::Foundation::HINSTANCE;
use windows_sys::Win32::System::SystemServices::DLL_PROCESS_ATTACH;

use crate::os;
use crate::worker::{self, Exit};

/// The handle `CreateThread` returned for the worker, for the worker to close.
static WORKER_HANDLE: AtomicUsize = AtomicUsize::new(0);

/// The loader's entry point.
///
/// On process attach this does exactly two things: it tells the loader not to
/// notify the DLL about thread creation, and it starts the worker thread. It
/// takes no lock, allocates nothing of note, logs nothing and touches no file,
/// because the loader lock is held.
///
/// The system starts a new thread only after `DllMain` returns, since thread
/// start-up needs the loader lock too, so the worker never runs concurrently
/// with this function.
#[allow(
    non_snake_case,
    reason = "the loader looks up the entry point by this exact name"
)]
#[unsafe(no_mangle)]
pub extern "system" fn DllMain(module: HINSTANCE, reason: u32, _reserved: *mut c_void) -> i32 {
    if reason == DLL_PROCESS_ATTACH {
        os::disable_thread_calls(module);
        WORKER_HANDLE.store(os::create_thread(worker_entry), Ordering::Release);
    }
    1
}

/// The worker thread. Runs under `catch_unwind`: a panic must never cross
/// into the system's thread start code.
extern "system" fn worker_entry(_parameter: *mut c_void) -> u32 {
    os::close_handle(WORKER_HANDLE.swap(0, Ordering::AcqRel));
    // First thing: take a reference to our own module, so nobody else's
    // `FreeLibrary` can unmap the code this thread runs.
    let pin = os::pin_own_module();
    let exit = catch_unwind(worker::run).unwrap_or(Exit::Stay);
    match (exit, pin) {
        (Exit::Unload, Some(module)) => os::unload_and_exit(module),
        // Staying loaded: the pin is deliberately never released, so the code
        // of any thread that is still running cannot be unmapped.
        _ => 0,
    }
}
