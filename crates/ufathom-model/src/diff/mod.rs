//! ABI-aware comparison of two dumps, for porting between game versions.
//!
//! Rules:
//!
//! - Types and enums match by package and short name.
//! - Fields match by name. Their order is not compared.
//! - Function parameters match **by position** and are compared by layout. A
//!   renamed parameter is not an ABI change.
//! - A function missing from a class in the new dump is searched through that
//!   class's ancestors in the new dump. Found there, it is reported as moved.
//! - A name that occurs more than once on either side is reported as
//!   ambiguous and never resolved by choosing a candidate.
//! - Addresses, member order and `native_rva` produce no difference.
//!
//! The result is sorted, so the same input always gives the same report.

mod display;
mod enums;
mod functions;
mod pairing;
mod report;
mod structs;

#[cfg(test)]
mod tests;

pub use report::{
    Ambiguity, AmbiguousSubject, DumpDiff, EnumChange, EnumDiff, FunctionChange, ParamChange,
    ParamFlag, SlotChange, TypeChange, TypeDiff, TypeKey,
};

use crate::dump::Dump;

/// Compares `old` with `new` and reports what changed from old to new.
///
/// Only the type content is compared; the engine layout, tool and target
/// differ between builds by design. The dumps need not be normalized.
#[must_use]
pub fn diff(old: &Dump, new: &Dump) -> DumpDiff {
    let mut result = DumpDiff::default();
    structs::diff_structs(&old.structs, &new.structs, &mut result);
    enums::diff_enums(&old.enums, &new.enums, &mut result);
    result.ambiguities.sort();
    result.ambiguities.dedup();
    result
}
