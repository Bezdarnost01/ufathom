//! `SelfMemory::write_exact` writes plain read-write memory and refuses
//! everything else without faulting and without changing page protection.
//!
//! A fault would crash the test process, so passing means the process
//! survived as well as the assertions.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "test code may panic on failure; clippy.toml only exempts #[test] functions, not helpers"
)]
use ufathom_mem::{MemError, MemoryReader, SelfMemory};

const PAGE: usize = 4096;

fn assert_not_writable(result: Result<(), MemError>, what: &str) {
    match result {
        Err(MemError::NotWritable { .. } | MemError::AddressOverflow { .. }) => {}
        other => panic!("{what}: expected a not-writable/overflow error, got {other:?}"),
    }
}

/// Two adjacent pages from the OS, so a test can give each its own protection.
/// Dropping unmaps them.
struct Pages {
    base: usize,
}

#[cfg(windows)]
mod os {
    use super::{PAGE, Pages};
    use windows_sys::Win32::System::Memory::{
        MEM_COMMIT, MEM_RELEASE, MEM_RESERVE, MEMORY_BASIC_INFORMATION, PAGE_GUARD, PAGE_NOACCESS,
        PAGE_READONLY, PAGE_READWRITE, VirtualAlloc, VirtualFree, VirtualProtect, VirtualQuery,
    };

    pub const GUARD: u32 = PAGE_READWRITE | PAGE_GUARD;
    pub const READ_ONLY: u32 = PAGE_READONLY;
    pub const NO_ACCESS: u32 = PAGE_NOACCESS;
    pub const READ_WRITE: u32 = PAGE_READWRITE;

    impl Pages {
        pub fn new() -> Self {
            // SAFETY: allocates fresh committed read-write pages.
            let base = unsafe {
                VirtualAlloc(
                    std::ptr::null(),
                    2 * PAGE,
                    MEM_COMMIT | MEM_RESERVE,
                    PAGE_READWRITE,
                )
            };
            assert!(!base.is_null());
            Self {
                base: base as usize,
            }
        }

        pub fn protect(&self, page: usize, protect: u32) {
            let mut old = 0u32;
            // SAFETY: the page lies inside the allocation made in `new`.
            let ok = unsafe {
                VirtualProtect(
                    (self.base + page * PAGE) as *const _,
                    PAGE,
                    protect,
                    &mut old,
                )
            };
            assert_ne!(ok, 0, "VirtualProtect");
        }

        /// The current protection of a page.
        pub fn protection(&self, page: usize) -> u32 {
            let mut info = MEMORY_BASIC_INFORMATION::default();
            // SAFETY: `info` is a valid out buffer of the size passed.
            let written = unsafe {
                VirtualQuery(
                    (self.base + page * PAGE) as *const _,
                    &mut info,
                    size_of::<MEMORY_BASIC_INFORMATION>(),
                )
            };
            assert_ne!(written, 0);
            info.Protect
        }

        /// Returns the pages to the OS and leaves their addresses unmapped.
        pub fn release(&mut self) {
            if self.base != 0 {
                // SAFETY: frees the whole allocation made in `new`, once.
                unsafe { VirtualFree(self.base as *mut _, 0, MEM_RELEASE) };
                self.base = 0;
            }
        }
    }
}

#[cfg(unix)]
mod os {
    use super::{PAGE, Pages};

    pub const READ_ONLY: u32 = libc::PROT_READ as u32;
    pub const NO_ACCESS: u32 = libc::PROT_NONE as u32;
    pub const READ_WRITE: u32 = (libc::PROT_READ | libc::PROT_WRITE) as u32;

    impl Pages {
        pub fn new() -> Self {
            // SAFETY: maps fresh anonymous read-write pages.
            let base = unsafe {
                libc::mmap(
                    std::ptr::null_mut(),
                    2 * PAGE,
                    libc::PROT_READ | libc::PROT_WRITE,
                    libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
                    -1,
                    0,
                )
            };
            assert_ne!(base, libc::MAP_FAILED);
            Self {
                base: base as usize,
            }
        }

        pub fn protect(&self, page: usize, protect: u32) {
            // SAFETY: the page lies inside the mapping made in `new`.
            let rc = unsafe {
                libc::mprotect(
                    (self.base + page * PAGE) as *mut _,
                    PAGE,
                    protect as libc::c_int,
                )
            };
            assert_eq!(rc, 0, "mprotect");
        }

        /// The current protection of a page, from `/proc/self/maps`.
        pub fn protection(&self, page: usize) -> u32 {
            let at = self.base + page * PAGE;
            let maps = std::fs::read_to_string("/proc/self/maps").unwrap();
            for line in maps.lines() {
                let (range, rest) = line.split_once(' ').unwrap();
                let (start, end) = range.split_once('-').unwrap();
                let start = usize::from_str_radix(start, 16).unwrap();
                let end = usize::from_str_radix(end, 16).unwrap();
                if (start..end).contains(&at) {
                    let perms = rest.split(' ').next().unwrap().as_bytes();
                    let mut bits = 0;
                    if perms[0] == b'r' {
                        bits |= libc::PROT_READ;
                    }
                    if perms[1] == b'w' {
                        bits |= libc::PROT_WRITE;
                    }
                    return bits as u32;
                }
            }
            panic!("page not mapped");
        }

        /// Returns the pages to the OS and leaves their addresses unmapped.
        pub fn release(&mut self) {
            if self.base != 0 {
                // SAFETY: unmaps the whole mapping made in `new`, once.
                unsafe { libc::munmap(self.base as *mut _, 2 * PAGE) };
                self.base = 0;
            }
        }
    }
}

