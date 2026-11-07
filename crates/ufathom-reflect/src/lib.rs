//! Walks objects and builds a dump, reporting diagnostics instead of failing.
//!
//! [`build_dump`] turns a solved layout ([`Discovery`]) into a complete,
//! deterministic [`Dump`]. It reads only through a [`MemoryReader`], so a
//! wrong offset is a diagnostic and never a fault, and it keeps working while
//! the target changes under it:
//!
//! 1. Every object header is read once, through a [`PageCache`] made for this
//!    pass and dropped with it. A header that does not validate is read once
//!    more without the cache.
//! 2. Each object is classified by walking its class's super chain up to the
//!    CoreUObject meta classes (`Class`, `ScriptStruct`, `Enum`, `Function`),
//!    memoized per class. Only types go into the dump: class default objects,
//!    instances, properties and packages are skipped.
//! 3. Every class, struct and enum is read, then re-validated: the array slot
//!    must still point at the same address and the header must still store the
//!    same index. On a mismatch the type is read once more without the cache,
//!    and if that also fails it is skipped with a diagnostic.
//! 4. The result is normalized, so two passes over one target give byte
//!    identical JSON whatever the pool size or scheduling.
//!
//! A problem with one object never aborts the pass: it becomes a
//! [`Diagnostic`](ufathom_model::Diagnostic). When more than
//! [`ReflectOptions::degraded_threshold`] of the objects fail the output is
//! marked [`degraded`](ReflectOutput::degraded), and the caller decides what
//! to do with it.
//!
//! # Choices that the dump format leaves open
//!
//! - `StructDef::flags` is 0: the layout has no offset for `ClassFlags` or
//!   `StructFlags`.
//! - `StructDef::package` and `EnumDef::package` are the name of the outermost
//!   object on the outer chain, which is the part of the path before the
//!   first `.`.
//! - `FunctionDef::native_rva` is relative to the module that holds the
//!   object array, and only for functions flagged native whose entry point
//!   lies inside that module. A module is found by the file name of the
//!   object array's location, or by the address range for an absolute one.
//! - `EnumDef::underlying_size` is the widest of the underlying integer of any
//!   `EnumProperty` that refers to the enum and the smallest integer that
//!   holds every value (signed if any is negative, ignoring a trailing `_MAX`
//!   that is one past the range): the enum object does not store its width.
//! - A reference that does not resolve to a type in the dump (a property's
//!   class, an array's element) gives `PropertyType::Unknown`, with a
//!   warning. An unresolved parent leaves `super_path` empty, with a warning.
//! - Function parameters are put in declaration order by offset, because a
//!   member chain may run backwards.

#![forbid(unsafe_code)]

mod build;
mod classify;
mod ctx;
mod error;
mod objects;
mod options;
mod par;
mod parse;
mod types;

pub use build::build_dump;
pub use error::ReflectError;
pub use options::{
    DEFAULT_DEGRADED_THRESHOLD, PhaseTimings, ReflectOptions, ReflectOutput, ReflectStats,
};

#[cfg(doc)]
use ufathom_mem::{MemoryReader, PageCache};
#[cfg(doc)]
use ufathom_model::Dump;
#[cfg(doc)]
use ufathom_scan::Discovery;
