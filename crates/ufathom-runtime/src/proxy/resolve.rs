//! Finding the real function behind a stub: loads the system DLL by full path
//! on first use, looks the export up, and caches the address in the stub's
//! slot.

use core::sync::atomic::{AtomicUsize, Ordering};
use std::sync::OnceLock;

use windows_sys::Win32::Foundation::{ERROR_MOD_NOT_FOUND, ERROR_PROC_NOT_FOUND, SetLastError};
use windows_sys::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
use windows_sys::Win32::System::SystemInformation::GetSystemDirectoryW;

use super::tables::{Export, Preset};

/// Characters a system folder path may have, with room for the file name.
const PATH_CAPACITY: usize = 1024;

/// Characters kept free for `\` + the DLL file name + NUL.
const FILE_NAME_ROOM: usize = 64;

/// The longest export name the resolver looks up. The longest name in any
/// table is far shorter.
const NAME_CAPACITY: usize = 128;

/// The slots and the real module of one preset.
pub(super) struct Forwarder {
    preset: &'static Preset,
    /// One slot per ordinal, indexed by ordinal. A slot holds the address the
    /// stub jumps to, or 0 until the first call.
    slots: &'static [AtomicUsize],
    /// The real system module, loaded on first use. 0 when it could not be.
    module: OnceLock<usize>,
}

impl Forwarder {
    /// A forwarder with every slot empty. `slots` must have one entry per
    /// ordinal up to `preset.max_ordinal()`.
    pub(super) const fn new(preset: &'static Preset, slots: &'static [AtomicUsize]) -> Self {
        Self {
            preset,
            slots,
            module: OnceLock::new(),
        }
    }

    /// The address to jump to for `ordinal`, stored in its slot: the real
    /// function, or one of the failure functions below.
    pub(super) fn resolve(&self, ordinal: usize) -> usize {
        self.resolve_with(ordinal, || load_system_module(&self.preset.file_name()))
    }

    /// As [`resolve`](Self::resolve), with the loader that finds the real
    /// module supplied by the caller. It runs at most once per forwarder.
    pub(super) fn resolve_with(&self, ordinal: usize, load: impl FnOnce() -> usize) -> usize {
        let Some(slot) = self.slots.get(ordinal) else {
            return address_of_missing_export();
        };
        let cached = slot.load(Ordering::Acquire);
        if cached != 0 {
            return cached;
        }
        let module = *self.module.get_or_init(load);
        let address = if module == 0 {
            address_of_missing_module()
        } else {
            self.preset
                .exports
                .iter()
                .find(|export| usize::from(export.ordinal) == ordinal)
                .and_then(|export| lookup(module, export))
                .unwrap_or(address_of_missing_export())
        };
        slot.store(address, Ordering::Release);
        address
    }
}

/// Called by the assembly resolver on a stub's first call. Returns where the
/// stub must jump to; never fails and never unwinds.
pub(super) extern "C" fn resolve_entry(forwarder: *const Forwarder, ordinal: usize) -> usize {
    // SAFETY: the generated assembly passes the address of the `static`
    // `Forwarder` of the preset the stub belongs to, which lives for the
    // whole process.
    match unsafe { forwarder.as_ref() } {
        Some(forwarder) => forwarder.resolve(ordinal),
        None => address_of_missing_module(),
    }
}

/// The failure target when the real DLL could not be loaded. Reports
/// `ERROR_MOD_NOT_FOUND` and returns 0.
///
/// A caller that reaches this does not get an arbitrary result: 0 is the
/// failure value of most functions of these DLLs (`FALSE`, a null handle,
/// no bytes). For a function whose failure value is not 0, such as one that
/// returns an `HRESULT` where 0 is `S_OK`, the caller reads a success it did
/// not get; that case is best-effort.
pub(super) extern "system" fn missing_module() -> usize {
    // SAFETY: no preconditions.
    unsafe { SetLastError(ERROR_MOD_NOT_FOUND) };
    0
}

/// The failure target when the real DLL has no such export (an older Windows
/// than the table was read from). Reports `ERROR_PROC_NOT_FOUND` and returns
/// 0, with the same caveat as [`missing_module`].
pub(super) extern "system" fn missing_export() -> usize {
    // SAFETY: no preconditions.
    unsafe { SetLastError(ERROR_PROC_NOT_FOUND) };
    0
}

/// The address of [`missing_module`].
pub(super) fn address_of_missing_module() -> usize {
    missing_module as *const () as usize
}

/// The address of [`missing_export`].
pub(super) fn address_of_missing_export() -> usize {
    missing_export as *const () as usize
}

/// The address of `export` in `module`. `None` when the module does not have
/// it. `GetProcAddress` follows forwarded exports.
fn lookup(module: usize, export: &Export) -> Option<usize> {
    let procedure = match export.name {
        Some(name) => {
            let bytes = name.as_bytes();
            let mut buffer = [0u8; NAME_CAPACITY];
            // The buffer keeps at least one trailing zero byte as the NUL.
            if bytes.len() >= NAME_CAPACITY {
                return None;
            }
            buffer.get_mut(..bytes.len())?.copy_from_slice(bytes);
            // SAFETY: `module` is a loaded module handle and `buffer` holds
            // the NUL-terminated name.
            unsafe { GetProcAddress(module as _, buffer.as_ptr()) }
        }
        None => {
            // An ordinal is passed as a pointer whose value is below 65536.
            let ordinal = core::ptr::without_provenance::<u8>(usize::from(export.ordinal));
            // SAFETY: `module` is a loaded module handle; a pointer with only
            // the low word set is the documented way to ask for an ordinal.
            unsafe { GetProcAddress(module as _, ordinal) }
        }
    };
    procedure.map(|address| address as usize)
}

