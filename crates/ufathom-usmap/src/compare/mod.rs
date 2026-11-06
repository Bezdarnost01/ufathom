//! Comparison of a dump against a usmap: the oracle check.

mod report;

use std::collections::BTreeMap;

use ufathom_model::{Dump, EnumDef, FieldDef, StructDef};

use crate::mapping::short_name;
use crate::model::{Usmap, UsmapEnum, UsmapProperty, UsmapStruct, UsmapType};

pub use report::{
    AmbiguousName, ArrayDimMismatch, CaseOnlyDifference, CaseOnlyKind, EnumDifference,
    EnumNumberMismatch, MissingEnumValue, OracleCounts, OracleReport, ParentDifference, Side,
    StructDifference, TypeKind, TypeMismatch,
};

/// Largest array dimension a usmap can store.
const MAX_USMAP_ARRAY_DIM: u32 = u8::MAX as u32;

/// Names are matched ignoring case, because builds spell some names
/// differently.
fn fold(name: &str) -> String {
    name.to_lowercase()
}

/// Groups items by folded name. The map's order is the report order.
fn group<'a, T>(
    items: impl IntoIterator<Item = &'a T>,
    name: impl Fn(&T) -> &str,
) -> BTreeMap<String, Vec<&'a T>> {
    let mut groups: BTreeMap<String, Vec<&'a T>> = BTreeMap::new();
    for item in items {
        groups.entry(fold(name(item))).or_default().push(item);
    }
    groups
}

/// Checks a dump against a usmap taken from the same build.
///
/// The usmap is the oracle: it says what must exist. Types are matched by
/// short name, ignoring case, since usmap has no packages. Properties are
/// matched by name, not offset, since usmap has none. A name that more than
/// one dump type carries is reported as ambiguous and left unchecked; nothing
/// is resolved by picking a candidate.
///
/// Dump types a usmap lacks are listed for information only: the two may have
/// been taken at different moments with different objects loaded. For the same
/// reason, properties and enumerators that only the dump has are not reported.
///
/// The dump's property types are compared after mapping them to usmap (see
/// [`UsmapType::from_property_type`]), the only form usmap can hold. One
/// leniency is applied: `AssetObjectProperty` is the old name of
/// `SoftObjectProperty` and the two compare equal. Enumerator numbers are
/// compared only when the usmap version stores them.
///
/// Names that differ only in case are matches, listed in
/// [`OracleReport::case_only`] and counted separately.
#[must_use]
pub fn compare(dump: &Dump, usmap: &Usmap) -> OracleReport {
    let mut run = Comparison::default();
    run.structs(dump, usmap);
    run.enums(dump, usmap);
    run.finish(dump, usmap)
}

#[derive(Default)]
struct Comparison {
    report: OracleReport,
    matched_structs: usize,
    matched_enums: usize,
    properties_checked: usize,
}

impl Comparison {
    fn structs(&mut self, dump: &Dump, usmap: &Usmap) {
        let mut dump_groups = group(&dump.structs, |def| def.name.as_str());
        for candidates in dump_groups.values_mut() {
            candidates.sort_by(|a, b| a.path.cmp(&b.path));
        }
        let usmap_groups = group(&usmap.structs, |def| def.name.as_str());

        for (key, holders) in &usmap_groups {
            let [usmap_struct] = holders.as_slice() else {
                self.ambiguous_in_usmap(TypeKind::Struct, key, holders.iter().map(|d| &d.name));
                continue;
            };
            match dump_groups.get(key).map(Vec::as_slice) {
                None | Some([]) => self.report.missing_structs.push(usmap_struct.name.clone()),
                Some([dump_struct]) => {
                    self.matched_structs += 1;
                    self.compare_struct(dump_struct, usmap_struct);
                }
                // Reported from the dump side below.
                Some(_) => {}
            }
        }

        for (key, candidates) in &dump_groups {
            let in_usmap = usmap_groups.contains_key(key);
            match candidates.as_slice() {
                [only] if !in_usmap => self.report.extra_structs.push(only.path.clone()),
                [_, _, ..] => self.ambiguous_in_dump(
                    TypeKind::Struct,
                    key,
                    candidates.iter().map(|def| &def.path),
                    in_usmap,
                ),
                _ => {}
            }
        }
    }

