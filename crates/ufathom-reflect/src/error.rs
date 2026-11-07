//! The error type of this crate.

use thiserror::Error;
use ufathom_engine::EngineError;

/// Why no dump could be built at all.
///
/// A problem with one object is never an error: it becomes a
/// [`Diagnostic`](ufathom_model::Diagnostic) in the dump. These variants are
/// the cases where nothing useful can be produced.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum ReflectError {
    /// The caller set the cancel flag. Whatever was read is discarded.
    #[error("the dump was cancelled")]
    Cancelled,

    /// The name table could not be loaded, so no object can be named.
    #[error("cannot load the name table: {source}")]
    Names {
        /// The underlying error.
        #[source]
        source: EngineError,
    },

    /// The global object array header is unreadable or inconsistent: a count
    /// above its capacity, a chunk count above the chunk table, and so on.
    #[error("cannot read the object array: {source}")]
    ObjectArray {
        /// The underlying error.
        #[source]
        source: EngineError,
    },

    /// The object array holds no object, so there is nothing to dump.
    #[error("the object array holds no object")]
    NoObjects,

    /// One of the CoreUObject classes every type is classified against is not
    /// among the objects, so no object can be classified.
    #[error("the core class /Script/CoreUObject.{name} is not among the objects")]
    MissingCoreClass {
        /// The class that was looked for.
        name: &'static str,
    },
}
