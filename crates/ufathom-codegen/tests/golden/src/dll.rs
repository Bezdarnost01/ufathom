use std::ffi::c_void;
use std::sync::atomic::{AtomicUsize, Ordering};

use ufathom_runtime::mem::SelfMemory;
use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, HINSTANCE, HMODULE};
use windows_sys::Win32::System::Diagnostics::Debug::OutputDebugStringW;
use windows_sys::Win32::System::LibraryLoader::{
    DisableThreadLibraryCalls, GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS, GetModuleHandleExW,
};
use windows_sys::Win32::System::SystemServices::DLL_PROCESS_ATTACH;
use windows_sys::Win32::System::Threading::CreateThread;

use crate::app::run;

static WORKER_HANDLE: AtomicUsize = AtomicUsize::new(0);

/// Starts the application on a worker, outside the loader lock.
#[allow(non_snake_case)]
#[no_mangle]
pub extern "system" fn DllMain(
    module: HINSTANCE,
    reason: u32,
    _reserved: *mut c_void,
) -> i32 {
    if reason == DLL_PROCESS_ATTACH {
        // SAFETY: `module` is the handle supplied by the loader.
        unsafe { DisableThreadLibraryCalls(module) };
        // SAFETY: the entry point has the required signature and no
        // parameter is passed. The worker starts after DllMain returns.
        let handle = unsafe {
            CreateThread(
                std::ptr::null(),
                0,
                Some(worker_entry),
                std::ptr::null(),
                0,
                std::ptr::null_mut(),
            )
        };
        if handle.is_null() {
            return 0;
        }
        WORKER_HANDLE.store(handle as usize, Ordering::Release);
    }
    1
}

extern "system" fn worker_entry(_parameter: *mut c_void) -> u32 {
    let handle = WORKER_HANDLE.swap(0, Ordering::AcqRel);
    if handle != 0 {
        // SAFETY: this is the thread handle created by DllMain. Closing
        // the handle does not stop the running thread.
        unsafe { CloseHandle(handle as HANDLE) };
    }

    // Keep the module mapped while application code can run. This scaffold
    // is resident: it intentionally does not implement hot-unload.
    let mut own_module: HMODULE = std::ptr::null_mut();
    // SAFETY: FROM_ADDRESS interprets the worker's address as a module
    // address and writes a new reference to `own_module`.
    let pinned = unsafe {
        GetModuleHandleExW(
            GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS,
            worker_entry as *const () as *const u16,
            &mut own_module,
        )
    };
    if pinned == 0 {
        debug("ufathom SDK: could not pin the application module");
        return 0;
    }

    match std::panic::catch_unwind(|| {
        let memory = SelfMemory::new();
        run(&memory)
    }) {
        Ok(Ok(())) => {}
        Ok(Err(error)) => debug(&format!("ufathom SDK: {error}")),
        Err(_) => debug("ufathom SDK: application panicked"),
    }
    0
}

fn debug(message: &str) {
    let wide: Vec<u16> = message.encode_utf16().chain(Some(0)).collect();
    // SAFETY: `wide` is NUL-terminated and lives through the call.
    unsafe { OutputDebugStringW(wide.as_ptr()) };
}
