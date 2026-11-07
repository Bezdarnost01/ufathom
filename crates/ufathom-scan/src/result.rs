//! What discovery returns.

use ufathom_model::layout::Location;
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
/// [`gworld`](Self::gworld) and [`gengine`](Self::gengine) are RVAs inside
/// [`module`](Self::module), the module that holds the object array. In a
/// modular build the globals may sit in other modules: the `*_slots` lists
/// name every slot found, each with its own module. When the evidence leaves
/// several candidates, all are listed and none is chosen.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Globals {
    /// The module the RVAs are relative to.
    pub module: String,
    /// Slots holding a pointer to the live `World`, sorted.
    pub gworld: Vec<u64>,
    /// Slots holding a pointer to the live `GameEngine`, sorted.
    pub gengine: Vec<u64>,
    /// Every slot, in any module, holding a pointer to a live `World`.
    pub gworld_slots: Vec<Location>,
    /// Every slot, in any module, holding a pointer to a live `GameEngine`.
    pub gengine_slots: Vec<Location>,
    /// Virtual table index of `UObject::ProcessEvent`.
    pub process_event_index: Option<u32>,
    /// RVA of `UObject::ProcessEvent` inside
    /// [`process_event_module`](Self::process_event_module).
    pub process_event_rva: Option<u64>,
    /// The module that holds `UObject::ProcessEvent`.
    pub process_event_module: Option<String>,
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
