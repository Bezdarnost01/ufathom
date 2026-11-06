//! Parses a minidump written by the operating system for a child this test spawned.

#![cfg(windows)]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "test code may panic on failure; clippy.toml only exempts #[test] functions, not helpers"
)]

mod common;

use std::fs::File;
use std::os::windows::io::AsRawHandle;

use ufathom_mem::{MemoryReader, MemoryReaderExt, MinidumpMemory, PageCache};
use windows_sys::Win32::System::Diagnostics::Debug::{
    MiniDumpWithFullMemory, MiniDumpWithFullMemoryInfo, MiniDumpWriteDump,
};

use common::{Host, MARKER_LEN, MARKER_TEXT, marker_bytes};

/// Hidden test: acts as the host when started by [`Host::spawn`], otherwise a no-op.
#[test]
fn hidden_child_host() {
    common::host_entry();
}

/// Writes a full-memory dump of `host` into `path`.
fn write_dump(host: &Host, path: &std::path::Path) {
    let file = File::create(path).unwrap();
    // SAFETY: the process handle is owned by the live `Child` and the file handle
    // by `file`; both outlive the call. The optional pointer arguments are null.
    let ok = unsafe {
        MiniDumpWriteDump(
            host.child().as_raw_handle(),
            host.pid(),
            file.as_raw_handle(),
            MiniDumpWithFullMemory | MiniDumpWithFullMemoryInfo,
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::null(),
        )
    };
    assert_ne!(
        ok,
        0,
        "MiniDumpWriteDump failed: {}",
        std::io::Error::last_os_error()
    );
}

#[test]
fn parses_an_os_written_minidump_of_a_child() {
    let host = Host::spawn();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("child.dmp");
    write_dump(&host, &path);

    let dump = MinidumpMemory::open(&path).unwrap();

    // The marker buffer is found at the address the child reported.
    let mut buf = vec![0u8; MARKER_LEN];
    dump.read_exact(host.addr, &mut buf).unwrap();
    assert_eq!(buf, marker_bytes());
    assert_eq!(
        dump.read_cstr(host.addr, 256).unwrap(),
        &MARKER_TEXT[..MARKER_TEXT.len() - 1]
    );

    // The child's own executable is in the module list, with a full path.
    let exe = std::env::current_exe().unwrap();
    let exe_name = exe.file_name().unwrap().to_string_lossy().to_lowercase();
    let modules = dump.modules().unwrap();
    let module = modules
        .iter()
        .find(|module| module.name.to_lowercase() == exe_name)
        .unwrap_or_else(|| {
            panic!(
                "{exe_name} not in {:?}",
                modules.iter().map(|m| &m.name).collect::<Vec<_>>()
            )
        });
    assert!(
        module
            .path
            .as_deref()
            .is_some_and(|path| path.contains('\\'))
    );
    assert!(module.size > 0);
    assert!(
        modules
            .iter()
            .any(|module| module.name.eq_ignore_ascii_case("kernel32.dll")),
        "kernel32.dll missing from the module list"
    );

    // The memory info list gives permissions for the buffer's region.
    let regions = dump.regions().unwrap();
    let holding = regions
        .iter()
        .find(|region| region.contains(host.addr))
        .expect("marker region listed");
    assert!(holding.perms.read && holding.perms.write);

    // Holes in the dump are errors, and the cache works on top of it.
    let mut small = [0u8; 8];
    assert!(dump.read_exact(0, &mut small).is_err());
    assert!(dump.read_exact(u64::MAX - 4, &mut small).is_err());
    let cached = PageCache::new(dump);
    let mut again = vec![0u8; MARKER_LEN];
    cached.read_exact(host.addr, &mut again).unwrap();
    assert_eq!(again, marker_bytes());
}
