//! `SelfMemory` must never fault, whatever it is asked to read.
//!
//! A fault here would crash the test process, so passing means the process
//! survived as well as the assertions.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "test code may panic on failure; clippy.toml only exempts #[test] functions, not helpers"
)]
use ufathom_mem::{MemError, MemoryReader, MemoryReaderExt, SelfMemory};

const PAGE: usize = 4096;

/// Deterministic xorshift64* generator, so failures reproduce.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }
}

fn assert_unreadable(result: Result<(), MemError>, what: &str) {
    match result {
        Err(MemError::Unreadable { .. } | MemError::AddressOverflow { .. }) => {}
        other => panic!("{what}: expected an unreadable/overflow error, got {other:?}"),
    }
}

#[test]
fn reads_own_memory() {
    let mem = SelfMemory::new();
    let data: Vec<u8> = (0..=255u8).cycle().take(10_000).collect();
    let mut copy = vec![0u8; data.len()];
    mem.read_exact(data.as_ptr() as u64, &mut copy).unwrap();
    assert_eq!(copy, data);
    let value = 0x1122_3344_5566_7788u64;
    assert_eq!(mem.read_u64(&value as *const u64 as u64).unwrap(), value);
    let text = b"self memory c string\0";
    assert_eq!(
        mem.read_cstr(text.as_ptr() as u64, 64).unwrap(),
        b"self memory c string"
    );
    assert!(mem.read_exact(data.as_ptr() as u64, &mut []).is_ok());
}

#[test]
fn lists_regions_and_modules_containing_this_code() {
    let mem = SelfMemory::new();
    let here = lists_regions_and_modules_containing_this_code as fn() as usize as u64;
    let regions = mem.regions().unwrap();
    assert!(!regions.is_empty());
    assert!(regions.windows(2).all(|pair| pair[0].base <= pair[1].base));
    let region = regions
        .iter()
        .find(|region| region.contains(here))
        .expect("the running test function lies in a listed region");
    assert!(region.perms.read && region.perms.execute);
    let modules = mem.modules().unwrap();
    assert!(!modules.is_empty());
    assert!(
        modules
            .iter()
            .any(|module| here >= module.base && here - module.base < module.size),
        "no module contains the test function"
    );
}

#[test]
fn null_kernel_and_extreme_addresses_are_errors() {
    let mem = SelfMemory::new();
    let mut buf = [0u8; 64];
    for addr in [
        0u64,
        1,
        0x10,
        0xffff,
        0x7fff_ffff_0000,
        0x7fff_ffff_ffff,
        0x8000_0000_0000,
        0xffff_0800_0000_0000,
        0xffff_8000_0000_0000,
        0xffff_f780_0000_0000,
        0xffff_ffff_ffff_0000,
        u64::MAX - 63,
        u64::MAX - 1,
        u64::MAX,
    ] {
        assert!(mem.read_exact(addr, &mut buf).is_err(), "{addr:#x}");
        assert!(mem.read_exact(addr, &mut buf[..1]).is_err(), "{addr:#x}");
    }
    assert!(mem.read_u8(0).is_err());
    assert!(mem.read_cstr(0, 16).is_err());
}

#[test]
fn overflowing_ranges_are_reported_as_overflow() {
    let mem = SelfMemory::new();
    let mut buf = [0u8; 16];
    for addr in [u64::MAX, u64::MAX - 8, u64::MAX - 15] {
        assert!(
            matches!(
                mem.read_exact(addr, &mut buf),
                Err(MemError::AddressOverflow { .. })
            ),
            "{addr:#x}"
        );
    }
}

#[test]
fn huge_lengths_do_not_fault() {
    let mem = SelfMemory::new();
    let local = [1u8; 16];
    let mut big = vec![0u8; 64 << 20];
    // Starts in readable memory but runs far past it.
    assert!(mem.read_exact(local.as_ptr() as u64, &mut big).is_err());
}

