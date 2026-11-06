//! Writing a dump and reading it back.

use ufathom_model::{PropertyType, StructKind};

use super::fixtures::{
    PROPERTY_TYPE_VARIANTS, class, class_with, dump_of, empty_dump, enumeration,
    every_property_type, field, function, variant_index,
};
use crate::{Compression, Usmap, UsmapType, UsmapVersion, write_usmap, write_usmap_with_report};

#[test]
fn fixture_covers_every_property_type_variant_once() {
    let mut seen = vec![0usize; PROPERTY_TYPE_VARIANTS];
    for (ty, _) in every_property_type() {
        seen[variant_index(&ty)] += 1;
    }
    assert!(seen.iter().all(|&count| count == 1), "{seen:?}");
}

#[test]
fn round_trip_covers_every_property_type() {
    let cases = every_property_type();
    let fields: Vec<(String, PropertyType)> = cases
        .iter()
        .enumerate()
        .map(|(position, (ty, _))| (format!("Field{position}"), ty.clone()))
        .collect();
    let refs: Vec<(&str, PropertyType)> = fields
        .iter()
        .map(|(name, ty)| (name.as_str(), ty.clone()))
        .collect();
    let dump = dump_of(
        vec![class_with("Holder", Some("Object"), &refs)],
        vec![enumeration("EMode", &[("A", 0), ("B", 5), ("C", 6)])],
    );

    let bytes = write_usmap(&dump);
    let usmap = Usmap::parse(&bytes).expect("own output parses");

    assert_eq!(usmap.version, UsmapVersion::ExplicitEnumValues);
    assert_eq!(usmap.compression, Compression::None);
    assert_eq!(usmap.version_info, None);
    assert_eq!(usmap.extension_bytes, 0);

    assert_eq!(usmap.structs.len(), 1);
    let holder = &usmap.structs[0];
    assert_eq!(holder.name, "Holder");
    assert_eq!(holder.super_name.as_deref(), Some("Object"));
    assert_eq!(holder.properties.len(), cases.len());
    assert_eq!(usize::from(holder.property_count), cases.len());
    for (position, (property, (dump_ty, expected))) in
        holder.properties.iter().zip(&cases).enumerate()
    {
        assert_eq!(property.name, format!("Field{position}"));
        assert_eq!(usize::from(property.index), position);
        assert_eq!(property.array_dim, 1);
        assert_eq!(&property.ty, expected, "mapping of {dump_ty}");
    }

    assert_eq!(usmap.enums.len(), 1);
    let values: Vec<(&str, i64)> = usmap.enums[0]
        .values
        .iter()
        .map(|value| (value.name.as_str(), value.value))
        .collect();
    assert_eq!(values, [("A", 0), ("B", 5), ("C", 6)]);

    // Every name is used and none is repeated.
    let mut sorted = usmap.names.clone();
    sorted.sort();
    sorted.dedup();
    assert_eq!(sorted.len(), usmap.names.len());
}

#[test]
fn byte_backed_by_an_enum_is_an_enum_property_over_byte() {
    let dump = dump_of(
        vec![class_with(
            "Holder",
            None,
            &[(
                "Role",
                PropertyType::Byte {
                    path: "/Script/Engine.ENetRole".to_owned(),
                },
            )],
        )],
        vec![],
    );
    let usmap = Usmap::parse(&write_usmap(&dump)).expect("parses");
    assert_eq!(
        usmap.structs[0].properties[0].ty,
        UsmapType::Enum {
            inner: Box::new(UsmapType::Byte),
            name: "ENetRole".to_owned()
        }
    );
}

#[test]
fn header_is_version_4_uncompressed() {
    let bytes = write_usmap(&empty_dump());
    // Magic, version 4, bHasVersionInfo = 0, compression none.
    assert_eq!(&bytes[..8], &[0xC4, 0x30, 0x04, 0, 0, 0, 0, 0]);
    let compressed = u32::from_le_bytes(bytes[8..12].try_into().expect("4 bytes"));
    let decompressed = u32::from_le_bytes(bytes[12..16].try_into().expect("4 bytes"));
    assert_eq!(compressed, decompressed);
    assert_eq!(usize::try_from(compressed).expect("size"), bytes.len() - 16);
}

#[test]
fn output_does_not_depend_on_input_order() {
    let mut dump = dump_of(
        vec![
            class_with(
                "Zed",
                Some("Alpha"),
                &[("B", PropertyType::Int32), ("A", PropertyType::Float)],
            ),
            class_with("Alpha", None, &[("X", PropertyType::Name)]),
        ],
        vec![
            enumeration("EZ", &[("Two", 2), ("One", 1)]),
            enumeration("EA", &[("Only", 0)]),
        ],
    );
    let first = write_usmap(&dump);
    dump.structs.reverse();
    dump.enums.reverse();
    for def in &mut dump.structs {
        def.fields.reverse();
    }
    for def in &mut dump.enums {
        def.values.reverse();
    }
    assert_eq!(write_usmap(&dump), first);
    assert_eq!(write_usmap(&dump), write_usmap(&dump));
}

