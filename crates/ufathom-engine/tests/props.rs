//! Property decoding rules: class names, missing payloads and the conversion
//! to dump types.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "test code may panic on failure; clippy.toml only exempts #[test] functions, not helpers"
)]

mod common;

use std::collections::HashMap;

use common::{Img, small, walk};
use ufathom_engine::limits::MAX_PROPERTY_DEPTH;
use ufathom_engine::{
    EngineError, FieldIter, ModernProperties, NameTable, PropertyKind, PropertySystemReader,
    RawPayload, RawProperty, property_type,
};
use ufathom_mem::Addr;
use ufathom_model::PropertyType;
use ufathom_model::layout::TypedPayloadLayout;
use ufathom_testkit::{Fixture, Profile};

#[test]
fn every_class_name_of_every_era_maps_to_one_kind() {
    use PropertyKind::*;
    for (name, kind) in [
        ("Int8Property", Int8),
        ("Int16Property", Int16),
        ("IntProperty", Int32),
        ("Int64Property", Int64),
        ("UInt16Property", UInt16),
        ("UInt32Property", UInt32),
        ("UInt64Property", UInt64),
        ("FloatProperty", Float),
        ("DoubleProperty", Double),
        ("ByteProperty", Byte),
        ("BoolProperty", Bool),
        ("NameProperty", Name),
        ("StrProperty", Str),
        ("TextProperty", Text),
        ("ObjectProperty", Object),
        ("ObjectPtrProperty", Object),
        ("ClassProperty", Class),
        ("ClassPtrProperty", Class),
        ("WeakObjectProperty", WeakObject),
        ("LazyObjectProperty", LazyObject),
        ("SoftObjectProperty", SoftObject),
        ("AssetObjectProperty", SoftObject),
        ("SoftClassProperty", SoftClass),
        ("AssetClassProperty", SoftClass),
        ("InterfaceProperty", Interface),
        ("StructProperty", Struct),
        ("ArrayProperty", Array),
        ("SetProperty", Set),
        ("MapProperty", Map),
        ("EnumProperty", Enum),
        ("DelegateProperty", Delegate),
        ("MulticastDelegateProperty", MulticastInlineDelegate),
        ("MulticastInlineDelegateProperty", MulticastInlineDelegate),
        ("MulticastSparseDelegateProperty", MulticastSparseDelegate),
        ("FieldPathProperty", FieldPath),
        ("OptionalProperty", Optional),
    ] {
        assert_eq!(PropertyKind::from_class_name(name), kind, "{name}");
    }
    for name in [
        "",
        "FooProperty",
        "intproperty",
        "Property",
        "NumericProperty",
        "Int",
    ] {
        assert_eq!(PropertyKind::from_class_name(name), Unknown, "{name:?}");
    }
}

/// A property system that serves hand-made properties, for the conversion.
struct Stub<'a> {
    inner: ModernProperties<'a>,
    props: HashMap<Addr, RawProperty>,
}

impl PropertySystemReader for Stub<'_> {
    fn fields(&self, owner: Addr) -> FieldIter<'_> {
        self.inner.fields(owner)
    }

    fn functions(&self, owner: Addr) -> FieldIter<'_> {
        self.inner.functions(owner)
    }

    fn decode(&self, field: Addr) -> Result<RawProperty, EngineError> {
        self.props.get(&field).cloned().ok_or(EngineError::Null {
            addr: field,
            what: "stub property",
        })
    }
}

fn raw(class: &str, payload: RawPayload) -> RawProperty {
    RawProperty {
        address: 0x1000,
        name: "Member".to_owned(),
        class_name: class.to_owned(),
        offset: 0,
        element_size: 8,
        array_dim: 1,
        flags: 0,
        payload,
    }
}

/// Converts with a path table: addresses 0x100 and up are objects named
/// `/P.O<addr>`, except `0x666`, which has no path, and `0x777`, whose path
/// is empty.
fn convert(props: &[(Addr, RawProperty)], top: &RawProperty) -> PropertyType {
    let fx = small(Profile::ue4_25_case_preserving_measured());
    let memory = Img::new(0x1000_0000, 0x100).snapshot("g.exe");
    let names = NameTable::default();
    let stub = Stub {
        inner: ModernProperties::new(&memory, &fx.layout, &names),
        props: props.iter().cloned().collect(),
    };
    let path_of = |addr: Addr| match addr {
        0x666 => None,
        0x777 => Some(String::new()),
        other => Some(format!("/P.O{other:x}")),
    };
    property_type(&stub, top, &path_of)
}

