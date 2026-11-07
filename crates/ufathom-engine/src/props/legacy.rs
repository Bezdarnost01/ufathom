//! The `UProperty` system: properties are objects in `UStruct::Children`.

use ufathom_mem::{Addr, MemoryReader};
use ufathom_model::EngineLayout;

use super::chain::FieldIter;
use super::classes::ObjectChains;
use super::{PropertySystemReader, RawProperty, decode_body, function_walk};
use crate::error::EngineError;
use crate::header::read_header;
use crate::names::NameTable;

/// Properties as `UProperty` objects.
///
/// `UStruct::Children` mixes properties and functions. An entry is a
/// property when its class reaches a class named `Property` up the super
/// chain; the answer is memoized per class address behind a lock, so
/// concurrent walks share it.
pub struct LegacyProperties<'a> {
    mem: &'a dyn MemoryReader,
    layout: &'a EngineLayout,
    names: &'a NameTable,
    chains: ObjectChains<'a>,
}

impl<'a> LegacyProperties<'a> {
    /// A reader over `layout`, resolving names through `names`.
    #[must_use]
    pub fn new(mem: &'a dyn MemoryReader, layout: &'a EngineLayout, names: &'a NameTable) -> Self {
        Self {
            mem,
            layout,
            names,
            chains: ObjectChains::new(mem, layout, names),
        }
    }
}

impl PropertySystemReader for LegacyProperties<'_> {
    fn fields(&self, owner: Addr) -> FieldIter<'_> {
        FieldIter::walk(
            self.mem,
            "property list",
            owner,
            self.layout.ustruct.children,
            self.layout.ufield.next,
            Box::new(|object| self.chains.is_property(object)),
        )
    }

    fn functions(&self, owner: Addr) -> FieldIter<'_> {
        function_walk(self.mem, self.layout, &self.chains, owner)
    }

    fn decode(&self, field: Addr) -> Result<RawProperty, EngineError> {
        let header = read_header(self.mem, &self.layout.uobject, &self.layout.fname, field)?;
        if header.class == 0 {
            return Err(EngineError::Null {
                addr: field,
                what: "property class",
            });
        }
        let class_name = self
            .chains
            .class_name(header.class)
            .map_err(|error| error.at(field, "property class"))?;
        let name = self
            .names
            .resolve(&self.layout.fname, header.name.as_slice())
            .map_err(|error| error.at(field, "property name"))?;
        decode_body(self.mem, &self.layout.properties, field, name, class_name)
    }
}

impl std::fmt::Debug for LegacyProperties<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LegacyProperties").finish_non_exhaustive()
    }
}