impl Pages {
    fn addr(&self, page: usize) -> u64 {
        (self.base + page * PAGE) as u64
    }

    /// Reads back through the normal reader. Fails for unreadable pages.
    fn read(&self, mem: &SelfMemory, addr: u64, len: usize) -> Result<Vec<u8>, MemError> {
        let mut buf = vec![0u8; len];
        mem.read_exact(addr, &mut buf)?;
        Ok(buf)
    }
}

impl Drop for Pages {
    fn drop(&mut self) {
        self.release();
    }
}

#[test]
fn writes_into_a_heap_buffer() {
    let mem = SelfMemory::new();
    let mut buffer = vec![0u8; 64];
    mem.write_exact(buffer.as_ptr() as u64 + 8, &[1, 2, 3, 4])
        .unwrap();
    assert_eq!(
        &buffer[..16],
        &[0, 0, 0, 0, 0, 0, 0, 0, 1, 2, 3, 4, 0, 0, 0, 0]
    );
    // The whole buffer, and an empty write.
    let all: Vec<u8> = (0..64).collect();
    mem.write_exact(buffer.as_mut_ptr() as u64, &all).unwrap();
    assert_eq!(buffer, all);
    mem.write_exact(buffer.as_ptr() as u64, &[]).unwrap();
    assert_eq!(buffer, all);
    // A write is visible through the normal reader too.
    let mut word = 0x1122_3344_5566_7788u64;
    mem.write_exact(
        &raw mut word as u64,
        &0xaabb_ccdd_eeff_0011u64.to_le_bytes(),
    )
    .unwrap();
    assert_eq!(word, 0xaabb_ccdd_eeff_0011);
}

#[test]
fn writes_across_two_adjacent_read_write_pages() {
    let mem = SelfMemory::new();
    let pages = Pages::new();
    let at = pages.addr(1) - 4;
    mem.write_exact(at, &[9; 8]).unwrap();
    assert_eq!(pages.read(&mem, at, 8).unwrap(), vec![9; 8]);
}

#[test]
fn refuses_null_kernel_and_extreme_addresses() {
    let mem = SelfMemory::new();
    for addr in [
        0u64,
        1,
        0x10,
        0xffff,
        0x7fff_ffff_0000,
        0x8000_0000_0000,
        0xffff_8000_0000_0000,
        u64::MAX - 63,
        u64::MAX - 1,
        u64::MAX,
    ] {
        assert_not_writable(mem.write_exact(addr, &[0xcc; 8]), "extreme address");
        assert_not_writable(mem.write_exact(addr, &[0xcc]), "extreme address");
    }
    // A range that overflows the address space.
    assert!(matches!(
        mem.write_exact(u64::MAX - 3, &[0; 8]),
        Err(MemError::AddressOverflow { .. })
    ));
}

#[test]
fn refuses_an_unmapped_page() {
    let mem = SelfMemory::new();
    let mut pages = Pages::new();
    let at = pages.addr(0);
    mem.write_exact(at, &[1]).unwrap();
    pages.release();
    assert_not_writable(mem.write_exact(at, &[2]), "released page");
    assert_not_writable(mem.write_exact(at + 16, &[2; 16]), "released page");
}

#[test]
fn refuses_a_read_only_page_and_leaves_its_protection() {
    let mem = SelfMemory::new();
    let pages = Pages::new();
    mem.write_exact(pages.addr(0), &[7; 4]).unwrap();
    pages.protect(0, os::READ_ONLY);
    assert_not_writable(mem.write_exact(pages.addr(0), &[1; 4]), "read-only page");
    assert_eq!(pages.protection(0), os::READ_ONLY, "protection was changed");
    assert_eq!(pages.read(&mem, pages.addr(0), 4).unwrap(), vec![7; 4]);
}

#[test]
fn refuses_a_no_access_page() {
    let mem = SelfMemory::new();
    let pages = Pages::new();
    pages.protect(0, os::NO_ACCESS);
    assert_not_writable(mem.write_exact(pages.addr(0), &[1; 4]), "no-access page");
    pages.protect(0, os::READ_WRITE);
    assert_eq!(pages.read(&mem, pages.addr(0), 4).unwrap(), vec![0; 4]);
}

#[cfg(windows)]
#[test]
fn refuses_a_guard_page_without_consuming_the_guard() {
    let mem = SelfMemory::new();
    let pages = Pages::new();
    pages.protect(1, os::GUARD);
    assert_not_writable(mem.write_exact(pages.addr(1), &[1; 4]), "guard page");
    // Touching the page would have cleared PAGE_GUARD.
    assert_eq!(pages.protection(1), os::GUARD, "the guard was consumed");
}

#[test]
fn a_write_spanning_writable_and_read_only_pages_changes_nothing() {
    let mem = SelfMemory::new();
    let pages = Pages::new();
    pages.protect(1, os::READ_ONLY);
    let at = pages.addr(1) - 4;
    assert_not_writable(mem.write_exact(at, &[0xee; 8]), "straddling write");
    // The writable half was not touched either.
    assert_eq!(pages.read(&mem, at, 4).unwrap(), vec![0; 4]);
    assert_eq!(pages.protection(1), os::READ_ONLY);
}

#[test]
fn refuses_executable_image_memory() {
    let mem = SelfMemory::new();
    let here = refuses_executable_image_memory as fn() as usize as u64;
    let mut before = [0u8; 4];
    mem.read_exact(here, &mut before).unwrap();
    assert_not_writable(mem.write_exact(here, &[0xcc; 4]), "code page");
    let mut after = [0u8; 4];
    mem.read_exact(here, &mut after).unwrap();
    assert_eq!(before, after, "code was modified");
}
