//! Exports of the system `version.dll`, forwarded to the real one, so a DLL
//! built with the `proxy-version` feature can be named `version.dll` and put
//! beside the game executable. The game then loads it at start-up, with no
//! injector.
//!
//! Linker forwarders cannot do this: `link.exe` refuses a forwarder that
//! names the module by full path, and one that names it as `version` would
//! resolve to this DLL again. So each export is an ordinary function that
//! loads `<system folder>\version.dll` by full path on its first call and
//! calls the real function with the same arguments. Nothing runs in
//! `DllMain`, and nothing is loaded until the game calls one of them.
//!
//! The signatures are those of `winver.h`. `GetFileVersionInfoByHandle` is
//! undocumented: it forwards eight integer arguments unchanged, which on
//! x86-64 passes through whatever the caller put in registers and stack
//! slots.

use core::ffi::c_void;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicUsize, Ordering};

use windows_sys::Win32::Foundation::{ERROR_MOD_NOT_FOUND, SetLastError};
use windows_sys::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
use windows_sys::Win32::System::SystemInformation::GetSystemDirectoryW;

/// Characters a system folder path may have, with room for `\version.dll`.
const PATH_CAPACITY: usize = 1024;

/// The real `version.dll`, loaded on first use. 0 when it could not be.
fn real_module() -> usize {
    static MODULE: OnceLock<usize> = OnceLock::new();
    *MODULE.get_or_init(|| {
        let mut path = [0u16; PATH_CAPACITY];
        let room = u32::try_from(PATH_CAPACITY - 16).unwrap_or(0);
        // SAFETY: `path` has `room` writable characters.
        let len = unsafe { GetSystemDirectoryW(path.as_mut_ptr(), room) };
        let Some(len) = usize::try_from(len)
            .ok()
            .filter(|&n| n > 0 && n < PATH_CAPACITY - 16)
        else {
            return 0;
        };
        let mut wide: Vec<u16> = path.iter().take(len).copied().collect();
        wide.extend("\\version.dll".encode_utf16());
        wide.push(0);
        // SAFETY: `wide` is a NUL-terminated full path. Loading by full path
        // gives the system module even when this DLL is named version.dll.
        unsafe { LoadLibraryW(wide.as_ptr()) as usize }
    })
}

/// The address of `name` (NUL-terminated) in the real module, cached in
/// `slot`. `None` when the module or the export is missing.
fn resolve(slot: &AtomicUsize, name: &'static str) -> Option<usize> {
    let cached = slot.load(Ordering::Acquire);
    if cached != 0 {
        return Some(cached);
    }
    let module = real_module();
    if module == 0 {
        return None;
    }
    // SAFETY: `module` is a loaded module handle and `name` is NUL-terminated.
    let address = unsafe { GetProcAddress(module as _, name.as_ptr()) }? as usize;
    slot.store(address, Ordering::Release);
    Some(address)
}

/// Reports that the real module is missing, and returns the failure value
/// every forwarded function shares.
fn missing<T: Default>() -> T {
    // SAFETY: no preconditions.
    unsafe { SetLastError(ERROR_MOD_NOT_FOUND) };
    T::default()
}

macro_rules! forward {
    ($( fn $name:ident($($arg:ident: $ty:ty),* $(,)?) -> $ret:ty; )*) => {$(
        #[doc = concat!("Forwards `", stringify!($name), "` to the system `version.dll`.")]
        ///
        /// # Safety
        ///
        /// As for the system function: the caller passes the arguments it
        /// documents.
        #[unsafe(no_mangle)]
        pub unsafe extern "system" fn $name($($arg: $ty),*) -> $ret {
            type Real = unsafe extern "system" fn($($ty),*) -> $ret;
            static SLOT: AtomicUsize = AtomicUsize::new(0);
            match resolve(&SLOT, concat!(stringify!($name), "\0")) {
                // SAFETY: the export of this name in the system module has
                // this signature, and the arguments are the caller's.
                Some(address) => unsafe {
                    core::mem::transmute::<usize, Real>(address)($($arg),*)
                },
                None => missing(),
            }
        }
    )*};
}

forward! {
    fn GetFileVersionInfoA(file: *const u8, handle: u32, len: u32, data: *mut c_void) -> i32;
    fn GetFileVersionInfoW(file: *const u16, handle: u32, len: u32, data: *mut c_void) -> i32;
    fn GetFileVersionInfoExA(flags: u32, file: *const u8, handle: u32, len: u32, data: *mut c_void) -> i32;
    fn GetFileVersionInfoExW(flags: u32, file: *const u16, handle: u32, len: u32, data: *mut c_void) -> i32;
    fn GetFileVersionInfoSizeA(file: *const u8, handle: *mut u32) -> u32;
    fn GetFileVersionInfoSizeW(file: *const u16, handle: *mut u32) -> u32;
    fn GetFileVersionInfoSizeExA(flags: u32, file: *const u8, handle: *mut u32) -> u32;
    fn GetFileVersionInfoSizeExW(flags: u32, file: *const u16, handle: *mut u32) -> u32;
    fn GetFileVersionInfoByHandle(
        a: usize, b: usize, c: usize, d: usize, e: usize, f: usize, g: usize, h: usize,
    ) -> usize;
    fn VerFindFileA(
        flags: u32, file: *const u8, win_dir: *const u8, app_dir: *const u8,
        cur_dir: *mut u8, cur_dir_len: *mut u32, dest_dir: *mut u8, dest_dir_len: *mut u32,
    ) -> u32;
    fn VerFindFileW(
        flags: u32, file: *const u16, win_dir: *const u16, app_dir: *const u16,
        cur_dir: *mut u16, cur_dir_len: *mut u32, dest_dir: *mut u16, dest_dir_len: *mut u32,
    ) -> u32;
    fn VerInstallFileA(
        flags: u32, src_file: *const u8, dest_file: *const u8, src_dir: *const u8,
        dest_dir: *const u8, cur_dir: *const u8, tmp_file: *mut u8, tmp_file_len: *mut u32,
    ) -> u32;
    fn VerInstallFileW(
        flags: u32, src_file: *const u16, dest_file: *const u16, src_dir: *const u16,
        dest_dir: *const u16, cur_dir: *const u16, tmp_file: *mut u16, tmp_file_len: *mut u32,
    ) -> u32;
    fn VerLanguageNameA(lang: u32, name: *mut u8, len: u32) -> u32;
    fn VerLanguageNameW(lang: u32, name: *mut u16, len: u32) -> u32;
    fn VerQueryValueA(block: *const c_void, sub_block: *const u8, buffer: *mut *mut c_void, len: *mut u32) -> i32;
    fn VerQueryValueW(block: *const c_void, sub_block: *const u16, buffer: *mut *mut c_void, len: *mut u32) -> i32;
}

/// Every name the system `version.dll` exports, in export table order. A
/// test checks this against the real module and against what a built SDK
/// DLL exports.
pub const EXPORTS: [&str; 17] = [
    "GetFileVersionInfoA",
    "GetFileVersionInfoByHandle",
    "GetFileVersionInfoExA",
    "GetFileVersionInfoExW",
    "GetFileVersionInfoSizeA",
    "GetFileVersionInfoSizeExA",
    "GetFileVersionInfoSizeExW",
    "GetFileVersionInfoSizeW",
    "GetFileVersionInfoW",
    "VerFindFileA",
    "VerFindFileW",
    "VerInstallFileA",
    "VerInstallFileW",
    "VerLanguageNameA",
    "VerLanguageNameW",
    "VerQueryValueA",
    "VerQueryValueW",
];
