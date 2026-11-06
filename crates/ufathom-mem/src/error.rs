//! The error type shared by every reader in this crate.

use std::io;

use crate::Addr;

/// Why a memory operation failed.
///
/// Addresses are printed in hex. The enum is `#[non_exhaustive]` so that new
/// causes can be added without breaking callers that match on it.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum MemError {
    /// The range is not mapped, not readable, or only partly so. A read that
    /// crosses from readable into unreadable memory reports this, never a
    /// partial success.
    #[error("unreadable memory at {addr:#x} ({len:#x} bytes)")]
    Unreadable {
        /// First address of the failing range.
        addr: Addr,
        /// Length of the failing range in bytes.
        len: u64,
    },

    /// `addr + len` does not fit in a 64-bit address.
    #[error("address range overflows: {addr:#x} + {len:#x}")]
    AddressOverflow {
        /// Start of the requested range.
        addr: Addr,
        /// Requested length in bytes.
        len: u64,
    },

    /// The operating system refused the request.
    #[error("access denied: {context}")]
    AccessDenied {
        /// The operation that was refused.
        context: String,
    },

    /// The target process has exited or never existed.
    #[error("process {pid} is gone")]
    ProcessGone {
        /// Process identifier.
        pid: u32,
    },

    /// A count or size from the caller or from target data is above a limit.
    #[error("{what} exceeds limit: {requested} > {limit}")]
    LimitExceeded {
        /// What was being measured.
        what: &'static str,
        /// The requested value.
        requested: u64,
        /// The largest accepted value.
        limit: u64,
    },

    /// A file or buffer is not a valid instance of its format.
    #[error("invalid {format}: {reason}")]
    InvalidFormat {
        /// Format name, such as `minidump`.
        format: &'static str,
        /// What is wrong, including the offending offset when known.
        reason: String,
    },

    /// A snapshot builder was given regions that cannot form an address space.
    #[error("invalid region at {base:#x} ({size:#x} bytes): {reason}")]
    InvalidRegion {
        /// Region base address.
        base: Addr,
        /// Region size in bytes.
        size: u64,
        /// What is wrong.
        reason: String,
    },

    /// An operating system call failed for a reason not covered above.
    #[error("{operation} failed with OS error {code}")]
    Os {
        /// The failing call.
        operation: &'static str,
        /// The raw OS error code.
        code: i32,
    },

    /// The operation does not exist on this platform.
    #[error("{operation} is not supported on this platform")]
    Unsupported {
        /// The unavailable operation.
        operation: &'static str,
    },

    /// An I/O error.
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),
}

impl MemError {
    /// Builds [`MemError::Unreadable`] from a slice length.
    pub(crate) fn unreadable(addr: Addr, len: usize) -> Self {
        Self::Unreadable {
            addr,
            len: len as u64,
        }
    }

    /// Builds [`MemError::InvalidFormat`].
    pub(crate) fn invalid(format: &'static str, reason: impl Into<String>) -> Self {
        Self::InvalidFormat {
            format,
            reason: reason.into(),
        }
    }
}

/// Checks that `addr + len` does not overflow and returns the exclusive end.
pub(crate) fn range_end(addr: Addr, len: usize) -> Result<u64, MemError> {
    addr.checked_add(len as u64)
        .ok_or(MemError::AddressOverflow {
            addr,
            len: len as u64,
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_carry_hex_addresses() {
        let msg = MemError::Unreadable {
            addr: 0xdead_beef,
            len: 16,
        }
        .to_string();
        assert!(msg.contains("0xdeadbeef"), "{msg}");
        let msg = MemError::AddressOverflow {
            addr: u64::MAX,
            len: 2,
        }
        .to_string();
        assert!(msg.contains("0xffffffffffffffff"), "{msg}");
    }

    #[test]
    fn range_end_detects_overflow() {
        assert_eq!(range_end(10, 5).ok(), Some(15));
        assert!(matches!(
            range_end(u64::MAX, 1),
            Err(MemError::AddressOverflow { .. })
        ));
        assert_eq!(range_end(u64::MAX, 0).ok(), Some(u64::MAX));
    }
}
