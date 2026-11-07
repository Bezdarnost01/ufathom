//! The operating system calls of the DLL, each behind a small safe function.
//!
//! This is the only module of the DLL with `unsafe` blocks. Every block states
//! why the call is sound. Nothing here reads or writes the target's memory:
//! that is the job of `SelfMemory`, in `ufathom-mem`.

use std::ffi::{OsStr, OsString, c_void};
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::path::PathBuf;

use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_ALREADY_EXISTS, FreeLibrary, GetLastError, HANDLE, HINSTANCE, HMODULE,
};
use windows_sys::Win32::System::Diagnostics::Debug::OutputDebugStringW;
use windows_sys::Win32::System::LibraryLoader::{
    DisableThreadLibraryCalls, FreeLibraryAndExitThread, GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS,
    GetModuleFileNameW, GetModuleHandleExW,
};
use windows_sys::Win32::System::Threading::{
    CreateMutexW, CreateThread, GetCurrentProcessId, GetCurrentThread, ReleaseMutex,
    SetThreadPriority, THREAD_PRIORITY_BELOW_NORMAL,
};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState;
use windows_sys::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId};

/// Longest module path read, in UTF-16 units: the extended path limit.
const MAX_PATH_UNITS: usize = 32_768;

/// A reference to this DLL's own module, taken with `GetModuleHandleExW`.
///
/// While it exists the module cannot be unmapped by someone else's
/// `FreeLibrary`. It is consumed by [`unload_and_exit`].
pub(crate) struct Module {
    handle: usize,
}

impl Module {
    fn raw(&self) -> HMODULE {
        self.handle as HMODULE
    }
}

/// Takes a reference to the module this code is in.
///
/// Returns `None` if the system does not know the module, for example when it
/// was mapped by hand. The DLL then never unloads itself.
pub(crate) fn pin_own_module() -> Option<Module> {
    let mut handle: HMODULE = std::ptr::null_mut();
    // SAFETY: with FROM_ADDRESS the "name" is an address inside the module, and
    // the address of this function is one. `handle` is a valid out pointer. The
    // flag set does not include UNCHANGED_REFCOUNT, so the call adds a
    // reference, which is what pinning means.
    let ok = unsafe {
        GetModuleHandleExW(
            GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS,
            pin_own_module as *const () as *const u16,
            &mut handle,
        )
    };
    (ok != 0 && !handle.is_null()).then_some(Module {
        handle: handle as usize,
    })
}

/// Releases the pin and the loader's reference, then ends the calling thread.
///
/// The first release only lowers the count. The second runs inside
/// `FreeLibraryAndExitThread`, so the code that unmaps this module and exits
/// the thread is not in this module. When the loader's reference is the only
/// other one, the module is unmapped here.
///
/// The caller must have joined or ended every other thread that runs this
/// module's code, and must hold nothing that needs a destructor from this
/// module afterwards.
pub(crate) fn unload_and_exit(module: Module) -> ! {
    let raw = module.raw();
    // SAFETY: `raw` is the handle `pin_own_module` obtained, and this call
    // releases the reference that call added. The loader's reference keeps the
    // module mapped, so this code still exists after the call.
    unsafe { FreeLibrary(raw) };
    // SAFETY: `raw` still refers to the module while the loader's reference
    // exists. This call releases that reference and ends the thread from
    // kernel32, so no return address inside the unmapped module is used.
    unsafe { FreeLibraryAndExitThread(raw, 0) }
}

/// The full path of a module: the one this code is in when `own` is `true`,
/// otherwise the executable of the process.
pub(crate) fn module_path(own: bool) -> Option<PathBuf> {
    let module: HMODULE = if own {
        let mut handle: HMODULE = std::ptr::null_mut();
        // SAFETY: as in `pin_own_module`, but UNCHANGED_REFCOUNT (0x2) makes it
        // a plain lookup that adds no reference.
        let ok = unsafe {
            GetModuleHandleExW(
                GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | 0x2,
                module_path as *const () as *const u16,
                &mut handle,
            )
        };
        if ok == 0 {
            return None;
        }
        handle
    } else {
        std::ptr::null_mut()
    };
    let mut capacity = 512;
    loop {
        let mut buffer = vec![0_u16; capacity];
        let size = u32::try_from(buffer.len()).unwrap_or(u32::MAX);
        // SAFETY: `buffer` is writable for `size` UTF-16 units. A null module
        // means the process executable.
        let length = unsafe { GetModuleFileNameW(module, buffer.as_mut_ptr(), size) } as usize;
        if length == 0 {
            return None;
        }
        if length < buffer.len() {
            buffer.truncate(length);
            return Some(PathBuf::from(OsString::from_wide(&buffer)));
        }
        capacity = capacity
            .checked_mul(2)
            .filter(|next| *next <= MAX_PATH_UNITS)?;
    }
}