    fn enums(&mut self, dump: &Dump, usmap: &Usmap) {
        let mut dump_groups = group(&dump.enums, |def| def.name.as_str());
        for candidates in dump_groups.values_mut() {
            candidates.sort_by(|a, b| a.path.cmp(&b.path));
        }
        let usmap_groups = group(&usmap.enums, |def| def.name.as_str());
        let explicit = usmap.has_explicit_enum_values();

        for (key, holders) in &usmap_groups {
            let [usmap_enum] = holders.as_slice() else {
                self.ambiguous_in_usmap(TypeKind::Enum, key, holders.iter().map(|d| &d.name));
                continue;
            };
            match dump_groups.get(key).map(Vec::as_slice) {
                None | Some([]) => self.report.missing_enums.push(usmap_enum.name.clone()),
                Some([dump_enum]) => {
                    self.matched_enums += 1;
                    self.compare_enum(dump_enum, usmap_enum, explicit);
                }
                Some(_) => {}
            }
        }

        for (key, candidates) in &dump_groups {
            let in_usmap = usmap_groups.contains_key(key);
            match candidates.as_slice() {
                [only] if !in_usmap => self.report.extra_enums.push(only.path.clone()),
                [_, _, ..] => self.ambiguous_in_dump(
                    TypeKind::Enum,
                    key,
                    candidates.iter().map(|def| &def.path),
                    in_usmap,
                ),
                _ => {}
            }
        }
    }