fn unknown(class: &str) -> PropertyType {
    PropertyType::Unknown {
        type_name: class.to_owned(),
    }
}

#[test]
fn simple_and_reference_types_convert_through_the_path_closure() {
    let scalar = |class: &str| convert(&[], &raw(class, RawPayload::Scalar));
    assert_eq!(scalar("IntProperty"), PropertyType::Int32);
    assert_eq!(scalar("UInt64Property"), PropertyType::UInt64);
    assert_eq!(scalar("NameProperty"), PropertyType::Name);
    assert_eq!(scalar("FieldPathProperty"), PropertyType::FieldPath);
    let object = |class: &str, address: Addr| {
        convert(&[], &raw(class, RawPayload::Object { class: address }))
    };
    assert_eq!(
        object("ObjectProperty", 0x100),
        PropertyType::Object {
            class: "/P.O100".into()
        }
    );
    assert_eq!(
        object("AssetObjectProperty", 0x100),
        PropertyType::SoftObject {
            class: "/P.O100".into()
        }
    );
    assert_eq!(
        object("WeakObjectProperty", 0x100),
        PropertyType::WeakObject {
            class: "/P.O100".into()
        }
    );
    assert_eq!(
        object("LazyObjectProperty", 0x100),
        PropertyType::LazyObject {
            class: "/P.O100".into()
        }
    );
    // SoftClass keeps only the meta class, Class both.
    let class = RawPayload::Class {
        class: 0x100,
        meta: 0x200,
    };
    assert_eq!(
        convert(&[], &raw("AssetClassProperty", class)),
        PropertyType::SoftClass {
            meta: "/P.O200".into()
        }
    );
    assert_eq!(
        convert(&[], &raw("ClassProperty", class)),
        PropertyType::Class {
            class: "/P.O100".into(),
            meta: "/P.O200".into()
        }
    );
    let signature = RawPayload::Delegate { signature: 0x300 };
    assert_eq!(
        convert(&[], &raw("MulticastDelegateProperty", signature)),
        PropertyType::MulticastInlineDelegate {
            signature: "/P.O300".into()
        }
    );
    assert_eq!(
        convert(&[], &raw("MulticastSparseDelegateProperty", signature)),
        PropertyType::MulticastSparseDelegate {
            signature: "/P.O300".into()
        }
    );
    assert_eq!(
        convert(&[], &raw("DelegateProperty", signature)),
        PropertyType::Delegate {
            signature: "/P.O300".into()
        }
    );
    assert_eq!(
        convert(
            &[],
            &raw(
                "StructProperty",
                RawPayload::Struct {
                    script_struct: 0x400
                }
            )
        ),
        PropertyType::Struct {
            path: "/P.O400".into()
        }
    );
    assert_eq!(
        convert(
            &[],
            &raw("InterfaceProperty", RawPayload::Interface { class: 0x500 })
        ),
        PropertyType::Interface {
            class: "/P.O500".into()
        }
    );
    let bool_bits = RawPayload::Bool {
        field_size: 1,
        byte_offset: 0,
        byte_mask: 4,
        field_mask: 4,
    };
    assert_eq!(
        convert(&[], &raw("BoolProperty", bool_bits)),
        PropertyType::Bool {
            byte_offset: 0,
            byte_mask: 4,
            field_mask: 4,
            field_size: 1
        }
    );
}

#[test]
fn a_reference_that_does_not_resolve_makes_the_property_unknown() {
    // No path, an empty path, and a null reference.
    for target in [0x666, 0x777, 0] {
        let object = raw("ObjectProperty", RawPayload::Object { class: target });
        assert_eq!(
            convert(&[], &object),
            unknown("ObjectProperty"),
            "{target:#x}"
        );
        let class = raw(
            "ClassProperty",
            RawPayload::Class {
                class: 0x100,
                meta: target,
            },
        );
        assert_eq!(
            convert(&[], &class),
            unknown("ClassProperty"),
            "{target:#x}"
        );
    }
    // A payload that could not be read, a class that is not known, and a
    // payload that does not belong to the class.
    assert_eq!(
        convert(&[], &raw("ObjectProperty", RawPayload::Unknown)),
        unknown("ObjectProperty")
    );
    assert_eq!(
        convert(&[], &raw("FooProperty", RawPayload::Scalar)),
        unknown("FooProperty")
    );
    assert_eq!(
        convert(
            &[],
            &raw("ObjectProperty", RawPayload::Array { inner: 0x100 })
        ),
        unknown("ObjectProperty")
    );
    assert_eq!(
        convert(&[], &raw("BoolProperty", RawPayload::Scalar)),
        unknown("BoolProperty")
    );
}

