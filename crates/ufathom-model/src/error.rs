//! Errors returned by this crate.

use thiserror::Error;

/// Failure to serialize or deserialize a [`Dump`](crate::Dump).
#[derive(Debug, Error)]
pub enum ModelError {
    /// The dump could not be written as JSON.
    #[error("failed to serialize dump: {0}")]
    Serialize(#[source] serde_json::Error),

    /// The text is not JSON, or does not have the shape of a dump.
    #[error("failed to parse dump JSON: {0}")]
    Parse(#[source] serde_json::Error),

    /// The dump was written by a tool using a different format version.
    ///
    /// Reading it anyway could silently misinterpret fields, so it is refused.
    #[error("unsupported dump format_version {found}; this build reads only version {supported}")]
    UnsupportedFormatVersion {
        /// The `format_version` found in the input.
        found: u64,
        /// The only version this build understands.
        supported: u32,
    },
}
