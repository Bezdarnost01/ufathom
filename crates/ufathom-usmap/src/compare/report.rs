//! The result of comparing a dump with a usmap.

use std::fmt;

use serde::Serialize;

/// Whether an item concerns structs and classes or enums.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub enum TypeKind {
    /// A struct or class.
    Struct,
    /// An enumeration.
    Enum,
}

/// Which input a name was found twice in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub enum Side {
    /// The dump.
    Dump,
    /// The usmap.
    Usmap,
}

/// A short name that more than one type carries.
///
/// usmap names types without a package, so the match cannot be decided and
/// the type is not compared. Neither candidate is picked.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct AmbiguousName {
    /// Struct or enum.
    pub kind: TypeKind,
    /// The input that holds the duplicate.
    pub side: Side,
    /// The name, folded to lower case, since matching ignores case.
    pub name: String,
    /// Dump object paths, or the usmap names as written, of every holder.
    pub candidates: Vec<String>,
    /// For a duplicate in the dump: whether the usmap has a type of this
    /// name, which is then left unchecked. Always `true` for a duplicate in
    /// the usmap.
    pub in_other_input: bool,
}

/// The parent of a matched struct differs.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct ParentDifference {
    /// Short name of the dump's parent, `None` for a root.
    pub dump: Option<String>,
    /// Name of the usmap's parent, `None` for a root.
    pub usmap: Option<String>,
}

/// A property whose type differs.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct TypeMismatch {
    /// Property name as the usmap writes it.
    pub property: String,
    /// The dump's type as the dump describes it.
    pub dump: String,
    /// The dump's type after mapping to usmap, which is what was compared.
    pub dump_as_usmap: String,
    /// The usmap's type.
    pub usmap: String,
}

/// A property whose array dimension differs.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct ArrayDimMismatch {
    /// Property name as the usmap writes it.
    pub property: String,
    /// The dump's dimension.
    pub dump: u32,
    /// The usmap's dimension. A usmap cannot store more than 255, so a dump
    /// dimension above that matches a stored 255.
    pub usmap: u8,
}

/// A matched struct that differs from the usmap.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct StructDifference {
    /// The usmap's name for the struct.
    pub name: String,
    /// Object path of the dump's struct.
    pub dump_path: String,
    /// Set when the parents differ.
    pub parent: Option<ParentDifference>,
    /// Usmap properties the dump's struct does not have, by name.
    pub missing_properties: Vec<String>,
    /// Properties present in both with different types.
    pub type_mismatches: Vec<TypeMismatch>,
    /// Properties present in both with different array dimensions.
    pub array_dim_mismatches: Vec<ArrayDimMismatch>,
}

/// An enumerator the usmap has and the dump does not.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct MissingEnumValue {
    /// Enumerator name.
    pub name: String,
    /// The usmap's number, or `None` when the usmap version does not store
    /// numbers.
    pub value: Option<i64>,
}

/// An enumerator present in both with a different number.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct EnumNumberMismatch {
    /// Enumerator name as the usmap writes it.
    pub name: String,
    /// The dump's number. With several enumerators of this name, the lowest.
    pub dump: i64,
    /// The usmap's number.
    pub usmap: i64,
}

/// A matched enum that differs from the usmap.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct EnumDifference {
    /// The usmap's name for the enum.
    pub name: String,
    /// Object path of the dump's enum.
    pub dump_path: String,
    /// Enumerators the dump lacks.
    pub missing_values: Vec<MissingEnumValue>,
    /// Enumerators with a different number. Empty for a usmap version that
    /// does not store numbers.
    pub number_mismatches: Vec<EnumNumberMismatch>,
}

/// Where a case-only difference was seen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub enum CaseOnlyKind {
    /// Name of a struct.
    StructName,
    /// Name of an enum.
    EnumName,
    /// Name of a parent type.
    ParentName,
    /// Name of a property.
    PropertyName,
    /// Name of a struct or enum a property refers to.
    TypeReference,
    /// Name of an enumerator.
    EnumValueName,
}

/// Two names that match when case is ignored and differ when it is not.
///
/// Builds differ in case, so this is not a mismatch, but it is counted
/// separately and never hidden.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct CaseOnlyDifference {
    /// What kind of name.
    pub kind: CaseOnlyKind,
    /// The struct or enum the name was found in, as the usmap writes it.
    pub owner: String,
    /// The name in the dump.
    pub dump: String,
    /// The name in the usmap.
    pub usmap: String,
}

/// Totals over a [`OracleReport`]. Every number is derivable from the lists
/// next to it, except the first group, which counts inputs.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct OracleCounts {
    /// Structs and classes in the usmap.
    pub usmap_structs: usize,
    /// Enums in the usmap.
    pub usmap_enums: usize,
    /// Structs and classes in the dump.
    pub dump_structs: usize,
    /// Enums in the dump.
    pub dump_enums: usize,
    /// Structs found in both, uniquely, and compared.
    pub matched_structs: usize,
    /// Enums found in both, uniquely, and compared.
    pub matched_enums: usize,
    /// Usmap properties looked up in matched structs.
    pub properties_checked: usize,
    /// Usmap structs absent from the dump.
    pub missing_structs: usize,
    /// Dump structs absent from the usmap. Information only.
    pub extra_structs: usize,
    /// Usmap enums absent from the dump.
    pub missing_enums: usize,
    /// Dump enums absent from the usmap. Information only.
    pub extra_enums: usize,
    /// Names that are ambiguous, in either input.
    pub ambiguous_names: usize,
    /// Matched structs with at least one difference.
    pub structs_with_differences: usize,
    /// Matched structs whose parent differs.
    pub parent_differences: usize,
    /// Usmap properties absent from the dump.
    pub missing_properties: usize,
    /// Properties with different types.
    pub type_mismatches: usize,
    /// Properties with different array dimensions.
    pub array_dim_mismatches: usize,
    /// Matched enums with at least one difference.
    pub enums_with_differences: usize,
    /// Enumerators absent from the dump.
    pub missing_enum_values: usize,
    /// Enumerators with a different number.
    pub enum_number_mismatches: usize,
    /// Names that differ only in case. Not counted as differences.
    pub case_only_differences: usize,
}

