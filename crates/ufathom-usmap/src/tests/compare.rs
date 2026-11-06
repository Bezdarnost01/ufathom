//! The oracle comparison.

use ufathom_model::PropertyType;

use super::fixtures::{
    class, class_with, dump_of, enumeration, every_property_type, usmap_enum, usmap_of,
    usmap_struct,
};
use crate::{
    AmbiguousName, CaseOnlyKind, Side, TypeKind, Usmap, UsmapType, UsmapVersion, compare,
    write_usmap,
};

fn round_trip(dump: &ufathom_model::Dump) -> Usmap {
    Usmap::parse(&write_usmap(dump)).expect("own output parses")
}

fn int_field_dump() -> ufathom_model::Dump {
    dump_of(
        vec![
            class_with(
                "Actor",
                Some("Object"),
                &[
                    ("Health", PropertyType::Int32),
                    ("Tags", PropertyType::Name),
                ],
            ),
            class_with("Pawn", Some("Actor"), &[("Speed", PropertyType::Float)]),
        ],
        vec![enumeration("EMode", &[("Off", 0), ("On", 5)])],
    )
}

#[test]
fn identical_input_gives_a_clean_report() {
    let cases = every_property_type();
    let fields: Vec<(String, PropertyType)> = cases
        .iter()
        .enumerate()
        .map(|(position, (ty, _))| (format!("F{position}"), ty.clone()))
        .collect();
    let refs: Vec<(&str, PropertyType)> = fields
        .iter()
        .map(|(name, ty)| (name.as_str(), ty.clone()))
        .collect();
    let mut dump = int_field_dump();
    dump.structs.push(class_with("All", Some("Pawn"), &refs));

    let report = compare(&dump, &round_trip(&dump));

    assert!(report.is_clean(), "{report}");
    assert!(report.missing_structs.is_empty());
    assert!(report.extra_structs.is_empty());
    assert!(report.ambiguous.is_empty());
    assert!(report.struct_differences.is_empty());
    assert!(report.enum_differences.is_empty());
    assert!(report.case_only.is_empty());
    assert_eq!(report.counts.matched_structs, 3);
    assert_eq!(report.counts.matched_enums, 1);
    assert_eq!(report.counts.properties_checked, 2 + 1 + cases.len());
    assert_eq!(report.counts.usmap_structs, 3);
    assert_eq!(report.counts.dump_structs, 3);
}

#[test]
fn types_only_the_usmap_has_are_missing() {
    let dump = int_field_dump();
    let mut usmap = round_trip(&dump);
    usmap.structs.push(usmap_struct("Ghost", None, &[]));
    usmap.structs.push(usmap_struct("Aardvark", None, &[]));
    usmap.enums.push(usmap_enum("EGhost", &[("X", 0)]));

    let report = compare(&dump, &usmap);
    assert_eq!(report.missing_structs, ["Aardvark", "Ghost"]);
    assert_eq!(report.missing_enums, ["EGhost"]);
    assert_eq!(report.counts.missing_structs, 2);
    assert_eq!(report.counts.missing_enums, 1);
    assert!(!report.is_clean());
}

#[test]
fn types_only_the_dump_has_are_information() {
    let mut dump = int_field_dump();
    let usmap = round_trip(&dump);
    dump.structs.push(class("Newcomer", None));
    dump.enums.push(enumeration("ENewcomer", &[("A", 0)]));

    let report = compare(&dump, &usmap);
    assert_eq!(report.extra_structs, ["/Script/Test.Newcomer"]);
    assert_eq!(report.extra_enums, ["/Script/Test.ENewcomer"]);
    assert_eq!(report.counts.extra_structs, 1);
    assert!(report.is_clean(), "extra dump content is not a failure");
}

