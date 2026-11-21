//! Readers for live processes: [`SelfMemory`], [`ProcessMemory`] and [`list_processes`].

use crate::error::MemError;
use crate::reader::{Addr, MemoryReader};
use crate::sys::{self, Target};
use crate::types::{MemoryRegion, ModuleInfo, ProcessInfo};

/// Reads the memory of the current process. This is the reader used inside the DLL.
///
/// A read never faults for any address, including null, kernel addresses,
/// `u64::MAX` and ranges that overflow: it returns an error, or the whole
/// buffer filled, never a partial result.
///
/// # Read paths
///
/// [`SelfMemory::new`] takes the fast path where the platform has one
/// (Windows x86-64). [`SelfMemory::safe`] always uses the operating system's
/// cross-process copy call (`ReadProcessMemory` on Windows, `process_vm_readv`
/// on Linux), which reports an error for unmapped, no-access and guarded pages
/// instead of raising an exception, and never dereferences a pointer into the
/// target.
///
/// The fast path reads with plain loads, so a read costs a few nanoseconds
/// instead of a system call. It is made safe in three layers, described in
/// `ARCHITECTURE.md`: the range is first checked with `VirtualQuery` (results
/// are cached per thread), then copied by a hand-written routine that has its
/// own structured exception handler, which turns an access violation,
/// in-page error or guard page violation into an error, and finally any read
/// that does not complete is repeated through the system call, which supplies
/// the error. A page freed between the check and the copy therefore gives an
/// error, never a crash. Reads of guard pages are refused by the check without
/// touching the page, so, like the system call, they leave the guard flag
/// alone. A module that was mapped by hand has no registered exception table;
/// there the fast path switches itself off and reads use the system call.
pub struct SelfMemory {
    target: Target,
}

impl SelfMemory {
    /// Creates a reader for the current process that takes the fast path where
    /// the platform has one and the system call everywhere else.
    pub fn new() -> Self {
        Self {
            target: Target::current(),
        }
    }

    /// Creates a reader for the current process that only ever uses the
    /// operating system's copy call and never dereferences a pointer into the
    /// target. Slower: about 130 ns per read on Windows, a system call each.
    pub fn safe() -> Self {
        Self {
            target: Target::current_os_copy(),
        }
    }
}

/// How many times the fast read path has recovered from a fault since the
/// process started.
///
/// A fault means a page that was readable when a range was checked was freed
/// or re-protected before the copy finished; the read was then answered
/// through the system call. It should be 0 in a quiet process, and small in a
/// busy one. Always 0 on platforms without a fast path.
pub fn fast_read_faults() -> u64 {
    sys::recovered_faults()
}

impl SelfMemory {
    /// Writes `bytes` at `addr` in the current process. This is the only write
    /// in the crate, and it is deliberately not part of [`MemoryReader`]: code
    /// that takes a reader, such as the dump pipeline, cannot reach it.
    ///
    /// Like a read, it never faults. The range must lie entirely in committed,
    /// plain read-write memory, which is checked before the first byte is
    /// written (`VirtualQueryEx` on Windows, `/proc/self/maps` on Linux), so a
    /// refused write changes nothing. Page protection is never changed:
    /// read-only, no-access, guard, copy-on-write and unmapped pages are all
    /// refused with [`MemError::NotWritable`]. An empty slice succeeds.
    ///
    /// The check and the write are two steps, so a page whose protection
    /// another thread changes in between can still turn a write into an error
    /// (the copy call itself never faults). Nothing here stops the game from
    /// reading or writing the same bytes at the same time: the write is not
    /// atomic against other threads. See `ufathom-runtime` for the checked,
    /// object-level API built on this.
    ///
    /// # Errors
    ///
    /// [`MemError::NotWritable`] for a range that is not entirely writable,
    /// [`MemError::AddressOverflow`] for a range that overflows,
    /// [`MemError::LimitExceeded`] for a range spanning an absurd number of
    /// regions, [`MemError::Unsupported`] on platforms without a backend, and
    /// [`MemError::Os`] for other failures.
    pub fn write_exact(&self, addr: Addr, bytes: &[u8]) -> Result<(), MemError> {
        self.target.write(addr, bytes)
    }
}

impl Default for SelfMemory {
    fn default() -> Self {
        Self::new()
    }
}

impl MemoryReader for SelfMemory {
    fn read_exact(&self, addr: Addr, buf: &mut [u8]) -> Result<(), MemError> {
        self.target.read(addr, buf)
    }

    fn regions(&self) -> Result<Vec<MemoryRegion>, MemError> {
        self.target.regions()
    }

    fn modules(&self) -> Result<Vec<ModuleInfo>, MemError> {
        self.target.modules()
    }
}

/// Reads the memory of another process, read-only.
///
/// On Windows the process is opened with exactly
/// [`PROCESS_OPEN_ACCESS`](crate::PROCESS_OPEN_ACCESS) (`PROCESS_QUERY_INFORMATION | PROCESS_VM_READ`)
/// and the handle is closed on drop. Access refusals and exited processes are
/// reported as [`MemError::AccessDenied`] and [`MemError::ProcessGone`]; they are
/// never worked around.
pub struct ProcessMemory {
    target: Target,
}

impl ProcessMemory {
    /// Opens the process `pid` for reading.
    ///
    /// # Errors
    ///
    /// [`MemError::ProcessGone`] if no such process exists or it has exited,
    /// [`MemError::AccessDenied`] if the system refuses read access.
    pub fn open(pid: u32) -> Result<Self, MemError> {
        Ok(Self {
            target: Target::open(pid)?,
        })
    }

    /// The identifier of the process being read.
    pub fn pid(&self) -> u32 {
        self.target.pid()
    }
}

impl MemoryReader for ProcessMemory {
    fn read_exact(&self, addr: Addr, buf: &mut [u8]) -> Result<(), MemError> {
        self.target.read(addr, buf)
    }

    fn regions(&self) -> Result<Vec<MemoryRegion>, MemError> {
        self.target.regions()
    }

    fn modules(&self) -> Result<Vec<ModuleInfo>, MemError> {
        self.target.modules()
    }
}

/// Lists the processes running on the machine.
///
/// A process whose image path the system refuses to reveal is still listed,
/// with `path: None`. One process that cannot be inspected never fails the
/// whole listing. To resolve paths this opens each process with the minimal
/// query-only right (`PROCESS_QUERY_LIMITED_INFORMATION` on Windows); it never
/// reads memory.
///
/// # Errors
///
/// Only if the process list itself cannot be obtained.
pub fn list_processes() -> Result<Vec<ProcessInfo>, MemError> {
    sys::list_processes()
}
