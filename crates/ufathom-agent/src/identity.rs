//! `identity.json`: the record that ties a dump to a build and a run.
//!
//! `dump.json` is deterministic and holds no machine detail. This file is the
//! opposite: it names the executable on disk, the process, the settings and
//! the time, so a dump can be traced to the exact binary that produced it.

use serde::Serialize;
use ufathom_model::layout::Location;
use ufathom_model::{EngineInfo, EngineLayout};
use ufathom_reflect::ReflectStats;
use ufathom_scan::{Discovery, Evidence, Globals};

use crate::config::{Config, ConfigSource};

/// The version of the `identity.json` shape.
pub const IDENTITY_FORMAT_VERSION: u32 = 1;

/// A file on disk and what is known of it.
#[derive(Debug, Clone, Serialize)]
pub struct FileIdentity {
    /// Absolute path as the process knows it.
    pub path: String,
    /// Lower-case hex SHA-256, or `None` when the file could not be read.
    pub sha256: Option<String>,
    /// Why the hash is missing.
    pub hash_error: Option<String>,
}

/// One solved value and how it was established.
#[derive(Debug, Clone, Serialize)]
pub struct EvidenceRecord {
    /// Dotted field name.
    pub field: String,
    /// The solved value.
    pub value: u64,
    /// The invariant that fixed it.
    pub method: String,
    /// Items examined.
    pub samples: u32,
    /// Items that agreed.
    pub matched: u32,
}

impl From<&Evidence> for EvidenceRecord {
    fn from(evidence: &Evidence) -> Self {
        Self {
            field: evidence.field.clone(),
            value: evidence.value,
            method: evidence.method.clone(),
            samples: evidence.samples,
            matched: evidence.matched,
        }
    }
}

/// Engine globals that were found.
#[derive(Debug, Clone, Serialize)]
pub struct GlobalsRecord {
    /// The module the RVAs are relative to.
    pub module: String,
    /// Slots holding the live `World`, as RVAs.
    pub gworld: Vec<u64>,
    /// Slots holding the live `GameEngine`, as RVAs.
    pub gengine: Vec<u64>,
    /// Every `World` slot in any module.
    pub gworld_slots: Vec<Location>,
    /// Every `GameEngine` slot in any module.
    pub gengine_slots: Vec<Location>,
    /// Virtual table index of `ProcessEvent`.
    pub process_event_index: Option<u32>,
    /// RVA of `ProcessEvent`.
    pub process_event_rva: Option<u64>,
    /// Module that holds `ProcessEvent`.
    pub process_event_module: Option<String>,
}

impl From<&Globals> for GlobalsRecord {
    fn from(globals: &Globals) -> Self {
        Self {
            module: globals.module.clone(),
            gworld: globals.gworld.clone(),
            gengine: globals.gengine.clone(),
            gworld_slots: globals.gworld_slots.clone(),
            gengine_slots: globals.gengine_slots.clone(),
            process_event_index: globals.process_event_index,
            process_event_rva: globals.process_event_rva,
            process_event_module: globals.process_event_module.clone(),
        }
    }
}

/// What discovery found.
#[derive(Debug, Clone, Serialize)]
pub struct DiscoveryRecord {
    /// Every offset needed to read the target.
    pub layout: EngineLayout,
    /// Engine globals.
    pub globals: GlobalsRecord,
    /// How each value was solved.
    pub evidence: Vec<EvidenceRecord>,
}

impl DiscoveryRecord {
    /// Records `discovery`.
    #[must_use]
    pub fn new(discovery: &Discovery) -> Self {
        Self {
            layout: discovery.layout.clone(),
            globals: GlobalsRecord::from(&discovery.globals),
            evidence: discovery
                .evidence
                .iter()
                .map(EvidenceRecord::from)
                .collect(),
        }
    }
}

/// Counts from the reflection pass.
#[derive(Debug, Clone, Serialize)]
pub struct ReflectRecord {
    /// Occupied slots of the object array.
    pub objects: u64,
    /// Free slots.
    pub null_slots: u64,
    /// Script structs.
    pub structs: u64,
    /// Classes.
    pub classes: u64,
    /// Enums.
    pub enums: u64,
    /// Functions.
    pub functions: u64,
    /// Objects that could not be dumped completely.
    pub failures: u64,
    /// Objects that were read a second time.
    pub rereads: u64,
    /// Diagnostics recorded in the dump.
    pub diagnostics: u64,
    /// More than the allowed share of objects failed.
    pub degraded: bool,
}

impl ReflectRecord {
    /// Records the stats of one pass.
    #[must_use]
    pub fn new(stats: &ReflectStats, diagnostics: usize, degraded: bool) -> Self {
        Self {
            objects: stats.objects,
            null_slots: stats.null_slots,
            structs: stats.structs,
            classes: stats.classes,
            enums: stats.enums,
            functions: stats.functions,
            failures: stats.failures,
            rereads: stats.rereads,
            diagnostics: u64::try_from(diagnostics).unwrap_or(u64::MAX),
            degraded,
        }
    }
}

/// Wall time of each step of a dump, in milliseconds.
#[derive(Debug, Clone, Default, Serialize)]
pub struct TimingsMs {
    /// Hashing the executable and the DLL (zero when cached).
    pub hash: u64,
    /// Solving the layout.
    pub discover: u64,
    /// The whole reflection pass.
    pub reflect: u64,
    /// Writing the dump files.
    pub write: u64,
    /// The whole dump up to the moment this record was written.
    pub total: u64,
}

/// The document written to `identity.json`.
#[derive(Debug, Clone, Serialize)]
pub struct Identity {
    /// Version of this shape.
    pub format_version: u32,
    /// Tool name.
    pub tool: String,
    /// Tool version.
    pub tool_version: String,
    /// When the dump was captured, UTC.
    pub captured_at: String,
    /// Name of the session folder.
    pub session: String,
    /// Label of the dump folder.
    pub label: String,
    /// `ok`, `degraded` or `failed`.
    pub result: String,
    /// Why the dump failed, for a failed record.
    pub error: Option<String>,
    /// Process id of the host.
    pub pid: u32,
    /// The game executable.
    pub game: FileIdentity,
    /// This DLL.
    pub dll: FileIdentity,
    /// What is known about the engine version.
    pub engine: Option<EngineInfo>,
    /// Where the settings came from.
    pub config_source: String,
    /// The settings in effect.
    pub config: Config,
    /// Offsets and evidence. Absent when discovery failed.
    pub discovery: Option<DiscoveryRecord>,
    /// Things discovery found but trusts less than it should.
    pub warnings: Vec<String>,
    /// Counts from reflection. Absent when it did not run to the end.
    pub reflect: Option<ReflectRecord>,
    /// Files written next to this one.
    pub files: Vec<String>,
    /// Time per step.
    pub timings_ms: TimingsMs,
}

/// A readable name for where the config came from.
#[must_use]
pub fn describe_source(source: &ConfigSource) -> String {
    match source {
        ConfigSource::File(path) => path.display().to_string(),
        ConfigSource::Defaults => "defaults".to_owned(),
    }
}
