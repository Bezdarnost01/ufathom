//! Tests of the dump IR: round trip, determinism and version checks.

use std::collections::BTreeSet;

use super::{Dump, EngineInfo, FORMAT_VERSION, PropertyType};
use crate::error::ModelError;
use crate::test_support::{
    PROPERTY_TYPE_VARIANTS, busy_dump, class, dump_of, empty_dump, every_property_type, field,
    function, param, variant_index,
};

/// Reverses every list that is allowed to change order. Function parameters
/// are left alone on purpose.
fn shuffled(dump: &Dump) -> Dump {
    let mut copy = dump.clone();
    copy.packages.reverse();
    copy.structs.reverse();
    copy.enums.reverse();
    copy.diagnostics.reverse();
    for def in &mut copy.structs {
        def.fields.reverse();
        def.functions.reverse();
    }
    for def in &mut copy.enums {
        def.values.reverse();
    }
    copy.structs.rotate_left(1);
    copy
}

#[test]
fn fixture_covers_every_property_type() {
    let types = every_property_type();
    assert_eq!(types.len(), PROPERTY_TYPE_VARIANTS);
    let seen: BTreeSet<usize> = types.iter().map(variant_index).collect();
    assert_eq!(seen.len(), PROPERTY_TYPE_VARIANTS);
}

#[test]
fn json_round_trip_keeps_every_property_type() {
    let dump = busy_dump();
    let json = dump.to_json().unwrap();
    let back = Dump::from_json(&json).unwrap();

    let mut expected = dump;
    expected.normalize();
    assert_eq!(back, expected);

    let seen: BTreeSet<usize> = back
        .structs
        .iter()
        .flat_map(|def| def.fields.iter())
        .map(|field| variant_index(&field.ty))
        .collect();
    assert_eq!(seen.len(), PROPERTY_TYPE_VARIANTS);
}

#[test]
fn nested_types_round_trip() {
    let nested = PropertyType::Map {
        key: Box::new(PropertyType::Name),
        value: Box::new(PropertyType::Array {
            inner: Box::new(PropertyType::Optional {
                inner: Box::new(PropertyType::Set {
                    element: Box::new(PropertyType::Enum {
                        path: "/Script/X.E".to_owned(),
                        underlying: Box::new(PropertyType::Int64),
                    }),
                }),
            }),
        }),
    };
    let json = serde_json::to_string(&nested).unwrap();
    assert_eq!(serde_json::from_str::<PropertyType>(&json).unwrap(), nested);
}

#[test]
fn absent_engine_information_is_legal() {
    let mut dump = empty_dump();
    dump.engine = EngineInfo::default();
    let back = Dump::from_json(&dump.to_json().unwrap()).unwrap();
    assert_eq!(back.engine, EngineInfo::default());
}

#[test]
fn same_content_in_shuffled_order_serializes_identically() {
    let dump = busy_dump();
    let other = shuffled(&dump);
    assert_ne!(dump, other, "the shuffle must actually change the order");
    assert_eq!(dump.to_json().unwrap(), other.to_json().unwrap());
}

#[test]
fn duplicate_paths_still_have_one_output_order() {
    let mut first = dump_of(
        vec![
            class("/Script/A", "Dup", None, 8),
            class("/Script/A", "Dup", None, 16),
        ],
        Vec::new(),
    );
    let mut second = first.clone();
    second.structs.reverse();
    assert_eq!(first.to_json().unwrap(), second.to_json().unwrap());
    first.normalize();
    second.normalize();
    assert_eq!(first, second);
}

#[test]
fn normalize_is_idempotent() {
    let mut once = busy_dump();
    once.normalize();
    let mut twice = once.clone();
    twice.normalize();
    assert_eq!(once, twice);
}

#[test]
fn to_json_does_not_modify_the_dump() {
    let dump = busy_dump();
    let before = dump.clone();
    let _ = dump.to_json().unwrap();
    assert_eq!(dump, before);
}

#[test]
fn packages_are_sorted_and_deduplicated() {
    let mut dump = busy_dump();
    dump.normalize();
    assert_eq!(
        dump.packages,
        ["/Script/Core", "/Script/CoreUObject", "/Script/Engine"]
    );
}

#[test]
fn fields_sort_by_offset_then_name() {
    let mut dump = dump_of(vec![class("/Script/A", "T", None, 0x20)], Vec::new());
    dump.structs[0].fields = vec![
        field("B", 8, PropertyType::Int32),
        field("C", 0, PropertyType::Int32),
        field("A", 8, PropertyType::Int32),
    ];
    dump.normalize();
    let names: Vec<&str> = dump.structs[0]
        .fields
        .iter()
        .map(|field| field.name.as_str())
        .collect();
    assert_eq!(names, ["C", "A", "B"]);
}

