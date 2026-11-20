//! The `version.dll` forwarders: the export list matches the system module,
//! and calls reach the real functions.
#![cfg(all(feature = "proxy-version", windows))]
#![allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]

use std::path::PathBuf;

use ufathom_runtime::proxy;

fn system_dir() -> PathBuf {
    let root = std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into());
    PathBuf::from(root).join("System32")
}

/// Export names of a PE file, read from its export directory.
fn exports_of(bytes: &[u8]) -> Vec<String> {
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
    let count = u32_at(directory + 24) as usize;
    let names = file_offset(u32_at(directory + 32));
    (0..count)
        .map(|i| {
            let start = file_offset(u32_at(names + 4 * i));
            let end = start + bytes[start..].iter().position(|&b| b == 0).unwrap();
            String::from_utf8(bytes[start..end].to_vec()).unwrap()
        })
        .collect()
}

#[test]
fn the_export_list_matches_the_system_module() {
    let bytes = std::fs::read(system_dir().join("version.dll")).unwrap();
    let mut real = exports_of(&bytes);
    real.sort();
    let mut ours: Vec<String> = proxy::EXPORTS.iter().map(|s| (*s).to_owned()).collect();
    ours.sort();
    assert_eq!(ours, real);
}

#[test]
fn forwarded_calls_read_a_version_resource() {
    let file: Vec<u16> = system_dir()
        .join("kernel32.dll")
        .as_os_str()
        .encode_wide_nul();
    let mut handle = 0u32;
    // SAFETY: a NUL-terminated path and a valid out pointer.
    let size = unsafe { proxy::GetFileVersionInfoSizeW(file.as_ptr(), &raw mut handle) };
    assert!(size > 0, "no version resource");
    let mut block = vec![0u8; size as usize];
    // SAFETY: `block` holds `size` bytes.
    let ok =
        unsafe { proxy::GetFileVersionInfoW(file.as_ptr(), 0, size, block.as_mut_ptr().cast()) };
    assert_ne!(ok, 0);
    let root: Vec<u16> = "\\".encode_utf16().chain(Some(0)).collect();
    let mut info: *mut core::ffi::c_void = core::ptr::null_mut();
    let mut len = 0u32;
    // SAFETY: `block` was filled above; out pointers are valid.
    let ok = unsafe {
        proxy::VerQueryValueW(
            block.as_ptr().cast(),
            root.as_ptr(),
            &raw mut info,
            &raw mut len,
        )
    };
    assert_ne!(ok, 0);
    assert!(len >= 52, "VS_FIXEDFILEINFO is 52 bytes, got {len}");
    // SAFETY: VerQueryValueW pointed `info` into `block` at a VS_FIXEDFILEINFO.
    let signature = unsafe { info.cast::<u32>().read_unaligned() };
    assert_eq!(signature, 0xFEEF_04BD);
}

trait EncodeWideNul {
    fn encode_wide_nul(&self) -> Vec<u16>;
}

impl EncodeWideNul for std::ffi::OsStr {
    fn encode_wide_nul(&self) -> Vec<u16> {
        use std::os::windows::ffi::OsStrExt;
        self.encode_wide().chain(Some(0)).collect()
    }
}