/// The id of the process the DLL is loaded in.
pub(crate) fn process_id() -> u32 {
    // SAFETY: no preconditions.
    unsafe { GetCurrentProcessId() }
}

/// Why the per-process mutex could not be taken.
pub(crate) enum MutexError {
    /// Another copy of the DLL holds it.
    AlreadyHeld,
    /// The system refused, with this error code.
    Os(u32),
}

/// A named mutex that exists only to say "an instance is running".
pub(crate) struct NamedMutex {
    handle: usize,
}

impl NamedMutex {
    /// Creates the mutex `name` and takes ownership of it, or reports that it
    /// already exists.
    pub(crate) fn acquire(name: &str) -> Result<Self, MutexError> {
        let wide = wide_nul(OsStr::new(name));
        // SAFETY: `wide` is a null-terminated UTF-16 string that outlives the
        // call; a null attributes pointer means default security.
        let handle = unsafe { CreateMutexW(std::ptr::null(), 1, wide.as_ptr()) };
        // SAFETY: reads the calling thread's last error, right after the call
        // that may have set it.
        let error = unsafe { GetLastError() };
        if handle.is_null() {
            return Err(MutexError::Os(error));
        }
        if error == ERROR_ALREADY_EXISTS {
            // SAFETY: `handle` is a handle we just received and own.
            unsafe { CloseHandle(handle) };
            return Err(MutexError::AlreadyHeld);
        }
        Ok(Self {
            handle: handle as usize,
        })
    }
}

impl Drop for NamedMutex {
    fn drop(&mut self) {
        let handle = self.handle as HANDLE;
        // SAFETY: `handle` is the mutex handle this value owns, created with
        // initial ownership by the thread that drops it (the worker). It is
        // released and closed exactly once, here.
        unsafe {
            ReleaseMutex(handle);
            CloseHandle(handle);
        }
    }
}

/// Closes a handle this DLL created and no longer needs.
pub(crate) fn close_handle(handle: usize) {
    if handle != 0 {
        // SAFETY: the caller passes a handle it owns, once.
        unsafe { CloseHandle(handle as HANDLE) };
    }
}

/// Lowers the priority of the calling thread to below normal.
pub(crate) fn lower_current_thread_priority() {
    // SAFETY: the pseudo handle of the calling thread is always valid for the
    // calling thread. A failure is ignored: the thread merely keeps its
    // priority.
    unsafe { SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_BELOW_NORMAL) };
}

/// Whether the key with this virtual-key code is down right now.
pub(crate) fn key_is_down(virtual_key: u8) -> bool {
    // SAFETY: no preconditions; any key code is accepted. The high bit of the
    // result is set while the key is down.
    unsafe { GetAsyncKeyState(i32::from(virtual_key)) < 0 }
}

/// The id of the process that owns the foreground window, if there is one.
pub(crate) fn foreground_process_id() -> Option<u32> {
    // SAFETY: no preconditions; a null window means there is no foreground
    // window.
    let window = unsafe { GetForegroundWindow() };
    if window.is_null() {
        return None;
    }
    let mut process = 0_u32;
    // SAFETY: `window` came from `GetForegroundWindow` and `process` is a valid
    // out pointer. A window that vanished in between gives 0 and no process.
    let thread = unsafe { GetWindowThreadProcessId(window, &mut process) };
    (thread != 0 && process != 0).then_some(process)
}

/// Writes a line to the debugger's output. Used only where no log file exists.
pub(crate) fn debug_log(message: &str) {
    let wide = wide_nul(OsStr::new(&format!("ufathom: {message}\n")));
    // SAFETY: `wide` is a null-terminated UTF-16 string that outlives the call.
    unsafe { OutputDebugStringW(wide.as_ptr()) };
}

/// `text` as UTF-16 with a terminating null.
pub(crate) fn wide_nul(text: &OsStr) -> Vec<u16> {
    text.encode_wide().chain(std::iter::once(0)).collect()
}

/// Tells the loader not to call `DllMain` for thread attach and detach of
/// this module.
pub(crate) fn disable_thread_calls(module: HINSTANCE) {
    // SAFETY: `module` is the handle the loader passed to `DllMain`, which is
    // valid for the duration of that call.
    unsafe { DisableThreadLibraryCalls(module) };
}

/// Starts a raw operating system thread and returns its handle as a number, or
/// 0 if the thread could not be created. The caller closes the handle.
pub(crate) fn create_thread(entry: extern "system" fn(*mut c_void) -> u32) -> usize {
    // SAFETY: `entry` is a function of the signature the system expects for a
    // thread start, and it takes no parameter. Null attributes and a null id
    // pointer are allowed, and zero flags start the thread at once.
    let handle = unsafe {
        CreateThread(
            std::ptr::null(),
            0,
            Some(entry),
            std::ptr::null(),
            0,
            std::ptr::null_mut(),
        )
    };
    handle as usize
}