/// The outcome of [`compare`](crate::compare).
///
/// Every list is sorted, so two runs over the same input give identical
/// reports, including their JSON.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct OracleReport {
    /// Totals.
    pub counts: OracleCounts,
    /// Usmap structs and classes the dump lacks, by usmap name.
    pub missing_structs: Vec<String>,
    /// Dump structs and classes the usmap lacks, by object path. Information
    /// only: the two inputs may come from different moments, with different
    /// objects loaded.
    pub extra_structs: Vec<String>,
    /// Usmap enums the dump lacks, by usmap name.
    pub missing_enums: Vec<String>,
    /// Dump enums the usmap lacks, by object path. Information only.
    pub extra_enums: Vec<String>,
    /// Names that could not be matched uniquely.
    pub ambiguous: Vec<AmbiguousName>,
    /// Matched structs that differ.
    pub struct_differences: Vec<StructDifference>,
    /// Matched enums that differ.
    pub enum_differences: Vec<EnumDifference>,
    /// Names that differ only in case.
    pub case_only: Vec<CaseOnlyDifference>,
}

impl OracleReport {
    /// Whether the dump satisfies the usmap: nothing the usmap lists is
    /// missing, mismatched or ambiguous. Extra dump content and case-only
    /// differences do not count against it.
    #[must_use]
    pub fn is_clean(&self) -> bool {
        self.missing_structs.is_empty()
            && self.missing_enums.is_empty()
            && self.ambiguous.is_empty()
            && self.struct_differences.is_empty()
            && self.enum_differences.is_empty()
    }
}

/// How many example items the summary prints per category.
const SUMMARY_EXAMPLES: usize = 5;

fn write_examples(
    f: &mut fmt::Formatter<'_>,
    title: &str,
    items: impl ExactSizeIterator<Item = String>,
) -> fmt::Result {
    let total = items.len();
    if total == 0 {
        return Ok(());
    }
    writeln!(f, "{title} ({total}):")?;
    for item in items.take(SUMMARY_EXAMPLES) {
        writeln!(f, "  {item}")?;
    }
    if total > SUMMARY_EXAMPLES {
        writeln!(f, "  ... and {} more", total - SUMMARY_EXAMPLES)?;
    }
    Ok(())
}

impl fmt::Display for OracleReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let c = &self.counts;
        writeln!(
            f,
            "usmap: {} structs, {} enums; dump: {} structs, {} enums",
            c.usmap_structs, c.usmap_enums, c.dump_structs, c.dump_enums
        )?;
        writeln!(
            f,
            "matched: {} structs ({} properties checked), {} enums",
            c.matched_structs, c.properties_checked, c.matched_enums
        )?;
        writeln!(
            f,
            "missing from the dump: {} structs, {} enums, {} properties, {} enumerators",
            c.missing_structs, c.missing_enums, c.missing_properties, c.missing_enum_values
        )?;
        writeln!(
            f,
            "mismatches: {} types, {} array dimensions, {} parents, {} enum numbers",
            c.type_mismatches,
            c.array_dim_mismatches,
            c.parent_differences,
            c.enum_number_mismatches
        )?;
        writeln!(f, "ambiguous names: {}", c.ambiguous_names)?;
        writeln!(f, "case-only differences: {}", c.case_only_differences)?;
        writeln!(
            f,
            "in the dump only (information): {} structs, {} enums",
            c.extra_structs, c.extra_enums
        )?;

        write_examples(f, "missing structs", self.missing_structs.iter().cloned())?;
        write_examples(f, "missing enums", self.missing_enums.iter().cloned())?;
        write_examples(
            f,
            "ambiguous",
            self.ambiguous
                .iter()
                .map(|item| format!("{:?} {:?} {}", item.kind, item.side, item.name)),
        )?;
        write_examples(
            f,
            "struct differences",
            self.struct_differences.iter().map(|item| {
                format!(
                    "{}: {} missing, {} type, {} dimension{}",
                    item.name,
                    item.missing_properties.len(),
                    item.type_mismatches.len(),
                    item.array_dim_mismatches.len(),
                    if item.parent.is_some() {
                        ", parent"
                    } else {
                        ""
                    }
                )
            }),
        )?;
        write_examples(
            f,
            "enum differences",
            self.enum_differences.iter().map(|item| {
                format!(
                    "{}: {} missing, {} number",
                    item.name,
                    item.missing_values.len(),
                    item.number_mismatches.len()
                )
            }),
        )?;

        if self.is_clean() {
            write!(f, "result: clean")
        } else {
            write!(f, "result: differences found")
        }
    }
}