#[test]
fn properties_are_numbered_by_array_slot() {
    let mut holder = class_with(
        "Holder",
        None,
        &[
            ("First", PropertyType::Int32),
            ("Array", PropertyType::Float),
            ("Last", PropertyType::Name),
        ],
    );
    holder.fields[1].array_dim = 4;
    let usmap = Usmap::parse(&write_usmap(&dump_of(vec![holder], vec![]))).expect("parses");
    let properties = &usmap.structs[0].properties;
    assert_eq!(
        properties
            .iter()
            .map(|p| (p.index, p.array_dim))
            .collect::<Vec<_>>(),
        [(0, 1), (1, 4), (5, 1)]
    );
    assert_eq!(usmap.structs[0].property_count, 6);
}

#[test]
fn script_structs_and_classes_are_both_written() {
    let mut vector = class_with("Vector", None, &[("X", PropertyType::Float)]);
    vector.kind = StructKind::ScriptStruct;
    let actor = class_with("Actor", Some("Object"), &[]);
    let usmap = Usmap::parse(&write_usmap(&dump_of(vec![vector, actor], vec![]))).expect("parses");
    let names: Vec<&str> = usmap.structs.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, ["Actor", "Vector"]);
}

#[test]
fn what_usmap_cannot_hold_is_reported_not_dropped_silently() {
    let mut big_array = class_with("Big", None, &[("Chars", PropertyType::Int8)]);
    big_array.fields[0].array_dim = 1000;

    let mut with_unknown = class_with(
        "Odd",
        None,
        &[
            (
                "Mystery",
                PropertyType::Unknown {
                    type_name: "FooProperty".to_owned(),
                },
            ),
            (
                "Nested",
                PropertyType::Array {
                    inner: Box::new(PropertyType::Unknown {
                        type_name: "BarProperty".to_owned(),
                    }),
                },
            ),
        ],
    );
    with_unknown.functions = vec![function("Odd", "Run"), function("Odd", "Stop")];

    // Two packages, one short name.
    let mut first = class("Dup", None);
    first.path = "/Script/A.Dup".to_owned();
    let mut second = class("Dup", None);
    second.path = "/Script/B.Dup".to_owned();
    let mut dup_enum_a = enumeration("EDup", &[("A", 1)]);
    dup_enum_a.path = "/Script/A.EDup".to_owned();
    let mut dup_enum_b = enumeration("EDup", &[("B", 2)]);
    dup_enum_b.path = "/Script/B.EDup".to_owned();

    let dump = dump_of(
        vec![second, big_array, with_unknown, first],
        vec![dup_enum_b, dup_enum_a],
    );
    let (bytes, report) = write_usmap_with_report(&dump);

    assert_eq!(report.duplicate_structs_skipped, ["/Script/B.Dup"]);
    assert_eq!(report.duplicate_enums_skipped, ["/Script/B.EDup"]);
    assert_eq!(report.unknown_property_types, ["Odd.Mystery", "Odd.Nested"]);
    assert_eq!(report.array_dims_clamped, ["Big.Chars (1000)"]);
    assert_eq!(report.functions_omitted, 2);
    assert!(report.limits_exceeded.is_empty());
    assert_eq!(bytes, write_usmap(&dump));

    // The first path in sorted order wins.
    let usmap = Usmap::parse(&bytes).expect("parses");
    assert_eq!(usmap.enums.len(), 1);
    assert_eq!(usmap.enums[0].values[0].name, "A");
    let big = usmap.structs.iter().find(|s| s.name == "Big").expect("Big");
    assert_eq!(big.properties[0].array_dim, 255);
    let odd = usmap.structs.iter().find(|s| s.name == "Odd").expect("Odd");
    assert_eq!(odd.properties[0].ty, UsmapType::Unknown);
    assert_eq!(usmap.structs.iter().filter(|s| s.name == "Dup").count(), 1);
}

#[test]
fn an_overlong_name_is_shortened_and_reported() {
    let long = "x".repeat(70_000);
    let mut holder = class("Holder", None);
    holder.fields = vec![field(&long, 0, PropertyType::Int32)];
    let (bytes, report) = write_usmap_with_report(&dump_of(vec![holder], vec![]));
    assert_eq!(report.limits_exceeded.len(), 1);
    let usmap = Usmap::parse(&bytes).expect("parses");
    assert_eq!(usmap.structs[0].properties[0].name.len(), 65_535);
}
