//! Fallback for platforms with no backend. Every operation fails with
//! [`MemError::Unsupported`]; nothing pretends to work.

use crate::error::MemError;
use crate::reader::Addr;
use crate::types::{MemoryRegion, ModuleInfo, ProcessInfo};

/// A process handle that cannot exist on this platform.
pub(crate) struct Target {
    pid: u32,
}

impl Target {
    pub(crate) fn current() -> Self {
        Self {
            pid: std::process::id(),
        }
    }

    /// The current process through the same copy call as [`Target::current`]:
    /// this platform has no direct path.
    pub(crate) fn current_os_copy() -> Self {
        Self::current()
    }

    pub(crate) fn open(_pid: u32) -> Result<Self, MemError> {
        Err(MemError::Unsupported {
            operation: "ProcessMemory::open",
        })
    }

    pub(crate) fn pid(&self) -> u32 {
        self.pid
    }

    pub(crate) fn read(&self, _addr: Addr, _buf: &mut [u8]) -> Result<(), MemError> {
        Err(MemError::Unsupported {
            operation: "read_exact",
        })
    }

    pub(crate) fn write(&self, _addr: Addr, _bytes: &[u8]) -> Result<(), MemError> {
        Err(MemError::Unsupported {
            operation: "write_exact",
        })
    }

    pub(crate) fn regions(&self) -> Result<Vec<MemoryRegion>, MemError> {
        Err(MemError::Unsupported {
            operation: "regions",
        })
    }

    pub(crate) fn modules(&self) -> Result<Vec<ModuleInfo>, MemError> {
        Err(MemError::Unsupported {
            operation: "modules",
        })
    }
}

pub(crate) fn list_processes() -> Result<Vec<ProcessInfo>, MemError> {
    Err(MemError::Unsupported {
        operation: "list_processes",
    })
}
