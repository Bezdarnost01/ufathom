//! The error type of the generator.

use std::path::PathBuf;

/// Why generation failed.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum CodegenError {
    /// A file or directory could not be written, read or removed.
    #[error("{}: {source}", path.display())]
    Io {
        /// The path concerned.
        path: PathBuf,
        /// The operating system error.
        #[source]
        source: std::io::Error,
    },
    /// An option is not usable.
    #[error("invalid option: {0}")]
    InvalidOption(String),
    /// The generator broke one of its own rules.
    #[error("internal error: {0}")]
    Internal(String),
}