#[test]
fn pseudo_random_addresses_never_fault() {
    let mem = SelfMemory::new();
    let anchor = vec![0u8; 3 * PAGE];
    let anchor_addr = anchor.as_ptr() as u64;
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    let mut buf = vec![0u8; 3 * PAGE];
    let (mut failed, mut succeeded) = (0u32, 0u32);
    for round in 0..200_000u32 {
        let raw = rng.next();
        let addr = match round % 5 {
            // Anywhere in the 64-bit space.
            0 => raw,
            // Anywhere in the canonical user range.
            1 => raw & 0x7fff_ffff_ffff,
            // Close to the top of the address space.
            2 => u64::MAX - (raw & 0xffff),
            // Close to a live allocation, partly inside and partly outside.
            3 => anchor_addr
                .wrapping_add(raw % (6 * PAGE as u64))
                .wrapping_sub(PAGE as u64),
            // Low addresses.
            _ => raw & 0xff_ffff,
        };
        let len = 1 + (rng.next() % buf.len() as u64) as usize;
        match mem.read_exact(addr, &mut buf[..len]) {
            Ok(()) => succeeded += 1,
            Err(_) => failed += 1,
        }
    }
    assert!(failed > 100_000, "random addresses should mostly fail");
    assert!(
        succeeded > 0,
        "reads near the live allocation should sometimes succeed"
    );
    drop(anchor);
}

#[cfg(windows)]
mod windows_pages {
    use std::ffi::c_void;
    use std::ptr;

    use windows_sys::Win32::System::Memory::{
        MEM_COMMIT, MEM_RELEASE, MEM_RESERVE, MEMORY_BASIC_INFORMATION, PAGE_GUARD, PAGE_NOACCESS,
        PAGE_READWRITE, VirtualAlloc, VirtualFree, VirtualProtect, VirtualQuery,
    };

    use super::*;

    /// An allocation released on drop.
    struct Alloc {
        base: *mut c_void,
    }

    impl Alloc {
        /// Reserves `reserve` bytes and commits the first `commit` bytes as read/write.
        fn new(reserve: usize, commit: usize) -> Self {
            // SAFETY: plain allocation request; a null result is checked below.
            let base = unsafe { VirtualAlloc(ptr::null(), reserve, MEM_RESERVE, PAGE_READWRITE) };
            assert!(!base.is_null(), "VirtualAlloc reserve failed");
            if commit > 0 {
                // SAFETY: commits pages inside the region reserved just above.
                let ok = unsafe { VirtualAlloc(base, commit, MEM_COMMIT, PAGE_READWRITE) };
                assert!(!ok.is_null(), "VirtualAlloc commit failed");
            }
            Self { base }
        }

        fn addr(&self) -> u64 {
            self.base as usize as u64
        }

        fn protect(&self, offset: usize, len: usize, flags: u32) {
            let mut old = 0u32;
            // SAFETY: the range lies inside memory this allocation committed.
            let ok = unsafe {
                VirtualProtect(
                    self.base.cast::<u8>().add(offset).cast(),
                    len,
                    flags,
                    &mut old,
                )
            };
            assert_ne!(ok, 0, "VirtualProtect failed");
        }

        fn fill(&self, offset: usize, len: usize, byte: u8) {
            // SAFETY: the range was committed read/write by `new` and is not protected yet.
            unsafe { ptr::write_bytes(self.base.cast::<u8>().add(offset), byte, len) };
        }

        fn protection_at(&self, offset: usize) -> u32 {
            let mut info = MEMORY_BASIC_INFORMATION::default();
            // SAFETY: `info` is a valid out buffer of the size passed.
            let written = unsafe {
                VirtualQuery(
                    self.base.cast::<u8>().add(offset).cast(),
                    &mut info,
                    std::mem::size_of::<MEMORY_BASIC_INFORMATION>(),
                )
            };
            assert_ne!(written, 0);
            info.Protect
        }
    }

    impl Drop for Alloc {
        fn drop(&mut self) {
            // SAFETY: `base` came from `VirtualAlloc` and is released once.
            unsafe { VirtualFree(self.base, 0, MEM_RELEASE) };
        }
    }

    #[test]
    fn committed_memory_is_readable() {
        let alloc = Alloc::new(2 * PAGE, 2 * PAGE);
        alloc.fill(0, 2 * PAGE, 0x5a);
        let mut buf = vec![0u8; 2 * PAGE];
        SelfMemory::new()
            .read_exact(alloc.addr(), &mut buf)
            .unwrap();
        assert!(buf.iter().all(|byte| *byte == 0x5a));
    }

