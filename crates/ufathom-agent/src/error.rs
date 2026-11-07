//! The error types of this crate.

use std::io;
use std::path::PathBuf;

use thiserror::Error;

/// A hotkey name that is not a supported key.
#[derive(Debug, Error)]
#[error(
    "unknown hotkey {name:?}: use F1 to F24, a letter, a digit, Insert, Delete, Home, End, PageUp, PageDown, Pause, ScrollLock, Tab, Backspace, or none"
)]
pub struct HotkeyError {
    /// The name as written.
    pub name: String,
}

/// Why a config file was not used. The caller falls back to defaults.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum ConfigError {
    /// The file exists but could not be read.
    #[error("cannot read {}: {source}", path.display())]
    Read {
        /// The config file.
        path: PathBuf,
        /// The underlying error.
        #[source]
        source: io::Error,
    },
    /// The file is not valid TOML, or has a key the config does not know, or
    /// a value of the wrong type.
    #[error("cannot parse {}: {message}", path.display())]
    Parse {
        /// The config file.
        path: PathBuf,
        /// What the parser reported.
        message: String,
    },
    /// The file parsed, but a value is out of range or not usable.
    #[error("invalid value for `{key}` in {}: {message}", path.display())]
    Invalid {
        /// The config file.
        path: PathBuf,
        /// The offending key.
        key: &'static str,
        /// Why the value is refused.
        message: String,
    },
}

/// Why a session could not be created.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum SessionError {
    /// The session's own thread pool could not be built.
    #[error("cannot build the thread pool: {0}")]
    Pool(String),
}

/// Why a dump produced no result.
///
/// Every variant that happens after discovery began also leaves a folder with
/// `identity.json` and `ufathom.log` behind, named by
/// [`DumpError::folder`], so a failed dump can be reported.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum DumpError {
    /// The cancel flag was set. Nothing was written.
    #[error("the dump was cancelled")]
    Cancelled,
    /// Discovery could not solve the layout of the target.
    #[error("discovery failed: {message}")]
    Discovery {
        /// The scan error, as text.
        message: String,
        /// The folder holding the failure record, if it could be written.
        folder: Option<PathBuf>,
    },
    /// The reflection pass could not build a dump.
    #[error("reflection failed: {message}")]
    Reflect {
        /// The reflect error, as text.
        message: String,
        /// The folder holding the failure record, if it could be written.
        folder: Option<PathBuf>,
    },
    /// An output file or folder could not be written.
    #[error("cannot write the output: {message}")]
    Output {
        /// What failed.
        message: String,
        /// The folder that was being written, if it was created.
        folder: Option<PathBuf>,
    },
    /// The dump code panicked. The panic was contained.
    #[error("the dump panicked: {message}")]
    Panicked {
        /// The panic message.
        message: String,
    },
}

impl DumpError {
    /// A short stable name of the failure, for results and logs.
    #[must_use]
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Cancelled => "cancelled",
            Self::Discovery { .. } => "discovery",
            Self::Reflect { .. } => "reflect",
            Self::Output { .. } => "output",
            Self::Panicked { .. } => "panic",
        }
    }

    /// The folder that holds the failure record, when one was written.
    #[must_use]
    pub fn folder(&self) -> Option<&PathBuf> {
        match self {
            Self::Discovery { folder, .. }
            | Self::Reflect { folder, .. }
            | Self::Output { folder, .. } => folder.as_ref(),
            Self::Cancelled | Self::Panicked { .. } => None,
        }
    }
}

/// Why the service could not start.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum ServiceError {
    /// The session could not be created.
    #[error(transparent)]
    Session(#[from] SessionError),
    /// The control folders could not be created.
    #[error("cannot create the control folder {}: {source}", path.display())]
    Control {
        /// The folder.
        path: PathBuf,
        /// The underlying error.
        #[source]
        source: io::Error,
    },
}
