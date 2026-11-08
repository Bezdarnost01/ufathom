//! What a generation run did, and what it could not represent exactly.

use std::collections::BTreeMap;
use std::fmt;

/// Why something was emitted as raw bytes, left out, or skipped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
pub enum DegradeReason {
    /// The field starts before the end of an earlier field. It has no storage
    /// of its own and is reachable only through its `FieldRef` constant.
    Overlap,
    /// The field does not fit inside the size of its owner. It is left out.
    OutOfBounds,
    /// The offset is not a multiple of the alignment of the Rust type.
    Misaligned,
    /// The Rust type needs more alignment than the owner is declared to have.
    AlignmentTooLow,
    /// The type of the field is not in the dump, or its layout is cyclic.
    UnknownType,
    /// The dump decodes the type but the runtime has no view of it.
    NoRuntimeView,
    /// The size the dump reports differs from the size of the Rust type.
    SizeMismatch,
    /// A bit-field `bool` that cannot be addressed inside its storage.
    InvalidBitfield,
    /// The parent class could not be embedded by value, so its bytes are
    /// reached through a pointer-cast `Deref` or not at all.
    BaseNotEmbedded,
    /// An enumerator whose value does not fit the underlying integer. It is
    /// left out of the constants.
    EnumeratorOutOfRange,
    /// A function whose parameter block could not be represented. It has no
    /// wrapper.
    FunctionSkipped,
}

impl DegradeReason {
    /// A stable short name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Overlap => "overlap",
            Self::OutOfBounds => "out-of-bounds",
            Self::Misaligned => "misaligned",
            Self::AlignmentTooLow => "alignment-too-low",
            Self::UnknownType => "unknown-type",
            Self::NoRuntimeView => "no-runtime-view",
            Self::SizeMismatch => "size-mismatch",
            Self::InvalidBitfield => "invalid-bitfield",
            Self::BaseNotEmbedded => "base-not-embedded",
            Self::EnumeratorOutOfRange => "enumerator-out-of-range",
            Self::FunctionSkipped => "function-skipped",
        }
    }
}

impl fmt::Display for DegradeReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One thing that could not be represented exactly.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Degradation {
    /// Path of the type or function that owns it.
    pub owner: String,
    /// Name of the field, parameter, enumerator or `<base>`.
    pub member: String,
    /// Why.
    pub reason: DegradeReason,
    /// A human-readable explanation with the numbers involved.
    pub detail: String,
}

/// A type whose alignment is higher than the dump declares.
///
/// This is information, not a degradation: some engine builds record an
/// alignment of 1 for most native classes, while their members and parent
/// need more. The generated type uses the larger alignment, which the native
/// compiler must have used too. The size and the field offsets are unchanged
/// and are still asserted at compile time.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct AlignmentRaise {
    /// Path of the class or script struct.
    pub owner: String,
    /// The alignment in the dump.
    pub declared: u32,
    /// The alignment of the generated type.
    pub used: u32,
}

/// A class whose generated type is larger than its size in the dump.
///
/// This is information, not a degradation: the engine computes the size of a
/// generated (blueprint) class without rounding it to the class alignment,
/// but every object is a separate heap allocation made with that alignment,
/// so it occupies the rounded size. Script structs are never padded, because
/// they are embedded in arrays and other structs.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct SizePadding {
    /// Path of the class.
    pub owner: String,
    /// The size in the dump.
    pub dump_size: u32,
    /// The size of the generated type, a multiple of its alignment.
    pub rust_size: u32,
}

/// Counts and diagnostics of one [`generate`](crate::generate) call.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GenReport {
    /// Number of packages (modules).
    pub packages: usize,
    /// Number of classes generated.
    pub classes: usize,
    /// Number of script structs generated.
    pub script_structs: usize,
    /// Number of enums generated.
    pub enums: usize,
    /// Number of data fields in the dump, including degraded ones.
    pub fields: usize,
    /// Number of functions with a generated wrapper.
    pub functions: usize,
    /// Number of functions without a wrapper.
    pub functions_skipped: usize,
    /// Everything that was not represented exactly, sorted.
    pub degraded: Vec<Degradation>,
    /// Types generated with a higher alignment than the dump declares, sorted
    /// by path. Not a degradation.
    pub alignments_raised: Vec<AlignmentRaise>,
    /// Classes generated larger than the dump says, sorted by path. Not a
    /// degradation.
    pub sizes_padded: Vec<SizePadding>,
    /// Files written because they were new or their hash changed, sorted.
    pub files_written: Vec<String>,
    /// Files left untouched because their hash did not change, sorted.
    pub files_unchanged: Vec<String>,
    /// Files of an earlier run that are no longer generated and were removed.
    pub files_removed: Vec<String>,
}

impl GenReport {
    /// Classes, script structs and enums.
    #[must_use]
    pub fn types(&self) -> usize {
        self.classes + self.script_structs + self.enums
    }

    /// Number of fields that were degraded or left out, not counting
    /// functions and enumerators.
    #[must_use]
    pub fn degraded_fields(&self) -> usize {
        self.degraded
            .iter()
            .filter(|d| {
                !matches!(
                    d.reason,
                    DegradeReason::EnumeratorOutOfRange | DegradeReason::FunctionSkipped
                )
            })
            .count()
    }

    /// How many degradations there are for each reason.
    #[must_use]
    pub fn by_reason(&self) -> BTreeMap<DegradeReason, usize> {
        let mut counts = BTreeMap::new();
        for d in &self.degraded {
            *counts.entry(d.reason).or_insert(0) += 1;
        }
        counts
    }
}