/// The system folder, from `GetSystemDirectoryW`, without a trailing
/// separator. `None` when the call fails or the path is too long.
fn system_directory() -> Option<Vec<u16>> {
    let mut path = [0u16; PATH_CAPACITY];
    let room = u32::try_from(PATH_CAPACITY - FILE_NAME_ROOM).ok()?;
    // SAFETY: `path` has `room` writable characters.
    let len = unsafe { GetSystemDirectoryW(path.as_mut_ptr(), room) };
    let len = usize::try_from(len)
        .ok()
        .filter(|&n| n > 0 && n < PATH_CAPACITY - FILE_NAME_ROOM)?;
    Some(path.iter().take(len).copied().collect())
}

/// Loads `file_name` from the system folder by full path. A full path gives
/// the system module even when this DLL has the same name. 0 on failure.
fn load_system_module(file_name: &str) -> usize {
    match system_directory() {
        Some(directory) => load_module_from(&directory, file_name),
        None => {
            // SAFETY: no preconditions.
            unsafe { SetLastError(ERROR_MOD_NOT_FOUND) };
            0
        }
    }
}

/// Loads `directory\file_name`. 0 on failure, with the last error from the
/// loader.
pub(super) fn load_module_from(directory: &[u16], file_name: &str) -> usize {
    let mut wide = directory.to_vec();
    wide.push(u16::from(b'\\'));
    wide.extend(file_name.encode_utf16());
    wide.push(0);
    // SAFETY: `wide` is a NUL-terminated full path.
    unsafe { LoadLibraryW(wide.as_ptr()) as usize }
}

#[cfg(test)]
mod tests {
    use core::sync::atomic::AtomicUsize;

    use windows_sys::Win32::Foundation::GetLastError;

    use super::*;

    const EXPORTS: &[Export] = &[
        Export {
            name: Some("GetFileVersionInfoSizeW"),
            ordinal: 1,
            private: false,
        },
        Export {
            name: Some("NoSuchExportInThisModule"),
            ordinal: 2,
            private: false,
        },
    ];
    const PRESET: Preset = Preset {
        dll: "version",
        feature: "proxy-version",
        exports: EXPORTS,
    };

    fn last_error() -> u32 {
        // SAFETY: no preconditions.
        unsafe { GetLastError() }
    }

    #[test]
    fn a_missing_module_resolves_to_a_failure_that_sets_the_error() {
        static SLOTS: [AtomicUsize; 3] = [const { AtomicUsize::new(0) }; 3];
        static FORWARDER: Forwarder = Forwarder::new(&PRESET, &SLOTS);
        // A folder that does not exist, so no `version.dll` is found in it.
        let absent =
            std::env::temp_dir().join(format!("ufathom-no-such-dir-{}", std::process::id()));
        let directory: Vec<u16> = absent.to_string_lossy().encode_utf16().collect();

        let target = FORWARDER.resolve_with(1, || load_module_from(&directory, "version.dll"));
        assert_eq!(target, address_of_missing_module());
        // The stub jumps there with the caller's arguments; it must return 0
        // and set the error instead of crashing.
        // SAFETY: `missing_module` ignores its arguments and has no preconditions.
        let result = unsafe {
            core::mem::transmute::<usize, extern "system" fn(usize, usize) -> usize>(target)(7, 8)
        };
        assert_eq!(result, 0);
        assert_eq!(last_error(), ERROR_MOD_NOT_FOUND);
        // The failure is cached: the loader does not run again.
        assert_eq!(
            FORWARDER.resolve_with(1, || panic!("must not load twice")),
            address_of_missing_module()
        );
    }

    #[test]
    fn a_missing_export_resolves_to_a_failure_that_sets_the_error() {
        static SLOTS: [AtomicUsize; 3] = [const { AtomicUsize::new(0) }; 3];
        static FORWARDER: Forwarder = Forwarder::new(&PRESET, &SLOTS);
        let system = system_directory().unwrap();
        let real = FORWARDER.resolve_with(1, || load_module_from(&system, "version.dll"));
        assert_ne!(real, 0);
        assert_ne!(real, address_of_missing_module());
        assert_ne!(real, address_of_missing_export());
        assert_eq!(
            FORWARDER.resolve_with(2, || panic!("the module is already loaded")),
            address_of_missing_export()
        );
        // A slot outside the table never reads out of bounds.
        assert_eq!(
            FORWARDER.resolve_with(99, || 0),
            address_of_missing_export()
        );
        let result = missing_export();
        assert_eq!(result, 0);
        assert_eq!(last_error(), ERROR_PROC_NOT_FOUND);
    }

    #[test]
    fn a_resolved_address_is_cached_in_its_slot() {
        static SLOTS: [AtomicUsize; 3] = [const { AtomicUsize::new(0) }; 3];
        static FORWARDER: Forwarder = Forwarder::new(&PRESET, &SLOTS);
        let system = system_directory().unwrap();
        let first = FORWARDER.resolve_with(1, || load_module_from(&system, "version.dll"));
        assert_eq!(SLOTS[1].load(Ordering::Acquire), first);
        let second = FORWARDER.resolve_with(1, || panic!("cached"));
        assert_eq!(first, second);
    }
}
