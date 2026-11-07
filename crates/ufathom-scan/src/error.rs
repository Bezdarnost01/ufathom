//! The error type of this crate.

use std::io;

use thiserror::Error;
use ufathom_mem::MemError;

/// Why discovery could not produce a layout.
///
/// Every variant names what was being solved. Discovery never guesses: a
/// field with no evidence is an error, not a default.
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

    /// The module list or region list of the target could not be read.
    #[error("cannot read the {what} of the target: {source}")]
    Target {
        /// What was being listed.
        what: &'static str,
        /// The reader's error.
        #[source]
        source: MemError,
    },

    /// No module matches the request.
    #[error("module not found: {}", requested.as_deref().unwrap_or("no .exe module is loaded"))]
    ModuleNotFound {
        /// The module that was asked for, or `None` for the default.
        requested: Option<String>,
    },

    /// The module has no readable and writable memory to search.
    #[error("module {module} has no writable data to search")]
    NoWritableData {
        /// The module searched.
        module: String,
    },

    /// No object array was confirmed.
    #[error(
        "object array not found: {shape_candidates} shape candidates over {slots} pointer slots, \
         {confirmed} confirmed by item inspection{hint}"
    )]
    ObjectArrayNotFound {
        /// Pointer slots that passed the pointer test.
        slots: usize,
        /// Slots whose neighbouring counts fit an object array shape.
        shape_candidates: usize,
        /// Candidates whose items stored their own index.
        confirmed: usize,
        /// Advice appended to the message.
        hint: &'static str,
    },

    /// Several candidate object arrays were found and each failed a later
    /// step.
    #[error("{} candidate object arrays were rejected: {}", rejected.len(), describe(rejected))]
    CandidatesRejected {
        /// Every candidate, in the order it was tried.
        rejected: Vec<RejectedCandidate>,
    },

    /// No name table was confirmed.
    #[error("name table not found: {reason}")]
    NameTableNotFound {
        /// What was tried and why it failed.
        reason: String,
    },

    /// A field has no evidence.
    #[error("no evidence for {field}: {detail}")]
    MissingEvidence {
        /// The field being solved.
        field: &'static str,
        /// What was missing.
        detail: String,
    },

    /// Several values fit the evidence equally well.
    #[error("{field} is ambiguous: {candidates}")]
    Ambiguous {
        /// The field being solved.
        field: &'static str,
        /// The candidates, formatted.
        candidates: String,
    },

    /// A value given by the caller contradicts the evidence.
    #[error("override for {field} is {given:#x} but the target shows {found:#x}")]
    OverrideContradiction {
        /// The overridden field.
        field: &'static str,
        /// The value given.
        given: u64,
        /// The value the evidence supports.
        found: u64,
    },

    /// A value given by the caller breaks an invariant of the engine.
    #[error("override for {field} is {given:#x}, which breaks an invariant: {reason}")]
    InvalidOverride {
        /// The overridden field.
        field: &'static str,
        /// The value given.
        given: u64,
        /// What it breaks.
        reason: String,
    },

    /// The solved layout breaks a global invariant of the engine.
    #[error("invariant violated: {invariant}: {violations} of {checked} checked ({example})")]
    ValidationFailed {
        /// The invariant.
        invariant: &'static str,
        /// Items examined.
        checked: usize,
        /// Items that broke it.
        violations: usize,
        /// One example.
        example: String,
    },

    /// A typed view rejected the layout that was just solved.
    #[error("the solved layout cannot be used: {0}")]
    Engine(#[from] ufathom_engine::EngineError),
}

/// A candidate object array that a later solving step rejected.
#[derive(Debug)]
pub struct RejectedCandidate {
    /// The module the `Objects` pointer was found in.
    pub module: String,
    /// Address of the `Objects` field.
    pub slot: u64,
    /// Objects the candidate claimed to hold.
    pub objects: u32,
    /// The step that rejected it.
    pub error: Box<ScanError>,
}

fn describe(rejected: &[RejectedCandidate]) -> String {
    rejected
        .iter()
        .map(|c| {
            format!(
                "{} at {:#x} ({} objects): {}",
                c.module, c.slot, c.objects, c.error
            )
        })
        .collect::<Vec<_>>()
        .join("; ")
}

impl ScanError {
    /// Builds [`ScanError::Malformed`].
    pub(crate) fn malformed(what: &'static str, reason: impl Into<String>) -> Self {
        Self::Malformed {
            what,
            reason: reason.into(),
        }
    }

    pub(crate) fn missing(field: &'static str, detail: impl Into<String>) -> Self {
        Self::MissingEvidence {
            field,
            detail: detail.into(),
        }
    }
}
