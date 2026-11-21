//! Memory readers: self, process, minidump and snapshot, plus a page cache. The only write is `SelfMemory::write_exact`.
//!
//! Everything is built on the [`MemoryReader`] trait. Code that analyses a
//! target takes a reader and cannot tell whether it runs inside the game
//! ([`SelfMemory`]), against another process ([`ProcessMemory`]), or against a
//! file ([`MinidumpMemory`]).
//!
//! # Safety model
//!
//! The target is untrusted and may change while it is read. No reader in this
//! crate dereferences a pointer into the target. Live readers use the
//! operating system's cross-process copy call, which reports an error for
//! unreadable memory. Offline readers bounds-check every offset. A read either
//! fills the whole buffer or returns an error, never a partial result.
//!
//! The only write is [`SelfMemory::write_exact`], which changes the current
//! process and nothing else. It is not part of [`MemoryReader`], so code that
//! takes a reader, such as the dump, cannot reach it. It refuses any range that
//! is not committed read-write memory and never changes page protection.
//! [`ProcessMemory`] is opened without the write right and stays read-only.
#![deny(unsafe_op_in_unsafe_fn)]

mod cache;
mod error;
mod live;
#[cfg_attr(
    not(target_os = "linux"),
    allow(
        dead_code,
        reason = "the maps parser is only used by the Linux backend, but tested everywhere"
    )
)]
mod maps;
mod minidump;
mod pod;
mod protect;
mod reader;
mod snapshot;
mod sys;
mod types;

pub use cache::{CacheStats, DEFAULT_MAX_PAGES, PAGE_SIZE, PageCache};
pub use error::MemError;
pub use live::{ProcessMemory, SelfMemory, fast_read_faults, list_processes};
pub use minidump::MinidumpMemory;
pub use pod::Pod;
pub use reader::{
    Addr, MAX_CSTR_LEN, MAX_READ_VEC_BYTES, MAX_READ_VEC_COUNT, MemoryReader, MemoryReaderExt,
};
pub use snapshot::{Snapshot, SnapshotBuilder};
#[cfg(windows)]
pub use sys::PROCESS_OPEN_ACCESS;
pub use types::{MemoryRegion, ModuleInfo, Perms, ProcessInfo, RegionKind};
