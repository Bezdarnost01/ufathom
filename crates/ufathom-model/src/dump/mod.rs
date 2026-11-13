//! The dump IR: a versioned, deterministic description of one game build.

mod engine;
mod types;

#[cfg(test)]
mod tests;

pub use engine::{EngineInfo, EngineVersion, RuntimeGlobals, TargetInfo, ToolInfo};
pub use types::{
    BoolBits, Diagnostic, EnumDef, EnumValue, FieldDef, FunctionDef, PropertyType, Severity,
    StructDef, StructKind,
};

use serde::{Deserialize, Serialize};

use crate::error::ModelError;
use crate::layout::EngineLayout;

/// The only `format_version` this build reads and writes.
///
/// Bump it whenever the JSON shape changes in a way an older reader would
/// misinterpret.
pub const FORMAT_VERSION: u32 = 1;

/// Everything learned about one build of a game.
///
/// A dump is deterministic: it holds no process ID, time or absolute path, and
/// [`Dump::to_json`] sorts it, so two dumps of one build are byte-identical
/// unless the set of loaded objects differs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Dump {
    /// Version of this JSON format. See [`FORMAT_VERSION`].
    pub format_version: u32,
    /// The tool that wrote the dump.
    pub tool: ToolInfo,
    /// The module that was dumped.
    pub target: TargetInfo,
    /// What is known about the engine version.
    pub engine: EngineInfo,
    /// The layout the dump was read with.
    pub layout: EngineLayout,
    /// Engine globals for in-process use. Omitted from JSON when empty.
    #[serde(default, skip_serializing_if = "RuntimeGlobals::is_empty")]
    pub globals: RuntimeGlobals,
    /// Names of all packages, sorted and without duplicates after
    /// [`Dump::normalize`].
    pub packages: Vec<String>,
    /// Classes and script structs.
    pub structs: Vec<StructDef>,
    /// Enumerations.
    pub enums: Vec<EnumDef>,
    /// Problems met while building the dump.
    pub diagnostics: Vec<Diagnostic>,
}

impl Dump {
    /// Creates an empty dump stamped with the current [`FORMAT_VERSION`].
    #[must_use]
    pub fn new(
        tool: ToolInfo,
        target: TargetInfo,
        engine: EngineInfo,
        layout: EngineLayout,
    ) -> Self {
        Self {
            format_version: FORMAT_VERSION,
            tool,
            target,
            engine,
            layout,
            globals: RuntimeGlobals::default(),
            packages: Vec::new(),
            structs: Vec::new(),
            enums: Vec::new(),
            diagnostics: Vec::new(),
        }
    }

    /// Puts the dump in canonical order. Idempotent.
    ///
    /// - Packages, structs, enums and diagnostics are sorted by path (then by
    ///   their remaining content, so even duplicates have one fixed order).
    ///   Packages are also deduplicated.
    /// - Fields are sorted by `(offset, name)`. Their order in memory is an
    ///   accident of how the engine walks a linked list, and sorting makes
    ///   dumps comparable and the generated layout easy to follow.
    /// - Enumerators are sorted by `(value, name)`.
    /// - Function parameters keep their declared order. The order is the
    ///   calling convention: two parameters swapped are two different
    ///   functions, so sorting would erase real information. Functions
    ///   themselves are sorted by path.
    /// - [`EngineInfo::evidence`] keeps the order the solver reported it.
    pub fn normalize(&mut self) {
        self.packages.sort();
        self.packages.dedup();

        for def in &mut self.structs {
            def.fields.sort_by(FieldDef::output_order);
            def.functions.sort();
        }
        self.structs.sort();

        for def in &mut self.enums {
            def.values.sort_by(EnumValue::output_order);
        }
        self.enums.sort();

        self.diagnostics.sort();
    }

    /// Serializes the dump as pretty-printed JSON in canonical order.
    ///
    /// The dump itself is not modified: a sorted clone is written.
    ///
    /// # Errors
    ///
    /// Returns [`ModelError::Serialize`] if JSON encoding fails.
    pub fn to_json(&self) -> Result<String, ModelError> {
        let mut sorted = self.clone();
        sorted.normalize();
        serde_json::to_string_pretty(&sorted).map_err(ModelError::Serialize)
    }

    /// Parses a dump from JSON.
    ///
    /// The `format_version` is checked before the rest of the document is
    /// interpreted, so a dump from another format version gets a clear error
    /// rather than a confusing complaint about a missing field, or worse, a
    /// silently wrong reading. The result is not normalized.
    ///
    /// # Errors
    ///
    /// - [`ModelError::Parse`] if the text is not valid JSON, has no numeric
    ///   `format_version`, or does not match the dump shape.
    /// - [`ModelError::UnsupportedFormatVersion`] if `format_version` is not
    ///   [`FORMAT_VERSION`].
    pub fn from_json(text: &str) -> Result<Self, ModelError> {
        #[derive(Deserialize)]
        struct VersionProbe {
            format_version: u64,
        }

        let probe: VersionProbe = serde_json::from_str(text).map_err(ModelError::Parse)?;
        if probe.format_version != u64::from(FORMAT_VERSION) {
            return Err(ModelError::UnsupportedFormatVersion {
                found: probe.format_version,
                supported: FORMAT_VERSION,
            });
        }
        serde_json::from_str(text).map_err(ModelError::Parse)
    }
}
