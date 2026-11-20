//! The proxy export tables match the system DLLs, and calls through the
//! stubs reach the real functions.
//!
//! The stubs are called by their assembly symbol names, as the linker sees
//! them. The codegen tests build whole proxy DLLs.
#![cfg(all(
    windows,
    any(
        feature = "proxy-version",
        feature = "proxy-dsound",
        feature = "proxy-dinput8",
        feature = "proxy-winmm"
    )
))]
#![allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]

use std::collections::BTreeSet;
use std::path::PathBuf;

use ufathom_runtime::proxy::{self, Preset};

fn system_dir() -> PathBuf {
    let root = std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into());
    PathBuf::from(root).join("System32")
}

/// `(name, ordinal)` of every export of a PE file, read from its export
/// directory. `None` for an export with only an ordinal. Gaps in the ordinal
/// range are not exports.
fn exports_of(bytes: &[u8]) -> BTreeSet<(Option<String>, u32)> {
    let u16_at = |o: usize| u16::from_le_bytes(bytes[o..o + 2].try_into().unwrap());
    let u32_at = |o: usize| u32::from_le_bytes(bytes[o..o + 4].try_into().unwrap());
    let pe = u32_at(0x3c) as usize;
    let sections = usize::from(u16_at(pe + 6));
    let optional = pe + 24;
    let table = optional + usize::from(u16_at(pe + 20));
    let file_offset = |rva: u32| -> usize {
        (0..sections)
            .map(|i| table + i * 40)
            .find_map(|s| {
                let (size, va, raw) = (
                    u32_at(s + 8).max(u32_at(s + 16)),
                    u32_at(s + 12),
                    u32_at(s + 20),
                );
                (va..va + size)
                    .contains(&rva)
                    .then(|| (rva - va + raw) as usize)
            })
            .unwrap()
    };
    let directory = file_offset(u32_at(optional + 112));
    let base = u32_at(directory + 16);
    let functions = u32_at(directory + 20) as usize;
    let names = u32_at(directory + 24) as usize;
    let function_table = file_offset(u32_at(directory + 28));
    let name_table = file_offset(u32_at(directory + 32));
    let ordinal_table = file_offset(u32_at(directory + 36));
    let mut named = std::collections::BTreeMap::new();
    for i in 0..names {
        let start = file_offset(u32_at(name_table + 4 * i));
        let end = start + bytes[start..].iter().position(|&b| b == 0).unwrap();
        let name = String::from_utf8(bytes[start..end].to_vec()).unwrap();
        named.insert(usize::from(u16_at(ordinal_table + 2 * i)), name);
    }
    (0..functions)
        .filter(|&i| u32_at(function_table + 4 * i) != 0)
        .map(|i| (named.get(&i).cloned(), base + u32::try_from(i).unwrap()))
        .collect()
}

fn table_of(preset: &Preset) -> BTreeSet<(Option<String>, u32)> {
    preset
        .exports
        .iter()
        .map(|e| (e.name.map(str::to_owned), u32::from(e.ordinal)))
        .collect()
}

#[test]
fn every_table_matches_its_system_dll() {
    for preset in proxy::PRESETS {
        let file = preset.file_name();
        let bytes = std::fs::read(system_dir().join(&file)).unwrap();
        let system = exports_of(&bytes);
        let ours = table_of(preset);
        let only_in_system: Vec<_> = system.difference(&ours).collect();
        let only_in_table: Vec<_> = ours.difference(&system).collect();
        assert!(
            only_in_system.is_empty() && only_in_table.is_empty(),
            "the table of {file} differs from this machine's {file}.\n\
             exported by the system DLL but missing from the table: {only_in_system:?}\n\
             in the table but not exported by the system DLL: {only_in_table:?}"
        );
    }
}

#[test]
fn the_tables_are_well_formed() {
    let com_entry_points = [
        "DllCanUnloadNow",
        "DllGetClassObject",
        "DllRegisterServer",
        "DllUnregisterServer",
    ];
    assert_eq!(proxy::PRESETS.len(), 4);
    for preset in proxy::PRESETS {
        assert_eq!(preset.feature, format!("proxy-{}", preset.dll));
        let ordinals: Vec<u16> = preset.exports.iter().map(|e| e.ordinal).collect();
        assert!(
            ordinals.windows(2).all(|pair| pair[0] < pair[1]),
            "{}: ordinals must be unique and ascending: {ordinals:?}",
            preset.dll
        );
        assert_eq!(Some(&preset.max_ordinal()), ordinals.last());
        let names: BTreeSet<_> = preset.exports.iter().filter_map(|e| e.name).collect();
        assert_eq!(
            names.len(),
            preset.exports.iter().filter(|e| e.name.is_some()).count(),
            "{}: duplicate names",
            preset.dll
        );
        for export in preset.exports {
            assert_eq!(
                export.private,
                export.name.is_some_and(|n| com_entry_points.contains(&n)),
                "{}: {export:?}",
                preset.dll
            );
        }
    }
}