    fn ambiguous_in_dump<'a>(
        &mut self,
        kind: TypeKind,
        key: &str,
        paths: impl Iterator<Item = &'a String>,
        in_usmap: bool,
    ) {
        self.report.ambiguous.push(AmbiguousName {
            kind,
            side: Side::Dump,
            name: key.to_owned(),
            candidates: paths.cloned().collect(),
            in_other_input: in_usmap,
        });
    }

    fn ambiguous_in_usmap<'a>(
        &mut self,
        kind: TypeKind,
        key: &str,
        names: impl Iterator<Item = &'a String>,
    ) {
        self.report.ambiguous.push(AmbiguousName {
            kind,
            side: Side::Usmap,
            name: key.to_owned(),
            candidates: names.cloned().collect(),
            in_other_input: true,
        });
    }

    fn case_only(&mut self, kind: CaseOnlyKind, owner: &str, dump: &str, usmap: &str) {
        self.report.case_only.push(CaseOnlyDifference {
            kind,
            owner: owner.to_owned(),
            dump: dump.to_owned(),
            usmap: usmap.to_owned(),
        });
    }

    fn compare_struct(&mut self, dump: &StructDef, usmap: &UsmapStruct) {
        let owner = usmap.name.as_str();
        if dump.name != usmap.name {
            self.case_only(CaseOnlyKind::StructName, owner, &dump.name, owner);
        }

        let mut difference = StructDifference {
            name: usmap.name.clone(),
            dump_path: dump.path.clone(),
            parent: None,
            missing_properties: Vec::new(),
            type_mismatches: Vec::new(),
            array_dim_mismatches: Vec::new(),
        };

        let dump_parent = dump.super_path.as_deref().map(short_name);
        match (dump_parent, usmap.super_name.as_deref()) {
            (None, None) => {}
            (Some(left), Some(right)) if left == right => {}
            (Some(left), Some(right)) if fold(left) == fold(right) => {
                self.case_only(CaseOnlyKind::ParentName, owner, left, right);
            }
            (left, right) => {
                difference.parent = Some(ParentDifference {
                    dump: left.map(str::to_owned),
                    usmap: right.map(str::to_owned),
                });
            }
        }

        let fields = group(&dump.fields, |field| field.name.as_str());
        for property in &usmap.properties {
            self.properties_checked += 1;
            let candidates = fields.get(&fold(&property.name));
            let Some(best) = candidates
                .into_iter()
                .flatten()
                .map(|field| Evaluation::new(field, property))
                .min_by_key(|evaluation| (evaluation.mismatches(), !evaluation.exact_name))
            else {
                difference.missing_properties.push(property.name.clone());
                continue;
            };

            if !best.exact_name {
                self.case_only(
                    CaseOnlyKind::PropertyName,
                    owner,
                    &best.field.name,
                    &property.name,
                );
            }
            if best.type_equal {
                for (left, right) in &best.reference_case_notes {
                    self.case_only(CaseOnlyKind::TypeReference, owner, left, right);
                }
            } else {
                difference.type_mismatches.push(TypeMismatch {
                    property: property.name.clone(),
                    dump: best.field.ty.to_string(),
                    dump_as_usmap: best.mapped.to_string(),
                    usmap: property.ty.to_string(),
                });
            }
            if !best.dim_equal {
                difference.array_dim_mismatches.push(ArrayDimMismatch {
                    property: property.name.clone(),
                    dump: best.field.array_dim,
                    usmap: property.array_dim,
                });
            }
        }

        difference.missing_properties.sort();
        difference.type_mismatches.sort();
        difference.array_dim_mismatches.sort();
        if difference.parent.is_some()
            || !difference.missing_properties.is_empty()
            || !difference.type_mismatches.is_empty()
            || !difference.array_dim_mismatches.is_empty()
        {
            self.report.struct_differences.push(difference);
        }
    }

    fn compare_enum(&mut self, dump: &EnumDef, usmap: &UsmapEnum, explicit: bool) {
        let owner = usmap.name.as_str();
        if dump.name != usmap.name {
            self.case_only(CaseOnlyKind::EnumName, owner, &dump.name, owner);
        }

        let mut difference = EnumDifference {
            name: usmap.name.clone(),
            dump_path: dump.path.clone(),
            missing_values: Vec::new(),
            number_mismatches: Vec::new(),
        };

        let values = group(&dump.values, |value| value.name.as_str());
        for wanted in &usmap.values {
            let Some(candidates) = values.get(&fold(&wanted.name)) else {
                difference.missing_values.push(MissingEnumValue {
                    name: wanted.name.clone(),
                    value: explicit.then_some(wanted.value),
                });
                continue;
            };
            // Prefer the candidate with the same number, then the same
            // spelling, then the lowest number.
            let chosen = candidates.iter().copied().min_by_key(|value| {
                (
                    explicit && value.value != wanted.value,
                    value.name != wanted.name,
                    value.value,
                )
            });
            let Some(chosen) = chosen else { continue };
            if chosen.name != wanted.name {
                self.case_only(
                    CaseOnlyKind::EnumValueName,
                    owner,
                    &chosen.name,
                    &wanted.name,
                );
            }
            if explicit && chosen.value != wanted.value {
                difference.number_mismatches.push(EnumNumberMismatch {
                    name: wanted.name.clone(),
                    dump: chosen.value,
                    usmap: wanted.value,
                });
            }
        }

        difference.missing_values.sort();
        difference.number_mismatches.sort();
        if !difference.missing_values.is_empty() || !difference.number_mismatches.is_empty() {
            self.report.enum_differences.push(difference);
        }
    }

    fn finish(mut self, dump: &Dump, usmap: &Usmap) -> OracleReport {
        let report = &mut self.report;
        report.missing_structs.sort();
        report.extra_structs.sort();
        report.missing_enums.sort();
        report.extra_enums.sort();
        report.ambiguous.sort();
        report.struct_differences.sort();
        report.enum_differences.sort();
        report.case_only.sort();

        report.counts = OracleCounts {
            usmap_structs: usmap.structs.len(),
            usmap_enums: usmap.enums.len(),
            dump_structs: dump.structs.len(),
            dump_enums: dump.enums.len(),
            matched_structs: self.matched_structs,
            matched_enums: self.matched_enums,
            properties_checked: self.properties_checked,
            missing_structs: report.missing_structs.len(),
            extra_structs: report.extra_structs.len(),
            missing_enums: report.missing_enums.len(),
            extra_enums: report.extra_enums.len(),
            ambiguous_names: report.ambiguous.len(),
            structs_with_differences: report.struct_differences.len(),
            parent_differences: report
                .struct_differences
                .iter()
                .filter(|item| item.parent.is_some())
                .count(),
            missing_properties: report
                .struct_differences
                .iter()
                .map(|item| item.missing_properties.len())
                .sum(),
            type_mismatches: report
                .struct_differences
                .iter()
                .map(|item| item.type_mismatches.len())
                .sum(),
            array_dim_mismatches: report
                .struct_differences
                .iter()
                .map(|item| item.array_dim_mismatches.len())
                .sum(),
            enums_with_differences: report.enum_differences.len(),
            missing_enum_values: report
                .enum_differences
                .iter()
                .map(|item| item.missing_values.len())
                .sum(),
            enum_number_mismatches: report
                .enum_differences
                .iter()
                .map(|item| item.number_mismatches.len())
                .sum(),
            case_only_differences: report.case_only.len(),
        };
        self.report
    }
}

