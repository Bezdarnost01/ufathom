//! Who wrote a dump, what it was taken from, and which engine that is.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::layout::Location;

/// The tool that produced a dump.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolInfo {
    /// Tool name.
    pub name: String,
    /// Tool version.
    pub version: String,
}

/// The module a dump was taken from.
///
/// Deliberately holds nothing about the run: no process ID, no time and no
/// absolute path, because a dump must be deterministic and free of machine
/// details.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TargetInfo {
    /// File name of the game module, for example `Game-Win64-Shipping.exe`.
    pub module: String,
    /// Identifier of the exact build, such as a hash of the module. Stable
    /// for one binary and different for another.
    pub build_id: String,
}

/// An engine version, ordered by `major`, `minor`, `patch`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct EngineVersion {
    /// Major version, 4 or 5.
    pub major: u16,
    /// Minor version.
    pub minor: u16,
    /// Patch version.
    pub patch: u16,
}

impl fmt::Display for EngineVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// What is known about the engine version. Every part may be absent: an
/// unknown version is legal and never an error.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EngineInfo {
    /// The numeric version the binary claims, for example `4.25.1`. The raw text
    /// it came from is kept in [`evidence`](Self::evidence).
    pub claimed: Option<String>,
    /// The build string the binary carries, for example a branch and
    /// changelist.
    pub build: Option<String>,
    /// Lowest version consistent with the structural evidence.
    pub min_version: Option<EngineVersion>,
    /// Highest version consistent with the structural evidence.
    pub max_version: Option<EngineVersion>,
    /// Human-readable facts behind the fields above.
    pub evidence: Vec<String>,
}

/// Engine globals a generated SDK needs at run time, inside the game.
///
/// Every part is optional: discovery leaves a value out when it is not found
/// or when several candidates remain. An empty value is not written to JSON,
/// so a dump taken without globals reads and writes as before.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeGlobals {
    /// Virtual table index of `UObject::ProcessEvent`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub process_event_index: Option<u32>,
    /// The slot holding the pointer to the live `UWorld` (`GWorld`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gworld: Option<Location>,
    /// The slot holding the pointer to the live `UEngine` (`GEngine`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gengine: Option<Location>,
}

impl RuntimeGlobals {
    /// `true` when nothing is known.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.process_event_index.is_none() && self.gworld.is_none() && self.gengine.is_none()
    }
}
