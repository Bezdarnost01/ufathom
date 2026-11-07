//! What discovery returns.

use ufathom_model::{EngineInfo, EngineLayout};

/// How one solved value was established.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Evidence {
    /// Dotted field name, for example `uobject.class`.
    pub field: String,
    /// The solved value: an offset, a size or a count.
    pub value: u64,
    /// The invariant that fixed it.
    pub method: String,
    /// Items examined.
    pub samples: u32,
    /// Items that agreed with the value.
    pub matched: u32,
}

/// Addresses and indices of the engine globals.
///
/// Slots are RVAs inside [`module`](Self::module). When the evidence leaves
/// several candidates, all are listed and none is chosen.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Globals {
    /// The module the RVAs are relative to.
    pub module: String,
    /// Slots holding a pointer to the live `World`, sorted.
    pub gworld: Vec<u64>,
    /// Slots holding a pointer to the live `GameEngine`, sorted.
    pub gengine: Vec<u64>,
    /// Virtual table index of `UObject::ProcessEvent`.
    pub process_event_index: Option<u32>,
    /// RVA of `UObject::ProcessEvent`.
    pub process_event_rva: Option<u64>,
}

/// The result of [`discover`](crate::discover).
#[derive(Debug, Clone)]
pub struct Discovery {
    /// Every offset needed to read the target.
    pub layout: EngineLayout,
    /// Structural facts about the engine version. String facts (version
    /// resource, build string) are merged in by the version module.
    pub engine: EngineInfo,
    /// Engine globals.
    pub globals: Globals,
    /// How each value was solved.
    pub evidence: Vec<Evidence>,
    /// Things that were found but are weaker than they should be.
    pub warnings: Vec<String>,
}

impl Discovery {
    /// The evidence record of a field, by dotted name.
    #[must_use]
    pub fn evidence_for(&self, field: &str) -> Option<&Evidence> {
        self.evidence.iter().find(|record| record.field == field)
    }
}