#[test]
fn the_def_file_maps_every_export_to_a_distinct_stub() {
    for preset in proxy::PRESETS {
        let text = preset.def_file();
        let lines: Vec<&str> = text
            .lines()
            .skip_while(|line| *line != "EXPORTS")
            .skip(1)
            .collect();
        assert_eq!(lines.len(), preset.exports.len(), "{}", preset.dll);
        for (line, export) in lines.iter().zip(preset.exports) {
            let stub = preset.stub_symbol(export.ordinal);
            assert!(line.contains(&stub), "{line}");
            assert!(line.contains(&format!("@{}", export.ordinal)), "{line}");
            assert_eq!(line.contains("NONAME"), export.name.is_none(), "{line}");
            assert_eq!(line.contains("PRIVATE"), export.private, "{line}");
        }
    }
}

#[cfg(feature = "proxy-version")]
fn wide_nul(path: &std::path::Path) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    path.as_os_str().encode_wide().chain(Some(0)).collect()
}

#[cfg(feature = "proxy-version")]
mod version {
    use core::ffi::c_void;

    use super::*;

    unsafe extern "system" {
        // Ordinals 8, 4, and 17 of version.dll.
        fn ufathom_proxy_version_8(file: *const u16, handle: *mut u32) -> u32;
        fn ufathom_proxy_version_4(
            flags: u32,
            file: *const u16,
            handle: u32,
            len: u32,
            data: *mut c_void,
        ) -> i32;
        fn ufathom_proxy_version_17(
            block: *const c_void,
            sub_block: *const u16,
            buffer: *mut *mut c_void,
            len: *mut u32,
        ) -> i32;
    }

    #[test]
    fn calls_through_the_stubs_read_a_version_resource() {
        let file = wide_nul(&system_dir().join("kernel32.dll"));
        let mut handle = 0u32;
        // The first call resolves the export, the second reads the cached slot.
        // SAFETY: a NUL-terminated path and a valid out pointer.
        let first = unsafe { ufathom_proxy_version_8(file.as_ptr(), &raw mut handle) };
        // SAFETY: as above.
        let size = unsafe { ufathom_proxy_version_8(file.as_ptr(), &raw mut handle) };
        assert!(size > 0, "no version resource");
        assert_eq!(first, size);

        // `GetFileVersionInfoExW` has five arguments, so one is on the stack
        // when the stub is reached through the resolver.
        let mut block = vec![0u8; size as usize];
        // SAFETY: `block` holds `size` bytes.
        let ok = unsafe {
            ufathom_proxy_version_4(0, file.as_ptr(), 0, size, block.as_mut_ptr().cast())
        };
        assert_ne!(ok, 0);
        let root: Vec<u16> = "\\".encode_utf16().chain(Some(0)).collect();
        let mut info: *mut c_void = core::ptr::null_mut();
        let mut len = 0u32;
        // SAFETY: `block` was filled above; the out pointers are valid.
        let ok = unsafe {
            ufathom_proxy_version_17(
                block.as_ptr().cast(),
                root.as_ptr(),
                &raw mut info,
                &raw mut len,
            )
        };
        assert_ne!(ok, 0);
        assert!(len >= 52, "VS_FIXEDFILEINFO is 52 bytes, got {len}");
        // SAFETY: `VerQueryValueW` pointed `info` into `block` at a VS_FIXEDFILEINFO.
        let signature = unsafe { info.cast::<u32>().read_unaligned() };
        assert_eq!(signature, 0xFEEF_04BD);
    }
}

#[cfg(feature = "proxy-dsound")]
mod dsound {
    use core::ffi::c_void;

    type Callback =
        unsafe extern "system" fn(*mut c_void, *const u16, *const u16, *mut c_void) -> i32;

    unsafe extern "system" {
        // Ordinals 3 and 4 of dsound.dll.
        fn ufathom_proxy_dsound_3(callback: Callback, context: *mut c_void) -> i32;
        fn ufathom_proxy_dsound_4() -> i32;
    }

    unsafe extern "system" fn count(
        _guid: *mut c_void,
        _description: *const u16,
        _module: *const u16,
        context: *mut c_void,
    ) -> i32 {
        // SAFETY: `context` is the `u32` the test passed.
        unsafe { *context.cast::<u32>() += 1 };
        1
    }

    #[test]
    fn calls_through_the_stubs_reach_directsound() {
        for call in 0..2 {
            let mut devices = 0u32;
            // SAFETY: the callback and a pointer that outlives the call.
            let result = unsafe { ufathom_proxy_dsound_3(count, (&raw mut devices).cast()) };
            // S_OK, or a DSERR_* code on a machine with no audio device.
            assert!(
                result == 0 || (result as u32) >> 16 == 0x8878,
                "call {call}: unexpected result {result:#x}"
            );
        }
        // SAFETY: no arguments.
        let result = unsafe { ufathom_proxy_dsound_4() };
        assert!(
            result == 0 || result == 1,
            "DllCanUnloadNow gave {result:#x}"
        );
    }
}