#[test]
fn containers_follow_their_inner_properties() {
    let int = raw("IntProperty", RawPayload::Scalar);
    let object = raw("ObjectProperty", RawPayload::Object { class: 0x100 });
    let props = [(0x10, int.clone()), (0x20, object.clone())];
    assert_eq!(
        convert(
            &props,
            &raw("ArrayProperty", RawPayload::Array { inner: 0x10 })
        ),
        PropertyType::Array {
            inner: Box::new(PropertyType::Int32)
        }
    );
    assert_eq!(
        convert(
            &props,
            &raw("SetProperty", RawPayload::Set { element: 0x20 })
        ),
        PropertyType::Set {
            element: Box::new(PropertyType::Object {
                class: "/P.O100".into()
            })
        }
    );
    assert_eq!(
        convert(
            &props,
            &raw(
                "MapProperty",
                RawPayload::Map {
                    key: 0x10,
                    value: 0x20
                }
            )
        ),
        PropertyType::Map {
            key: Box::new(PropertyType::Int32),
            value: Box::new(PropertyType::Object {
                class: "/P.O100".into()
            }),
        }
    );
    assert_eq!(
        convert(
            &props,
            &raw("OptionalProperty", RawPayload::Optional { inner: 0x10 })
        ),
        PropertyType::Optional {
            inner: Box::new(PropertyType::Int32)
        }
    );
    // An inner property that cannot be decoded, is null, or does not
    // resolve makes the whole property unknown, sized by the outer one.
    for inner in [0x99, 0] {
        assert_eq!(
            convert(&props, &raw("ArrayProperty", RawPayload::Array { inner })),
            unknown("ArrayProperty")
        );
    }
    let bad_inner = [(
        0x30,
        raw("ObjectProperty", RawPayload::Object { class: 0x666 }),
    )];
    assert_eq!(
        convert(
            &bad_inner,
            &raw(
                "MapProperty",
                RawPayload::Map {
                    key: 0x30,
                    value: 0x30
                }
            )
        ),
        unknown("MapProperty")
    );
}

#[test]
fn bytes_and_enums_distinguish_plain_bytes_from_enum_bytes() {
    let plain = raw("ByteProperty", RawPayload::Byte { enum_object: 0 });
    assert_eq!(convert(&[], &plain), PropertyType::UInt8);
    let with_enum = raw("ByteProperty", RawPayload::Byte { enum_object: 0x100 });
    assert_eq!(
        convert(&[], &with_enum),
        PropertyType::Byte {
            path: "/P.O100".into()
        }
    );
    assert_eq!(
        convert(
            &[],
            &raw("ByteProperty", RawPayload::Byte { enum_object: 0x666 })
        ),
        unknown("ByteProperty")
    );
    assert_eq!(
        convert(&[], &raw("ByteProperty", RawPayload::Unknown)),
        unknown("ByteProperty")
    );
    let props = [(0x10, plain)];
    let enum_property = raw(
        "EnumProperty",
        RawPayload::Enum {
            underlying: 0x10,
            enum_object: 0x200,
        },
    );
    assert_eq!(
        convert(&props, &enum_property),
        PropertyType::Enum {
            path: "/P.O200".into(),
            underlying: Box::new(PropertyType::UInt8)
        }
    );
    let no_enum = raw(
        "EnumProperty",
        RawPayload::Enum {
            underlying: 0x10,
            enum_object: 0,
        },
    );
    assert_eq!(convert(&props, &no_enum), unknown("EnumProperty"));
}

fn nested_arrays(depth: usize) -> (Vec<(Addr, RawProperty)>, RawProperty) {
    // Array<Array<... Int>> with `depth` arrays.
    let mut props = vec![(0x1000, raw("IntProperty", RawPayload::Scalar))];
    let mut inner = 0x1000;
    for level in 1..depth {
        let address = 0x1000 + level as u64;
        props.push((address, raw("ArrayProperty", RawPayload::Array { inner })));
        inner = address;
    }
    (props, raw("ArrayProperty", RawPayload::Array { inner }))
}

