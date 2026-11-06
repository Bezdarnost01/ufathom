//! Plain data: engine layouts, the dump IR and structural diffs.

#![forbid(unsafe_code)]

pub mod dump;
mod error;
pub mod flags;
pub mod layout;

#[cfg(test)]
mod test_support;

pub use dump::{
    BoolBits, Diagnostic, Dump, EngineInfo, EngineVersion, EnumDef, EnumValue, FORMAT_VERSION,
    FieldDef, FunctionDef, PropertyType, Severity, StructDef, StructKind, TargetInfo, ToolInfo,
};
pub use error::ModelError;
pub use layout::EngineLayout;
