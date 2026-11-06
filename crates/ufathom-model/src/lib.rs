//! Plain data: engine layouts, the dump IR and structural diffs.
//!
//! This crate performs no I/O and never touches target memory. It holds:
//!
//! - [`layout`]: [`EngineLayout`], every offset needed to read one build.
//! - [`dump`]: [`Dump`], the versioned, deterministic IR written to JSON.
//! - [`flags`]: named bits of the engine's flag enums.
//! - [`diff`](mod@diff): an ABI-aware comparison of two dumps.

#![forbid(unsafe_code)]

pub mod diff;
pub mod dump;
mod error;
pub mod flags;
pub mod layout;

#[cfg(test)]
mod test_support;

pub use diff::{DumpDiff, diff};
pub use dump::{
    BoolBits, Diagnostic, Dump, EngineInfo, EngineVersion, EnumDef, EnumValue, FORMAT_VERSION,
    FieldDef, FunctionDef, PropertyType, Severity, StructDef, StructKind, TargetInfo, ToolInfo,
};
pub use error::ModelError;
pub use layout::EngineLayout;
