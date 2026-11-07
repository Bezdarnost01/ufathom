//! Resolving a layout [`Location`] to an address.

use ufathom_mem::{Addr, MemoryReader};
use ufathom_model::layout::Location;

use crate::error::EngineError;

/// Turns a [`Location`] into an absolute address.
///
/// An `Rva` location is resolved against the module list of `mem`, matching
/// the module name case-insensitively and comparing file names only.
///
/// # Errors
///
/// - [`EngineError::Read`] when the module list cannot be read.
/// - [`EngineError::ModuleNotFound`] when no module has that name.
/// - [`EngineError::Inconsistent`] when `base + rva` overflows.
pub fn resolve_location(mem: &dyn MemoryReader, location: &Location) -> Result<Addr, EngineError> {
    match location {
        Location::Absolute(addr) => Ok(*addr),
        Location::Rva { module, rva } => {
            let modules = mem.modules().map_err(EngineError::read(0, "module list"))?;
            let wanted = file_name(module);
            let found = modules
                .iter()
                .find(|candidate| file_name(&candidate.name).eq_ignore_ascii_case(wanted))
                .ok_or_else(|| EngineError::ModuleNotFound {
                    module: module.clone(),
                })?;
            found
                .base
                .checked_add(*rva)
                .ok_or_else(|| EngineError::Inconsistent {
                    addr: found.base,
                    what: "module RVA",
                    reason: format!("base plus RVA {rva:#x} overflows"),
                })
        }
    }
}

fn file_name(path: &str) -> &str {
    path.rsplit(['\\', '/']).next().unwrap_or(path)
}
