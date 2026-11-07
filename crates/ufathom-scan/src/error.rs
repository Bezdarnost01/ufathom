//! The error type of this crate.

use std::io;

use thiserror::Error;
use ufathom_mem::MemError;

/// Why a scan step failed.
///
/// Every variant says what was being parsed and why it was rejected. The
/// enum is `#[non_exhaustive]`: later steps add their own causes.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum ScanError {
    /// A read of target memory failed.
    #[error("memory read failed: {0}")]
    Memory(#[from] MemError),

    /// A file could not be opened or read.
    #[error("I/O error on {context}: {source}")]
    Io {
        /// The file, or a description of the stream.
        context: String,
        /// The underlying error.
        #[source]
        source: io::Error,
    },

    /// The data does not start like a PE image.
    #[error("not a PE image: {reason}")]
    NotPe {
        /// What was missing.
        reason: String,
    },

    /// The data is a PE image of a kind this crate does not read.
    #[error("unsupported PE image: {reason}")]
    Unsupported {
        /// What is not supported.
        reason: String,
    },

    /// A structure is inconsistent: a size, offset or tree that cannot be
    /// right. Everything read from a target is untrusted, so this is the
    /// normal outcome for damaged or hostile input.
    #[error("malformed {what}: {reason}")]
    Malformed {
        /// The structure, such as `section table` or `resource directory`.
        what: &'static str,
        /// What is wrong, with the offending value when there is one.
        reason: String,
    },

    /// A count or size read from the data is above a named limit.
    #[error("{what} exceeds its limit: {value} > {limit}")]
    LimitExceeded {
        /// What was counted.
        what: &'static str,
        /// The value found.
        value: u64,
        /// The largest accepted value.
        limit: u64,
    },

    /// A section's raw data runs past the end of the file.
    #[error(
        "file is truncated: {section} needs bytes up to {needed_end:#x} but the file has {file_len:#x}"
    )]
    TruncatedFile {
        /// Name of the section, or `headers`.
        section: String,
        /// File offset the section's data must reach.
        needed_end: u64,
        /// Actual length of the file.
        file_len: u64,
    },
}

impl ScanError {
    /// Builds [`ScanError::Malformed`].
    pub(crate) fn malformed(what: &'static str, reason: impl Into<String>) -> Self {
        Self::Malformed {
            what,
            reason: reason.into(),
        }
    }
}