#[test]
fn parent_differences_are_reported() {
    let dump = int_field_dump();
    let mut usmap = round_trip(&dump);
    for def in &mut usmap.structs {
        match def.name.as_str() {
            "Pawn" => def.super_name = Some("Character".to_owned()),
            "Actor" => def.super_name = None,
            _ => {}
        }
    }
    let report = compare(&dump, &usmap);
    assert_eq!(report.counts.parent_differences, 2);
    let pawn = report
        .struct_differences
        .iter()
        .find(|d| d.name == "Pawn")
        .expect("Pawn");
    let parent = pawn.parent.as_ref().expect("parent");
    assert_eq!(parent.dump.as_deref(), Some("Actor"));
    assert_eq!(parent.usmap.as_deref(), Some("Character"));
    let actor = report
        .struct_differences
        .iter()
        .find(|d| d.name == "Actor")
        .expect("Actor");
    let parent = actor.parent.as_ref().expect("parent");
    assert_eq!(parent.dump.as_deref(), Some("Object"));
    assert_eq!(parent.usmap, None);
}

#[test]
fn property_differences_are_reported_by_name_not_offset() {
    let dump = int_field_dump();
    let mut usmap = round_trip(&dump);
    {
        let actor = usmap
            .structs
            .iter_mut()
            .find(|d| d.name == "Actor")
            .expect("Actor");
        // Not in the dump.
        actor.properties.push(crate::UsmapProperty {
            name: "Armor".to_owned(),
            index: 9,
            array_dim: 1,
            ty: UsmapType::Int32,
        });
        // Wrong type.
        actor.properties[0].ty = UsmapType::Int64;
        // Wrong dimension.
        actor.properties[1].array_dim = 3;
        // A different index must not matter.
        actor.properties[1].index = 40;
    }

    let report = compare(&dump, &usmap);
    assert_eq!(report.struct_differences.len(), 1);
    let actor = &report.struct_differences[0];
    assert_eq!(actor.name, "Actor");
    assert_eq!(actor.missing_properties, ["Armor"]);
    assert_eq!(actor.type_mismatches.len(), 1);
    assert_eq!(actor.type_mismatches[0].property, "Health");
    assert_eq!(actor.type_mismatches[0].dump, "int32");
    assert_eq!(actor.type_mismatches[0].usmap, "Int64");
    assert_eq!(actor.array_dim_mismatches.len(), 1);
    assert_eq!(actor.array_dim_mismatches[0].property, "Tags");
    assert_eq!(actor.array_dim_mismatches[0].dump, 1);
    assert_eq!(actor.array_dim_mismatches[0].usmap, 3);
    assert_eq!(report.counts.missing_properties, 1);
    assert_eq!(report.counts.type_mismatches, 1);
    assert_eq!(report.counts.array_dim_mismatches, 1);
    assert_eq!(report.counts.structs_with_differences, 1);
}

#[test]
fn properties_only_the_dump_has_are_not_reported() {
    let dump = int_field_dump();
    let mut usmap = round_trip(&dump);
    usmap.structs[0].properties.pop();
    let report = compare(&dump, &usmap);
    assert!(report.is_clean(), "{report}");
}

#[test]
fn nested_type_differences_are_found() {
    let dump = dump_of(
        vec![class_with(
            "Holder",
            None,
            &[
                (
                    "Map",
                    PropertyType::Map {
                        key: Box::new(PropertyType::Name),
                        value: Box::new(PropertyType::Array {
                            inner: Box::new(PropertyType::Float),
                        }),
                    },
                ),
                (
                    "Link",
                    PropertyType::Struct {
                        path: "/Script/Engine.Vector".to_owned(),
                    },
                ),
            ],
        )],
        vec![],
    );
    let mut usmap = round_trip(&dump);
    usmap.structs[0].properties[0].ty = UsmapType::Map {
        key: Box::new(UsmapType::Name),
        value: Box::new(UsmapType::Array {
            inner: Box::new(UsmapType::Double),
        }),
    };
    usmap.structs[0].properties[1].ty = UsmapType::Struct {
        name: "Rotator".to_owned(),
    };
    let report = compare(&dump, &usmap);
    assert_eq!(report.counts.type_mismatches, 2);
    let mismatch = &report.struct_differences[0].type_mismatches[0];
    assert_eq!(mismatch.property, "Link");
    assert_eq!(mismatch.dump, "Struct</Script/Engine.Vector>");
    assert_eq!(mismatch.dump_as_usmap, "Struct<Vector>");
    assert_eq!(mismatch.usmap, "Struct<Rotator>");
}

