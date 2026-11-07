//! The error type of this crate.

use thiserror::Error;
use ufathom_mem::{Addr, MemError};

/// Why a view could not read or decode something.
///
/// Every variant names what was being read and, where one exists, the target
/// address. The enum is `#[non_exhaustive]` so causes can be added.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum EngineError {
    /// A read of target memory failed.
    #[error("cannot read {what} at {addr:#x}: {source}")]
    Read {
        /// First address of the failing read.
        addr: Addr,
        /// What was being read.
        what: &'static str,
        /// The reader's error.
        #[source]
        source: MemError,
    },

    /// A pointer that must not be null was null.
    #[error("{what} at {addr:#x} is null")]
    Null {
        /// Address holding the pointer.
        addr: Addr,
        /// What the pointer should have led to.
        what: &'static str,
    },

    /// A count or length read from the target is above a named limit.
    #[error("{what} at {addr:#x} exceeds its limit: {value} > {limit}")]
    Limit {
        /// Address of the structure that holds the value.
        addr: Addr,
        /// What was counted.
        what: &'static str,
        /// The value found.
        value: u64,
        /// The largest accepted value.
        limit: u64,
    },

    /// A linked structure loops back on itself.
    #[error("{what} at {addr:#x} forms a cycle")]
    Cycle {
        /// The node reached a second time.
        addr: Addr,
        /// What kind of chain it is.
        what: &'static str,
    },

    /// A named module is not loaded in the target.
    #[error("module {module} is not loaded in the target")]
    ModuleNotFound {
        /// The module that was asked for.
        module: String,
    },

    /// The layout cannot be used for this read.
    #[error("unusable layout for {what}: {reason}")]
    InvalidLayout {
        /// The structure the layout describes.
        what: &'static str,
        /// What is wrong with it.
        reason: String,
    },

    /// A name index has no entry in the name table.
    #[error("name index {index:#x} does not resolve")]
    UnresolvedName {
        /// The index that was looked up.
        index: u32,
    },

    /// Bytes handed to a decoder do not have the expected shape.
    #[error("malformed {what}: {reason}")]
    Malformed {
        /// What was being decoded.
        what: &'static str,
        /// What is wrong.
        reason: String,
    },

    /// Data read from the target contradicts itself.
    #[error("{what} at {addr:#x} is inconsistent: {reason}")]
    Inconsistent {
        /// Address of the data.
        addr: Addr,
        /// What was being checked.
        what: &'static str,
        /// The contradiction.
        reason: String,
    },

    /// Another error, with the address and purpose of the failing step.
    #[error("{what} at {addr:#x}: {source}")]
    Context {
        /// Address being processed.
        addr: Addr,
        /// What was being processed.
        what: &'static str,
        /// The underlying error.
        #[source]
        source: Box<EngineError>,
    },
}

impl EngineError {
    /// Builds the closure that turns a [`MemError`] into [`EngineError::Read`].
    pub(crate) fn read(addr: Addr, what: &'static str) -> impl FnOnce(MemError) -> Self {
        move |source| Self::Read { addr, what, source }
    }

    /// Wraps `self` with the address and purpose of the step that failed.
    #[must_use]
    pub fn at(self, addr: Addr, what: &'static str) -> Self {
        Self::Context {
            addr,
            what,
            source: Box::new(self),
        }
    }

    /// The error under any number of [`EngineError::Context`] wrappers.
    #[must_use]
    pub fn root(&self) -> &Self {
        let mut current = self;
        while let Self::Context { source, .. } = current {
            current = source;
        }
        current
    }

    /// The target address the error names, if it names one.
    #[must_use]
    pub fn addr(&self) -> Option<Addr> {
        match self {
            Self::Read { addr, .. }
            | Self::Null { addr, .. }
            | Self::Limit { addr, .. }
            | Self::Cycle { addr, .. }
            | Self::Inconsistent { addr, .. }
            | Self::Context { addr, .. } => Some(*addr),
            Self::ModuleNotFound { .. }
            | Self::InvalidLayout { .. }
            | Self::UnresolvedName { .. }
            | Self::Malformed { .. } => None,
        }
    }
}