#[test]
fn enum_values_sort_by_value_then_name() {
    let mut dump = busy_dump();
    dump.normalize();
    let axis = dump.enums.iter().find(|def| def.name == "EAxis").unwrap();
    let values: Vec<(&str, i64)> = axis
        .values
        .iter()
        .map(|entry| (entry.name.as_str(), entry.value))
        .collect();
    assert_eq!(values, [("X", 0), ("Y", 1), ("Z", 2)]);
}

#[test]
fn parameters_keep_declared_order() {
    let owner = "/Script/A.T";
    let make = |params| {
        let mut def = class("/Script/A", "T", None, 8);
        def.functions = vec![function(owner, "F", 0, params)];
        dump_of(vec![def], Vec::new())
    };
    let int_then_float = make(vec![
        param("X", 0, PropertyType::Int32, 0),
        param("Y", 4, PropertyType::Float, 0),
    ]);
    let float_then_int = make(vec![
        param("Y", 4, PropertyType::Float, 0),
        param("X", 0, PropertyType::Int32, 0),
    ]);

    let mut normalized = int_then_float.clone();
    normalized.normalize();
    assert_eq!(
        normalized, int_then_float,
        "normalize must not reorder parameters"
    );
    assert_ne!(
        int_then_float.to_json().unwrap(),
        float_then_int.to_json().unwrap()
    );
}

#[test]
fn functions_sort_by_path() {
    let mut dump = busy_dump();
    dump.normalize();
    let actor = dump.structs.iter().find(|def| def.name == "Actor").unwrap();
    let names: Vec<&str> = actor
        .functions
        .iter()
        .map(|function| function.name.as_str())
        .collect();
    assert_eq!(names, ["Alpha", "Zeta"]);
}

#[test]
fn diagnostics_sort_by_path_then_severity() {
    let mut dump = busy_dump();
    dump.normalize();
    let order: Vec<(&str, super::Severity)> = dump
        .diagnostics
        .iter()
        .map(|entry| (entry.path.as_str(), entry.severity))
        .collect();
    assert_eq!(
        order,
        [
            ("/Script/Engine.Actor", super::Severity::Info),
            ("/Script/Engine.Actor", super::Severity::Error),
            ("/Script/Engine.Zed", super::Severity::Warning),
        ]
    );
}

#[test]
fn new_dumps_carry_the_current_format_version() {
    assert_eq!(empty_dump().format_version, FORMAT_VERSION);
}

fn with_format_version(json: &str, version: serde_json::Value) -> String {
    let mut value: serde_json::Value = serde_json::from_str(json).unwrap();
    value["format_version"] = version;
    value.to_string()
}

#[test]
fn unknown_format_version_is_rejected_with_a_clear_error() {
    let json = busy_dump().to_json().unwrap();
    for found in [0_u64, u64::from(FORMAT_VERSION) + 1, 999] {
        let tampered = with_format_version(&json, found.into());
        let error = Dump::from_json(&tampered).unwrap_err();
        match &error {
            ModelError::UnsupportedFormatVersion {
                found: reported,
                supported,
            } => {
                assert_eq!(*reported, found);
                assert_eq!(*supported, FORMAT_VERSION);
            }
            other => panic!("expected UnsupportedFormatVersion, got {other:?}"),
        }
        let message = error.to_string();
        assert!(message.contains("format_version"), "{message}");
        assert!(message.contains(&found.to_string()), "{message}");
    }
}

#[test]
fn a_future_format_is_refused_even_if_its_shape_changed() {
    let text = r#"{"format_version": 2, "something_new": [1, 2, 3]}"#;
    assert!(matches!(
        Dump::from_json(text),
        Err(ModelError::UnsupportedFormatVersion { found: 2, .. })
    ));
}

#[test]
fn malformed_input_is_a_parse_error() {
    let json = busy_dump().to_json().unwrap();
    let missing_version = {
        let mut value: serde_json::Value = serde_json::from_str(&json).unwrap();
        value.as_object_mut().unwrap().remove("format_version");
        value.to_string()
    };
    let non_numeric = with_format_version(&json, "one".into());
    let wrong_shape = format!(r#"{{"format_version": {FORMAT_VERSION}}}"#);
    let truncated = json.get(..json.len() / 2).unwrap().to_owned();

    for text in [
        missing_version.as_str(),
        non_numeric.as_str(),
        wrong_shape.as_str(),
        truncated.as_str(),
        "",
        "not json",
    ] {
        assert!(
            matches!(Dump::from_json(text), Err(ModelError::Parse(_))),
            "input {text:.40} should be a parse error"
        );
    }
}

#[test]
fn json_holds_no_run_specific_data_keys() {
    let json = busy_dump().to_json().unwrap();
    let value: serde_json::Value = serde_json::from_str(&json).unwrap();
    let target = value["target"].as_object().unwrap();
    let keys: BTreeSet<&str> = target.keys().map(String::as_str).collect();
    assert_eq!(keys, BTreeSet::from(["module", "build_id"]));
}