#[test]
fn nesting_is_followed_to_the_depth_limit_and_a_loop_ends_there() {
    let (props, top) = nested_arrays(MAX_PROPERTY_DEPTH);
    let mut ty = convert(&props, &top);
    let mut arrays = 0;
    while let PropertyType::Array { inner } = ty {
        arrays += 1;
        ty = *inner;
    }
    assert_eq!((arrays, ty), (MAX_PROPERTY_DEPTH, PropertyType::Int32));
    let (props, top) = nested_arrays(MAX_PROPERTY_DEPTH + 1);
    assert_eq!(convert(&props, &top), unknown("ArrayProperty"));
    // An array whose inner property is itself.
    let looped = raw("ArrayProperty", RawPayload::Array { inner: 0x50 });
    assert_eq!(
        convert(&[(0x50, looped.clone())], &looped),
        unknown("ArrayProperty")
    );
}

/// Sets every typed offset to `None`, or to an address no memory covers.
fn with_typed(fx: &Fixture, typed: TypedPayloadLayout) -> Fixture {
    let mut fx = fx.clone();
    fx.layout.properties.typed = typed;
    fx
}

fn typed_none() -> TypedPayloadLayout {
    TypedPayloadLayout {
        bool_property: None,
        object_class: None,
        class_meta: None,
        script_struct: None,
        array_inner: None,
        set_element: None,
        map_key: None,
        map_value: None,
        enum_underlying: None,
        enum_type: None,
        byte_enum: None,
        delegate_signature: None,
        interface_class: None,
        optional_value: None,
    }
}

fn typed_unreadable() -> TypedPayloadLayout {
    let far = Some(0x4000_0000);
    TypedPayloadLayout {
        bool_property: None,
        object_class: far,
        class_meta: far,
        script_struct: far,
        array_inner: far,
        set_element: far,
        map_key: far,
        map_value: far,
        enum_underlying: far,
        enum_type: far,
        byte_enum: far,
        delegate_signature: far,
        interface_class: far,
        optional_value: far,
    }
}

fn is_scalar(ty: &PropertyType) -> bool {
    matches!(
        ty,
        PropertyType::Int8
            | PropertyType::Int16
            | PropertyType::Int32
            | PropertyType::Int64
            | PropertyType::UInt16
            | PropertyType::UInt32
            | PropertyType::UInt64
            | PropertyType::Float
            | PropertyType::Double
            | PropertyType::Name
            | PropertyType::Str
            | PropertyType::Text
            | PropertyType::FieldPath
    )
}

#[test]
fn a_missing_or_unreadable_payload_leaves_the_field_usable_but_unknown() {
    for profile in [
        Profile::ue4_21_measured(),
        Profile::ue4_25_case_preserving_measured(),
        Profile::ue5_case_preserving_assumed(),
    ] {
        let base = small(profile);
        for (how, typed) in [
            ("no offsets", typed_none()),
            ("unreadable", typed_unreadable()),
        ] {
            let fx = with_typed(&base, typed);
            let label = format!("{} {how}", fx.profile.name);
            let walked = walk(&fx, None);
            let mut checked = 0;
            for def in &fx.truth.structs {
                let found = &walked.structs[&def.path];
                assert!(found.fields.errors.is_empty(), "{label}: {}", def.path);
                assert_eq!(found.fields.raw.len(), def.fields.len(), "{label}");
                for raw in &found.fields.raw {
                    let truth = def.fields.iter().find(|f| f.name == raw.name).unwrap();
                    // Offset, size, dimension and flags never depend on the payload.
                    assert_eq!(
                        (raw.offset, raw.element_size, raw.array_dim, raw.flags),
                        (
                            truth.offset,
                            truth.element_size,
                            truth.array_dim,
                            truth.flags
                        ),
                        "{label}: {}.{}",
                        def.path,
                        raw.name
                    );
                    checked += 1;
                }
                for (raw, ty) in found.fields.raw.iter().zip(&found.fields.types) {
                    let truth = def.fields.iter().find(|f| f.name == raw.name).unwrap();
                    if is_scalar(&truth.ty) {
                        assert_eq!(ty, &truth.ty, "{label}: {}", raw.name);
                    } else {
                        assert!(
                            matches!(ty, PropertyType::Unknown { type_name } if type_name == &raw.class_name),
                            "{label}: {} became {ty:?}",
                            raw.name
                        );
                        assert_eq!(raw.payload, RawPayload::Unknown, "{label}: {}", raw.name);
                    }
                }
                // Function parameters too.
                for function in &found.functions {
                    assert!(function.params.errors.is_empty(), "{label}");
                }
            }
            assert!(checked > 80, "{label}: {checked}");
        }
    }
}