    #[test]
    fn no_access_page_is_an_error() {
        let alloc = Alloc::new(PAGE, PAGE);
        alloc.protect(0, PAGE, PAGE_NOACCESS);
        let mem = SelfMemory::new();
        let mut buf = [0u8; 16];
        assert_unreadable(mem.read_exact(alloc.addr(), &mut buf), "PAGE_NOACCESS");
        assert_unreadable(
            mem.read_exact(alloc.addr() + 100, &mut buf[..1]),
            "PAGE_NOACCESS",
        );
        assert_unreadable(
            mem.read_exact(alloc.addr() + PAGE as u64 - 1, &mut buf[..1]),
            "last byte",
        );
    }

    #[test]
    fn guard_page_is_an_error_and_raises_no_exception() {
        let alloc = Alloc::new(2 * PAGE, 2 * PAGE);
        alloc.fill(0, 2 * PAGE, 0x11);
        alloc.protect(PAGE, PAGE, PAGE_READWRITE | PAGE_GUARD);
        let mem = SelfMemory::new();
        let mut buf = [0u8; 16];
        assert_unreadable(
            mem.read_exact(alloc.addr() + PAGE as u64, &mut buf),
            "PAGE_GUARD",
        );
        // A read that starts on a normal page and ends on the guard page fails whole.
        let mut span = [0u8; 32];
        assert_unreadable(
            mem.read_exact(alloc.addr() + PAGE as u64 - 16, &mut span),
            "span into PAGE_GUARD",
        );
        // The page before the guard stays readable.
        mem.read_exact(alloc.addr(), &mut buf).unwrap();
        assert_eq!(buf, [0x11; 16]);
    }

    #[test]
    fn guard_page_is_not_consumed_by_a_failed_read() {
        // A guard page is how stacks grow. A reader that triggered and
        // cleared guard pages would corrupt the target.
        let alloc = Alloc::new(PAGE, PAGE);
        alloc.protect(0, PAGE, PAGE_READWRITE | PAGE_GUARD);
        let mut buf = [0u8; 8];
        for _ in 0..3 {
            assert!(
                SelfMemory::new()
                    .read_exact(alloc.addr(), &mut buf)
                    .is_err()
            );
        }
        assert_ne!(
            alloc.protection_at(0) & PAGE_GUARD,
            0,
            "the read cleared the guard flag"
        );
    }

    #[test]
    fn freed_page_is_an_error() {
        let alloc = Alloc::new(PAGE, PAGE);
        let addr = alloc.addr();
        let mut buf = [0u8; 8];
        SelfMemory::new().read_exact(addr, &mut buf).unwrap();
        drop(alloc);
        assert_unreadable(SelfMemory::new().read_exact(addr, &mut buf), "freed page");
    }

    #[test]
    fn read_from_committed_into_reserved_is_an_error() {
        let alloc = Alloc::new(2 * PAGE, PAGE);
        alloc.fill(0, PAGE, 0x77);
        let mem = SelfMemory::new();
        let mut buf = [0u8; 32];
        // Entirely inside the committed page.
        mem.read_exact(alloc.addr() + PAGE as u64 - 32, &mut buf)
            .unwrap();
        // Crosses into the reserved, uncommitted page.
        assert_unreadable(
            mem.read_exact(alloc.addr() + PAGE as u64 - 16, &mut buf),
            "committed to reserved",
        );
        // Starts in the reserved page.
        assert_unreadable(
            mem.read_exact(alloc.addr() + PAGE as u64, &mut buf[..1]),
            "reserved",
        );
        // The buffer must not be mistaken for a partial success.
        let mut big = vec![0u8; 2 * PAGE];
        assert!(mem.read_exact(alloc.addr(), &mut big).is_err());
    }

