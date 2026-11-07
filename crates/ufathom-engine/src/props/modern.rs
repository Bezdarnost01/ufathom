//! The `FProperty` system: properties are fields in `UStruct::ChildProperties`.

use std::collections::HashMap;
use std::sync::Mutex;

use ufathom_mem::{Addr, MemoryReader};
use ufathom_model::EngineLayout;
use ufathom_model::layout::PropertySystem;

use super::chain::FieldIter;
use super::classes::{ObjectChains, lock};
use super::{PropertySystemReader, RawProperty, decode_body, function_walk};
use crate::error::EngineError;
use crate::header::FNameBytes;
use crate::limits::MAX_FNAME_SIZE;
use crate::names::NameTable;
use crate::window::Window;

/// Properties as `FField` chains.
///
/// The class name of a field comes from its `FFieldClass`, at the offset the
/// layout gives (it moved in UE 5.8). Functions stay in `UStruct::Children`.
pub struct ModernProperties<'a> {
    mem: &'a dyn MemoryReader,
    layout: &'a EngineLayout,
    names: &'a NameTable,
    chains: ObjectChains<'a>,
    class_names: Mutex<HashMap<Addr, String>>,
}

impl<'a> ModernProperties<'a> {
    /// A reader over `layout`, resolving names through `names`.
    ///
    /// If `layout` does not describe the modern system, every call returns
    /// [`EngineError::InvalidLayout`].
    #[must_use]
    pub fn new(mem: &'a dyn MemoryReader, layout: &'a EngineLayout, names: &'a NameTable) -> Self {
        Self {
            mem,
            layout,
            names,
            chains: ObjectChains::new(mem, layout, names),
            class_names: Mutex::new(HashMap::new()),
        }
    }

    fn not_modern() -> EngineError {
        EngineError::InvalidLayout {
            what: "property system",
            reason: "the layout does not describe FField properties".to_owned(),
        }
    }

    fn field_class_name(&self, class: Addr, name_offset: u32) -> Result<String, EngineError> {
        if let Some(name) = lock(&self.class_names).get(&class) {
            return Ok(name.clone());
        }
        let size = self.layout.fname.size;
        if size == 0 || size as usize > MAX_FNAME_SIZE {
            return Err(EngineError::InvalidLayout {
                what: "FName",
                reason: format!("size {size}"),
            });
        }
        let window = Window::read(self.mem, class, "field class", &[(name_offset, size)])?;
        let bytes = FNameBytes::from_slice(window.bytes(name_offset, size)?)?;
        let name = self
            .names
            .resolve(&self.layout.fname, bytes.as_slice())
            .map_err(|error| error.at(class, "field class name"))?;
        lock(&self.class_names).insert(class, name.clone());
        Ok(name)
    }
}

impl PropertySystemReader for ModernProperties<'_> {
    fn fields(&self, owner: Addr) -> FieldIter<'_> {
        let PropertySystem::Modern { ffield, .. } = &self.layout.properties.system else {
            return FieldIter::failed(self.mem, "property list", Self::not_modern());
        };
        let Some(head) = self.layout.ustruct.child_properties else {
            return FieldIter::failed(
                self.mem,
                "property list",
                EngineError::InvalidLayout {
                    what: "UStruct",
                    reason: "a modern layout needs ChildProperties".to_owned(),
                },
            );
        };
        FieldIter::walk(
            self.mem,
            "property list",
            owner,
            head,
            ffield.next,
            Box::new(|_| Ok(true)),
        )
    }

    fn functions(&self, owner: Addr) -> FieldIter<'_> {
        function_walk(self.mem, self.layout, &self.chains, owner)
    }

    fn decode(&self, field: Addr) -> Result<RawProperty, EngineError> {
        let PropertySystem::Modern {
            ffield,
            field_class,
        } = &self.layout.properties.system
        else {
            return Err(Self::not_modern());
        };
        let name_size = self.layout.fname.size;
        if name_size == 0 || name_size as usize > MAX_FNAME_SIZE {
            return Err(EngineError::InvalidLayout {
                what: "FName",
                reason: format!("size {name_size}"),
            });
        }
        let window = Window::read(
            self.mem,
            field,
            "field",
            &[(ffield.class, 8), (ffield.name, name_size)],
        )?;
        let class = window.u64(ffield.class)?;
        if class == 0 {
            return Err(EngineError::Null {
                addr: field,
                what: "field class",
            });
        }
        let class_name = self
            .field_class_name(class, field_class.name)
            .map_err(|error| error.at(field, "field class"))?;
        let name_bytes = FNameBytes::from_slice(window.bytes(ffield.name, name_size)?)?;
        let name = self
            .names
            .resolve(&self.layout.fname, name_bytes.as_slice())
            .map_err(|error| error.at(field, "field name"))?;
        decode_body(self.mem, &self.layout.properties, field, name, class_name)
    }
}

impl std::fmt::Debug for ModernProperties<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ModernProperties").finish_non_exhaustive()
    }
}