/// How one dump field measures up against a usmap property of the same name.
struct Evaluation<'a> {
    field: &'a FieldDef,
    mapped: UsmapType,
    exact_name: bool,
    type_equal: bool,
    dim_equal: bool,
    /// `(dump name, usmap name)` of struct and enum references that matched
    /// only ignoring case.
    reference_case_notes: Vec<(String, String)>,
}

impl<'a> Evaluation<'a> {
    fn new(field: &'a FieldDef, property: &UsmapProperty) -> Self {
        let mapped = UsmapType::from_property_type(&field.ty);
        let mut reference_case_notes = Vec::new();
        let type_equal = types_equal(&mapped, &property.ty, &mut reference_case_notes);
        let dump_dim = field.array_dim.clamp(1, MAX_USMAP_ARRAY_DIM);
        Self {
            field,
            mapped,
            exact_name: field.name == property.name,
            type_equal,
            dim_equal: dump_dim == u32::from(property.array_dim),
            reference_case_notes,
        }
    }

    fn mismatches(&self) -> u8 {
        u8::from(!self.type_equal) + u8::from(!self.dim_equal)
    }
}

/// Structural equality of a dump type (already mapped) and a usmap type.
/// References to structs and enums match ignoring case, and the pairs that
/// differ in case are pushed to `case_notes`.
fn types_equal(
    dump: &UsmapType,
    usmap: &UsmapType,
    case_notes: &mut Vec<(String, String)>,
) -> bool {
    let mut names_equal = |left: &str, right: &str| {
        if left == right {
            true
        } else if fold(left) == fold(right) {
            case_notes.push((left.to_owned(), right.to_owned()));
            true
        } else {
            false
        }
    };
    match (dump, usmap) {
        (UsmapType::Struct { name: left }, UsmapType::Struct { name: right }) => {
            names_equal(left, right)
        }
        (
            UsmapType::Enum {
                inner: left_inner,
                name: left,
            },
            UsmapType::Enum {
                inner: right_inner,
                name: right,
            },
        ) => names_equal(left, right) && types_equal(left_inner, right_inner, case_notes),
        (UsmapType::Array { inner: left }, UsmapType::Array { inner: right })
        | (UsmapType::Optional { inner: left }, UsmapType::Optional { inner: right })
        | (UsmapType::Set { element: left }, UsmapType::Set { element: right }) => {
            types_equal(left, right, case_notes)
        }
        (
            UsmapType::Map {
                key: left_key,
                value: left_value,
            },
            UsmapType::Map {
                key: right_key,
                value: right_value,
            },
        ) => {
            types_equal(left_key, right_key, case_notes)
                && types_equal(left_value, right_value, case_notes)
        }
        (
            UsmapType::SoftObject | UsmapType::AssetObject,
            UsmapType::SoftObject | UsmapType::AssetObject,
        ) => true,
        (left, right) => left == right,
    }
}