#[test]
fn enum_differences_are_reported() {
    let dump = int_field_dump();
    let mut usmap = round_trip(&dump);
    usmap.enums[0].values[1].value = 6; // On: 5 in the dump
    usmap.enums[0].values.push(crate::UsmapEnumValue {
        name: "Extra".to_owned(),
        value: 9,
    });
    let report = compare(&dump, &usmap);
    assert_eq!(report.enum_differences.len(), 1);
    let diff = &report.enum_differences[0];
    assert_eq!(diff.name, "EMode");
    assert_eq!(diff.missing_values.len(), 1);
    assert_eq!(diff.missing_values[0].name, "Extra");
    assert_eq!(diff.missing_values[0].value, Some(9));
    assert_eq!(diff.number_mismatches.len(), 1);
    assert_eq!(diff.number_mismatches[0].name, "On");
    assert_eq!(
        (
            diff.number_mismatches[0].dump,
            diff.number_mismatches[0].usmap
        ),
        (5, 6)
    );
    assert_eq!(report.counts.enum_number_mismatches, 1);
    assert_eq!(report.counts.missing_enum_values, 1);
    assert_eq!(report.counts.enums_with_differences, 1);
}

#[test]
fn enum_numbers_are_not_compared_when_the_usmap_version_has_none() {
    // Version 3 stores no numbers: the parsed "numbers" are positions.
    let dump = dump_of(vec![], vec![enumeration("EMode", &[("Off", 0), ("On", 5)])]);
    let mut usmap = usmap_of(
        vec![],
        vec![usmap_enum("EMode", &[("Off", 0), ("On", 1), ("Gone", 2)])],
    );
    usmap.version = UsmapVersion::LargeEnums;

    let report = compare(&dump, &usmap);
    assert_eq!(report.counts.enum_number_mismatches, 0);
    assert_eq!(report.enum_differences.len(), 1);
    assert_eq!(report.enum_differences[0].missing_values.len(), 1);
    assert_eq!(report.enum_differences[0].missing_values[0].name, "Gone");
    assert_eq!(report.enum_differences[0].missing_values[0].value, None);
}

#[test]
fn a_name_used_twice_in_the_dump_is_ambiguous_and_not_compared() {
    let mut first = class_with("Dup", None, &[("A", PropertyType::Int32)]);
    first.path = "/Script/First.Dup".to_owned();
    let mut second = class_with("Dup", None, &[("B", PropertyType::Int32)]);
    second.path = "/Script/Second.Dup".to_owned();
    let mut enum_a = enumeration("EDup", &[("A", 0)]);
    enum_a.path = "/Script/First.EDup".to_owned();
    let mut enum_b = enumeration("EDup", &[("B", 0)]);
    enum_b.path = "/Script/Second.EDup".to_owned();
    let unused = {
        let mut a = class("OnlyInDump", None);
        a.path = "/Script/First.OnlyInDump".to_owned();
        let mut b = class("OnlyInDump", None);
        b.path = "/Script/Second.OnlyInDump".to_owned();
        [a, b]
    };
    let mut structs = vec![second, first, class("Fine", None)];
    structs.extend(unused);
    let dump = dump_of(structs, vec![enum_b, enum_a]);

    let usmap = usmap_of(
        vec![
            usmap_struct("Dup", None, &[("Anything", UsmapType::Int32)]),
            usmap_struct("Fine", None, &[]),
        ],
        vec![usmap_enum("EDup", &[("Anything", 3)])],
    );

    let report = compare(&dump, &usmap);
    assert_eq!(
        report.ambiguous,
        [
            AmbiguousName {
                kind: TypeKind::Struct,
                side: Side::Dump,
                name: "dup".to_owned(),
                candidates: vec![
                    "/Script/First.Dup".to_owned(),
                    "/Script/Second.Dup".to_owned()
                ],
                in_other_input: true,
            },
            AmbiguousName {
                kind: TypeKind::Struct,
                side: Side::Dump,
                name: "onlyindump".to_owned(),
                candidates: vec![
                    "/Script/First.OnlyInDump".to_owned(),
                    "/Script/Second.OnlyInDump".to_owned()
                ],
                in_other_input: false,
            },
            AmbiguousName {
                kind: TypeKind::Enum,
                side: Side::Dump,
                name: "edup".to_owned(),
                candidates: vec![
                    "/Script/First.EDup".to_owned(),
                    "/Script/Second.EDup".to_owned()
                ],
                in_other_input: true,
            },
        ]
    );
    // Not resolved by picking one: no missing property was reported for the
    // ambiguous types, and they were neither matched nor called missing.
    assert!(report.struct_differences.is_empty());
    assert!(report.enum_differences.is_empty());
    assert!(report.missing_structs.is_empty());
    assert!(report.missing_enums.is_empty());
    assert_eq!(report.counts.matched_structs, 1);
    assert_eq!(report.counts.matched_enums, 0);
    assert_eq!(report.counts.ambiguous_names, 3);
    assert!(!report.is_clean());
}

