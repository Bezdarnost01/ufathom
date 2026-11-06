//! `ProcessMemory` against a child process that this test spawned.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "test code may panic on failure; clippy.toml only exempts #[test] functions, not helpers"
)]
mod common;

use std::sync::Arc;

use ufathom_mem::{MemError, MemoryReader, MemoryReaderExt, PageCache, ProcessMemory};

use common::{Host, MARKER_LEN, MARKER_TEXT, marker_bytes};

/// Hidden test: acts as the host when started by [`Host::spawn`], otherwise a no-op.
#[test]
fn hidden_child_host() {
    common::host_entry();
}

#[test]
fn reads_the_childs_buffer() {
    let host = Host::spawn();
    let mem = ProcessMemory::open(host.pid()).unwrap();
    assert_eq!(mem.pid(), host.pid());

    let mut buf = vec![0u8; MARKER_LEN];
    mem.read_exact(host.addr, &mut buf).unwrap();
    assert_eq!(buf, marker_bytes());
    assert_eq!(
        mem.read_cstr(host.addr, 256).unwrap(),
        &MARKER_TEXT[..MARKER_TEXT.len() - 1]
    );
    assert_eq!(mem.read_u8(host.addr).unwrap(), MARKER_TEXT[0]);
}

#[test]
fn unreadable_addresses_in_the_child_are_errors() {
    let host = Host::spawn();
    let mem = ProcessMemory::open(host.pid()).unwrap();
    let mut buf = [0u8; 32];
    for addr in [0u64, 0x10, 0xffff_8000_0000_0000, u64::MAX - 8] {
        assert!(mem.read_exact(addr, &mut buf).is_err(), "{addr:#x}");
    }
    assert!(matches!(
        mem.read_exact(u64::MAX - 8, &mut buf),
        Err(MemError::AddressOverflow { .. })
    ));
    // A read that starts in the buffer but runs off the end of its region.
    let mut big = vec![0u8; 64 << 20];
    assert!(mem.read_exact(host.addr, &mut big).is_err());
    // The child is still alive and readable afterwards.
    mem.read_exact(host.addr, &mut buf).unwrap();
}

#[test]
fn regions_and_modules_of_the_child_are_listed() {
    let host = Host::spawn();
    let mem = ProcessMemory::open(host.pid()).unwrap();

    let regions = mem.regions().unwrap();
    assert!(!regions.is_empty());
    let holding = regions
        .iter()
        .find(|region| region.contains(host.addr))
        .expect("the marker buffer lies in a listed region");
    assert!(holding.perms.read && holding.perms.write);

    let modules = mem.modules().unwrap();
    assert!(!modules.is_empty());
    let exe = std::env::current_exe().unwrap();
    let exe_name = exe.file_name().unwrap().to_string_lossy().to_lowercase();
    assert!(
        modules
            .iter()
            .any(|module| module.name.to_lowercase() == exe_name),
        "child's own executable {exe_name} not among {:?}",
        modules.iter().map(|m| &m.name).collect::<Vec<_>>()
    );
    assert!(modules.iter().all(|module| module.size > 0));
}

#[test]
fn works_behind_a_page_cache_shared_between_threads() {
    let host = Host::spawn();
    let cache = Arc::new(PageCache::new(ProcessMemory::open(host.pid()).unwrap()));
    let expected = marker_bytes();
    let addr = host.addr;
    let workers: Vec<_> = (0..4)
        .map(|_| {
            let cache = Arc::clone(&cache);
            let expected = expected.clone();
            std::thread::spawn(move || {
                let mut buf = vec![0u8; MARKER_LEN];
                cache.read_exact(addr, &mut buf).unwrap();
                assert_eq!(buf, expected);
            })
        })
        .collect();
    for worker in workers {
        worker.join().unwrap();
    }
    assert!(cache.stats().hits > 0 || cache.stats().misses > 0);
}

#[test]
fn nonexistent_pid_is_process_gone() {
    // No platform hands out this pid.
    for pid in [u32::MAX, u32::MAX - 3] {
        assert!(
            matches!(ProcessMemory::open(pid), Err(MemError::ProcessGone { .. })),
            "pid {pid}"
        );
    }
}

#[test]
fn reading_after_the_child_exits_reports_process_gone() {
    let mut host = Host::spawn();
    let pid = host.pid();
    let addr = host.addr;
    let mem = ProcessMemory::open(pid).unwrap();
    let mut buf = [0u8; 16];
    mem.read_exact(addr, &mut buf).unwrap();
    host.kill();
    assert!(matches!(
        mem.read_exact(addr, &mut buf),
        Err(MemError::ProcessGone { .. })
    ));
    assert!(matches!(mem.regions(), Err(MemError::ProcessGone { .. })));
    assert!(matches!(mem.modules(), Err(MemError::ProcessGone { .. })));
    assert!(matches!(
        ProcessMemory::open(pid),
        Err(MemError::ProcessGone { .. })
    ));
}