    #[test]
    fn regions_describe_allocations_and_skip_reserved_memory() {
        let alloc = Alloc::new(3 * PAGE, 2 * PAGE);
        alloc.protect(PAGE, PAGE, PAGE_NOACCESS);
        let regions = SelfMemory::new().regions().unwrap();
        let first = regions
            .iter()
            .find(|region| region.contains(alloc.addr()))
            .expect("committed page is listed");
        assert!(first.perms.read && first.perms.write && !first.perms.execute);
        let no_access = regions
            .iter()
            .find(|region| region.contains(alloc.addr() + PAGE as u64))
            .expect("no-access page is listed");
        assert!(!no_access.perms.read);
        assert!(
            regions
                .iter()
                .all(|region| !region.contains(alloc.addr() + 2 * PAGE as u64)),
            "reserved memory must not be listed"
        );
    }
}

#[cfg(unix)]
mod unix_pages {
    use std::ptr;

    use super::*;

    struct Map {
        base: *mut libc::c_void,
        len: usize,
    }

    impl Map {
        fn new(pages: usize, prot: i32) -> Self {
            let len = pages * PAGE;
            // SAFETY: anonymous private mapping with no fixed address; the result is checked.
            let base = unsafe {
                libc::mmap(
                    ptr::null_mut(),
                    len,
                    prot,
                    libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
                    -1,
                    0,
                )
            };
            assert_ne!(base, libc::MAP_FAILED, "mmap failed");
            Self { base, len }
        }

        fn addr(&self) -> u64 {
            self.base as usize as u64
        }

        fn protect(&self, page: usize, prot: i32) {
            // SAFETY: the range lies inside this mapping.
            let rc = unsafe {
                libc::mprotect(self.base.cast::<u8>().add(page * PAGE).cast(), PAGE, prot)
            };
            assert_eq!(rc, 0, "mprotect failed");
        }

        fn unmap(&mut self) {
            if !self.base.is_null() {
                // SAFETY: unmaps exactly what `new` mapped, once.
                unsafe { libc::munmap(self.base, self.len) };
                self.base = ptr::null_mut();
            }
        }

        fn unmap_page(&self, page: usize) {
            // SAFETY: unmaps one page inside this mapping; the final munmap in Drop
            // tolerates holes.
            let rc = unsafe { libc::munmap(self.base.cast::<u8>().add(page * PAGE).cast(), PAGE) };
            assert_eq!(rc, 0);
        }
    }

    impl Drop for Map {
        fn drop(&mut self) {
            self.unmap();
        }
    }

    #[test]
    fn prot_none_page_is_an_error() {
        let map = Map::new(1, libc::PROT_READ | libc::PROT_WRITE);
        map.protect(0, libc::PROT_NONE);
        let mut buf = [0u8; 16];
        assert_unreadable(
            SelfMemory::new().read_exact(map.addr(), &mut buf),
            "PROT_NONE",
        );
    }

    #[test]
    fn unmapped_page_is_an_error() {
        let mut map = Map::new(1, libc::PROT_READ | libc::PROT_WRITE);
        let addr = map.addr();
        let mut buf = [0u8; 16];
        SelfMemory::new().read_exact(addr, &mut buf).unwrap();
        map.unmap();
        assert_unreadable(SelfMemory::new().read_exact(addr, &mut buf), "unmapped");
    }

    #[test]
    fn read_crossing_into_an_inaccessible_page_is_an_error() {
        let map = Map::new(3, libc::PROT_READ | libc::PROT_WRITE);
        map.protect(1, libc::PROT_NONE);
        map.unmap_page(2);
        let mem = SelfMemory::new();
        let mut buf = [0u8; 32];
        assert_unreadable(
            mem.read_exact(map.addr() + PAGE as u64 - 16, &mut buf),
            "into PROT_NONE",
        );
        assert_unreadable(
            mem.read_exact(map.addr() + 2 * PAGE as u64 - 16, &mut buf),
            "into unmapped",
        );
        mem.read_exact(map.addr() + PAGE as u64 - 32, &mut buf)
            .unwrap();
    }

    #[test]
    fn regions_and_modules_come_from_proc() {
        let mem = SelfMemory::new();
        let regions = mem.regions().unwrap();
        assert!(!regions.is_empty());
        let local = 0u8;
        let here = &local as *const u8 as u64;
        assert!(
            regions
                .iter()
                .any(|region| region.contains(here) && region.perms.write)
        );
        assert!(!mem.modules().unwrap().is_empty());
    }
}