#[test]
fn a_name_used_twice_in_the_usmap_is_ambiguous() {
    let dump = dump_of(vec![class("Twice", None)], vec![]);
    let usmap = usmap_of(
        vec![
            usmap_struct("Twice", None, &[]),
            usmap_struct("twice", None, &[]),
        ],
        vec![],
    );
    let report = compare(&dump, &usmap);
    assert_eq!(report.ambiguous.len(), 1);
    assert_eq!(report.ambiguous[0].side, Side::Usmap);
    assert_eq!(report.ambiguous[0].candidates, ["Twice", "twice"]);
    assert!(report.struct_differences.is_empty());
    assert!(report.extra_structs.is_empty());
}

#[test]
fn case_only_differences_match_but_are_counted_separately() {
    let dump = dump_of(
        vec![
            class_with(
                "Pawn",
                Some("Actor"),
                &[
                    ("Health", PropertyType::Int32),
                    (
                        "Mode",
                        PropertyType::Byte {
                            path: "/Script/Test.EMode".to_owned(),
                        },
                    ),
                ],
            ),
            class("Actor", None),
        ],
        vec![enumeration("EMode", &[("Off", 0)])],
    );
    let usmap = usmap_of(
        vec![
            usmap_struct(
                "PAWN",
                Some("ACTOR"),
                &[
                    ("health", UsmapType::Int32),
                    (
                        "Mode",
                        UsmapType::Enum {
                            inner: Box::new(UsmapType::Byte),
                            name: "emode".to_owned(),
                        },
                    ),
                ],
            ),
            usmap_struct("Actor", None, &[]),
        ],
        vec![usmap_enum("eMode", &[("OFF", 0)])],
    );

    let report = compare(&dump, &usmap);
    assert!(report.is_clean(), "{report}");
    assert_eq!(report.counts.matched_structs, 2);
    assert_eq!(report.counts.type_mismatches, 0);
    assert_eq!(report.counts.missing_properties, 0);
    assert_eq!(report.counts.case_only_differences, 6);
    let kinds: Vec<CaseOnlyKind> = report.case_only.iter().map(|d| d.kind).collect();
    assert_eq!(
        kinds,
        [
            CaseOnlyKind::StructName,
            CaseOnlyKind::EnumName,
            CaseOnlyKind::ParentName,
            CaseOnlyKind::PropertyName,
            CaseOnlyKind::TypeReference,
            CaseOnlyKind::EnumValueName,
        ]
    );
    let property = report
        .case_only
        .iter()
        .find(|d| d.kind == CaseOnlyKind::PropertyName)
        .expect("property");
    assert_eq!(
        (property.dump.as_str(), property.usmap.as_str()),
        ("Health", "health")
    );
    assert_eq!(property.owner, "PAWN");
}

#[test]
fn a_difference_beyond_case_is_still_a_mismatch() {
    let dump = dump_of(
        vec![class_with(
            "Holder",
            None,
            &[(
                "Link",
                PropertyType::Struct {
                    path: "/Script/Engine.Vector".to_owned(),
                },
            )],
        )],
        vec![],
    );
    let usmap = usmap_of(
        vec![usmap_struct(
            "Holder",
            None,
            &[(
                "Link",
                UsmapType::Struct {
                    name: "Vector2D".to_owned(),
                },
            )],
        )],
        vec![],
    );
    let report = compare(&dump, &usmap);
    assert_eq!(report.counts.type_mismatches, 1);
    assert_eq!(report.counts.case_only_differences, 0);
}

#[test]
fn lossy_mappings_and_old_names_compare_equal() {
    let dump = dump_of(
        vec![class_with(
            "Holder",
            None,
            &[
                (
                    "Meta",
                    PropertyType::Class {
                        class: "/Script/CoreUObject.Class".to_owned(),
                        meta: "/Script/Engine.Actor".to_owned(),
                    },
                ),
                (
                    "Soft",
                    PropertyType::SoftObject {
                        class: "/Script/Engine.Texture".to_owned(),
                    },
                ),
                (
                    "Role",
                    PropertyType::Enum {
                        path: "/Script/Test.ERole".to_owned(),
                        underlying: Box::new(PropertyType::UInt8),
                    },
                ),
            ],
        )],
        vec![],
    );
    let usmap = usmap_of(
        vec![usmap_struct(
            "Holder",
            None,
            &[
                ("Meta", UsmapType::Object),
                ("Soft", UsmapType::AssetObject),
                (
                    "Role",
                    UsmapType::Enum {
                        inner: Box::new(UsmapType::Byte),
                        name: "ERole".to_owned(),
                    },
                ),
            ],
        )],
        vec![],
    );
    assert!(compare(&dump, &usmap).is_clean());
}

#[test]
fn a_dump_array_dimension_above_255_matches_a_stored_255() {
    let mut holder = class_with("Holder", None, &[("Chars", PropertyType::Int8)]);
    holder.fields[0].array_dim = 1000;
    let dump = dump_of(vec![holder], vec![]);
    let report = compare(&dump, &round_trip(&dump));
    assert!(report.is_clean(), "{report}");

    let mut usmap = round_trip(&dump);
    usmap.structs[0].properties[0].array_dim = 200;
    assert_eq!(compare(&dump, &usmap).counts.array_dim_mismatches, 1);
}

#[test]
fn the_report_does_not_depend_on_input_order() {
    let mut dump = int_field_dump();
    dump.structs.push(class("Extra1", None));
    dump.structs.push(class("Extra2", None));
    let mut usmap = round_trip(&int_field_dump());
    usmap.structs.push(usmap_struct("MissingB", None, &[]));
    usmap.structs.push(usmap_struct("MissingA", None, &[]));
    usmap.structs[0].properties.push(crate::UsmapProperty {
        name: "Zeta".to_owned(),
        index: 20,
        array_dim: 1,
        ty: UsmapType::Int32,
    });
    usmap.structs[0].properties.push(crate::UsmapProperty {
        name: "Alpha".to_owned(),
        index: 21,
        array_dim: 1,
        ty: UsmapType::Int32,
    });

    let first = compare(&dump, &usmap);

    dump.structs.reverse();
    usmap.structs.reverse();
    for def in &mut usmap.structs {
        def.properties.reverse();
    }
    let second = compare(&dump, &usmap);

    assert_eq!(first, second);
    assert_eq!(first.missing_structs, ["MissingA", "MissingB"]);
    assert_eq!(
        first.extra_structs,
        ["/Script/Test.Extra1", "/Script/Test.Extra2"]
    );
    assert_eq!(
        first.struct_differences[0].missing_properties,
        ["Alpha", "Zeta"]
    );
}

#[test]
fn the_report_serializes_and_displays() {
    let dump = int_field_dump();
    let mut usmap = round_trip(&dump);
    usmap.structs.push(usmap_struct("Ghost", None, &[]));
    let report = compare(&dump, &usmap);

    let json = serde_json::to_string(&report).expect("serializes");
    let value: serde_json::Value = serde_json::from_str(&json).expect("json");
    assert_eq!(value["counts"]["missing_structs"], 1);
    assert_eq!(value["missing_structs"][0], "Ghost");
    assert_eq!(json, serde_json::to_string(&report).expect("serializes"));

    let text = report.to_string();
    assert!(text.contains("missing from the dump: 1 structs"), "{text}");
    assert!(text.contains("Ghost"), "{text}");
    assert!(text.ends_with("result: differences found"), "{text}");

    let clean = compare(&dump, &round_trip(&dump));
    assert!(clean.to_string().ends_with("result: clean"));
}

#[test]
fn an_empty_dump_and_usmap_are_clean() {
    let report = compare(&super::fixtures::empty_dump(), &usmap_of(vec![], vec![]));
    assert!(report.is_clean());
    assert_eq!(report.counts, crate::OracleCounts::default());
}
