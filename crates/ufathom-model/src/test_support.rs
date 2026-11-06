//! Builders shared by the unit tests. Compiled only for tests.
//!
//! The addresses and RVAs here are made up. The structural offsets follow the
//! calibration table in `ARCHITECTURE.md`.

use crate::dump::{
    Diagnostic, Dump, EngineInfo, EngineVersion, EnumDef, EnumValue, FieldDef, FunctionDef,
    PropertyType, Severity, StructDef, StructKind, TargetInfo, ToolInfo,
};
use crate::flags::{CPF_OUT_PARM, CPF_PARM, CPF_RETURN_PARM};
use crate::layout::{
    BoolPayloadLayout, EngineLayout, FFieldClassLayout, FFieldLayout, FNameLayout, Location,
    NameArrayLayout, NameEntryArrayLayout, NamePoolLayout, NameTableLayout, ObjectArrayKind,
    ObjectArrayLayout, PoolEntryLayout, PropertyLayout, PropertySystem, TypedPayloadLayout,
    UClassLayout, UEnumLayout, UFieldLayout, UFunctionLayout, UObjectLayout, UStructLayout,
};

const MODULE: &str = "Game-Win64-Shipping.exe";

fn rva(rva: u64) -> Location {
    Location::Rva {
        module: MODULE.to_owned(),
        rva,
    }
}

fn typed_legacy(base: u32) -> TypedPayloadLayout {
    TypedPayloadLayout {
        bool_property: Some(BoolPayloadLayout {
            field_size: base,
            byte_offset: base + 1,
            byte_mask: base + 2,
            field_mask: base + 3,
        }),
        object_class: Some(base),
        class_meta: Some(base + 8),
        script_struct: Some(base),
        array_inner: Some(base),
        set_element: Some(base),
        map_key: Some(base),
        map_value: Some(base + 8),
        enum_underlying: Some(base),
        enum_type: Some(base + 8),
        byte_enum: Some(base),
        delegate_signature: Some(base),
        interface_class: Some(base),
        optional_value: None,
    }
}

/// DBD 2.6.4 shape: 8-byte `FName`, outer at 0x20, legacy properties, array
/// name table.
pub fn layout_array_names_legacy() -> EngineLayout {
    EngineLayout {
        pointer_size: 8,
        names: NameTableLayout::Array(NameArrayLayout {
            location: rva(0x1000),
            indirect: true,
            num_elements: 0x10_0000,
            num_chunks: 0x10_0004,
            elements_per_chunk: 0x4000,
            entry: NameEntryArrayLayout {
                index_offset: 0,
                string_offset: 0x10,
                wide_mask: 1,
            },
        }),
        objects: ObjectArrayLayout {
            location: rva(0x2000),
            kind: ObjectArrayKind::Chunked {
                elements_per_chunk: 0x10000,
            },
            objects: 0,
            max_elements: 0x10,
            num_elements: 0x14,
            max_chunks: Some(0x18),
            num_chunks: Some(0x1C),
            item_size: 0x18,
            item_object: 0,
            item_serial: Some(0x10),
            pointer_transform: Vec::new(),
        },
        fname: FNameLayout {
            size: 8,
            comparison_index: 0,
            display_index: None,
            number: Some(4),
        },
        uobject: UObjectLayout {
            flags: 8,
            index: 0xC,
            class: 0x10,
            name: 0x18,
            outer: 0x20,
            size: 0x28,
        },
        ufield: UFieldLayout { next: 0x28 },
        ustruct: UStructLayout {
            super_struct: 0x30,
            children: 0x38,
            child_properties: None,
            properties_size: 0x40,
            min_alignment: 0x44,
            min_alignment_width: 4,
        },
        uclass: UClassLayout {
            cast_flags: None,
            default_object: None,
        },
        ufunction: UFunctionLayout {
            flags: 0x88,
            native_func: Some(0xB0),
        },
        uenum: UEnumLayout::PairArray {
            offset: 0x40,
            stride: 0x10,
            value_offset: 8,
            value_width: 8,
        },
        properties: PropertyLayout {
            system: PropertySystem::Legacy,
            array_dim: 0x30,
            element_size: 0x34,
            flags: 0x38,
            offset_internal: 0x44,
            size: 0x70,
            typed: typed_legacy(0x70),
        },
    }
}

/// DBD 3.7.2 shape: 12-byte `FName` with outer at 0x28, legacy properties.
pub fn layout_array_names_wide_fname() -> EngineLayout {
    let mut layout = layout_array_names_legacy();
    layout.fname = FNameLayout {
        size: 12,
        comparison_index: 0,
        display_index: Some(4),
        number: Some(8),
    };
    layout.uobject = UObjectLayout {
        flags: 8,
        index: 0xC,
        class: 0x10,
        name: 0x18,
        outer: 0x28,
        size: 0x30,
    };
    layout.ufield = UFieldLayout { next: 0x30 };
    layout.ustruct = UStructLayout {
        super_struct: 0x48,
        children: 0x50,
        child_properties: None,
        properties_size: 0x58,
        min_alignment: 0x5C,
        min_alignment_width: 4,
    };
    layout.uclass = UClassLayout {
        cast_flags: Some(0xC0),
        default_object: Some(0x108),
    };
    layout.ufunction = UFunctionLayout {
        flags: 0xA0,
        native_func: Some(0xC8),
    };
    layout.uenum = UEnumLayout::PairArray {
        offset: 0x48,
        stride: 0x10,
        value_offset: 8,
        value_width: 8,
    };
    layout.properties = PropertyLayout {
        system: PropertySystem::Legacy,
        array_dim: 0x38,
        element_size: 0x3C,
        flags: 0x40,
        offset_internal: 0x4C,
        size: 0x80,
        typed: typed_legacy(0x80),
    };
    layout
}

/// DBD 4.4.2 shape: pool names with header +4, string +6, shift 1, stride 4,
/// and `FField` properties.
pub fn layout_pool_modern_dbd() -> EngineLayout {
    let mut layout = layout_array_names_wide_fname();
    layout.names = NameTableLayout::Pool(NamePoolLayout {
        location: rva(0x3000),
        blocks: 0x10,
        current_block: 0x8,
        cursor: 0xC,
        stride: 4,
        block_offset_bits: 16,
        entry: PoolEntryLayout {
            header_offset: 4,
            string_offset: 6,
            len_shift: 1,
            wide_mask: 1,
        },
    });
    layout.ustruct.child_properties = Some(0x58);
    layout.ustruct.properties_size = 0x68;
    layout.ustruct.min_alignment = 0x6C;
    layout.properties = PropertyLayout {
        system: PropertySystem::Modern {
            ffield: FFieldLayout {
                class: 8,
                next: 0x20,
                name: 0x28,
                flags: Some(0x30),
            },
            field_class: FFieldClassLayout {
                name: 0,
                cast_flags: Some(0x10),
            },
        },
        array_dim: 0x38,
        element_size: 0x3C,
        flags: 0x40,
        offset_internal: 0x4C,
        size: 0x78,
        typed: typed_legacy(0x78),
    };
    layout
}

/// A stock UE5 shape: pool names with header 0, string 2, shift 6, stride 2,
/// outline numbers (no `FName::Number`), 16-bit `MinAlignment`, split enum
/// arrays, a scrambled object array and the array inner moved.
pub fn layout_pool_stock_ue5() -> EngineLayout {
    let mut layout = layout_pool_modern_dbd();
    layout.names = NameTableLayout::Pool(NamePoolLayout {
        location: Location::Absolute(0x1_4000_0000),
        blocks: 0x10,
        current_block: 0x8,
        cursor: 0xC,
        stride: 2,
        block_offset_bits: 16,
        entry: PoolEntryLayout {
            header_offset: 0,
            string_offset: 2,
            len_shift: 6,
            wide_mask: 1,
        },
    });
    layout.fname = FNameLayout {
        size: 8,
        comparison_index: 0,
        display_index: None,
        number: None,
    };
    layout.ustruct.min_alignment_width = 2;
    layout.uenum = UEnumLayout::SplitArrays {
        names: 0x40,
        values: 0x48,
        count: 0x50,
    };
    layout.objects.pointer_transform = vec![
        crate::layout::TransformOp::Xor(0xA5A5_A5A5_A5A5_A5A5),
        crate::layout::TransformOp::RotateLeft(7),
        crate::layout::TransformOp::Not,
    ];
    if let PropertySystem::Modern { field_class, .. } = &mut layout.properties.system {
        field_class.name = 0x18;
    }
    layout.properties.typed.array_inner = Some(0x80);
    layout.properties.typed.optional_value = Some(0x78);
    layout
}

/// A dump with an empty content section.
pub fn empty_dump() -> Dump {
    Dump::new(
        ToolInfo {
            name: "ufathom".to_owned(),
            version: "0.1.0".to_owned(),
        },
        TargetInfo {
            module: MODULE.to_owned(),
            build_id: "build-0001".to_owned(),
        },
        EngineInfo {
            claimed: Some("4.25.1".to_owned()),
            build: Some("++UE4+Release-4.25-CL-377698".to_owned()),
            min_version: Some(EngineVersion {
                major: 4,
                minor: 25,
                patch: 0,
            }),
            max_version: None,
            evidence: vec!["FField present".to_owned(), "FNamePool present".to_owned()],
        },
        layout_pool_modern_dbd(),
    )
}

/// A field of 4 bytes at `offset`.
pub fn field(name: &str, offset: u32, ty: PropertyType) -> FieldDef {
    FieldDef {
        name: name.to_owned(),
        offset,
        ty,
        element_size: 4,
        array_dim: 1,
        flags: 0,
    }
}

/// A function parameter of 4 bytes at `offset`.
pub fn param(name: &str, offset: u32, ty: PropertyType, extra_flags: u64) -> FieldDef {
    FieldDef {
        flags: CPF_PARM | extra_flags,
        ..field(name, offset, ty)
    }
}

/// The return value of a function.
pub fn return_value(offset: u32, ty: PropertyType) -> FieldDef {
    param("ReturnValue", offset, ty, CPF_OUT_PARM | CPF_RETURN_PARM)
}

/// A function whose `params_size` is the end of its last parameter. Real
/// dumps read it from the function's `PropertiesSize` instead.
pub fn function(owner_path: &str, name: &str, flags: u32, params: Vec<FieldDef>) -> FunctionDef {
    let params_size = params
        .iter()
        .map(|param| param.offset + param.element_size * param.array_dim)
        .max()
        .unwrap_or(0);
    FunctionDef {
        path: format!("{owner_path}:{name}"),
        name: name.to_owned(),
        flags,
        params_size,
        params,
        native_rva: None,
    }
}

/// A class named `name` in `package`, with no members.
pub fn class(package: &str, name: &str, super_path: Option<&str>, size: u32) -> StructDef {
    StructDef {
        path: format!("{package}.{name}"),
        name: name.to_owned(),
        package: package.to_owned(),
        kind: StructKind::Class,
        super_path: super_path.map(str::to_owned),
        size,
        alignment: 8,
        fields: Vec::new(),
        functions: Vec::new(),
        flags: 0,
    }
}

/// An enum with 1-byte underlying type.
pub fn enumeration(package: &str, name: &str, values: &[(&str, i64)]) -> EnumDef {
    EnumDef {
        path: format!("{package}.{name}"),
        name: name.to_owned(),
        package: package.to_owned(),
        underlying_size: 1,
        values: values
            .iter()
            .map(|&(value_name, value)| EnumValue {
                name: value_name.to_owned(),
                value,
            })
            .collect(),
    }
}

/// An empty dump holding exactly these structs and enums.
pub fn dump_of(structs: Vec<StructDef>, enums: Vec<EnumDef>) -> Dump {
    let mut dump = empty_dump();
    dump.structs = structs;
    dump.enums = enums;
    dump
}

/// One value of every [`PropertyType`] variant.
pub fn every_property_type() -> Vec<PropertyType> {
    let path = |text: &str| text.to_owned();
    vec![
        PropertyType::Int8,
        PropertyType::Int16,
        PropertyType::Int32,
        PropertyType::Int64,
        PropertyType::UInt8,
        PropertyType::UInt16,
        PropertyType::UInt32,
        PropertyType::UInt64,
        PropertyType::Float,
        PropertyType::Double,
        PropertyType::Bool {
            byte_offset: 1,
            byte_mask: 0x04,
            field_mask: 0x04,
            field_size: 1,
        },
        PropertyType::Name,
        PropertyType::Str,
        PropertyType::Text,
        PropertyType::Object {
            class: path("/Script/Engine.Actor"),
        },
        PropertyType::Class {
            class: path("/Script/CoreUObject.Class"),
            meta: path("/Script/Engine.Actor"),
        },
        PropertyType::WeakObject {
            class: path("/Script/Engine.Pawn"),
        },
        PropertyType::LazyObject {
            class: path("/Script/Engine.Pawn"),
        },
        PropertyType::SoftObject {
            class: path("/Script/Engine.Texture"),
        },
        PropertyType::SoftClass {
            meta: path("/Script/Engine.Actor"),
        },
        PropertyType::Interface {
            class: path("/Script/Engine.Interface_Test"),
        },
        PropertyType::Struct {
            path: path("/Script/CoreUObject.Vector"),
        },
        PropertyType::Array {
            inner: Box::new(PropertyType::Int32),
        },
        PropertyType::Set {
            element: Box::new(PropertyType::Name),
        },
        PropertyType::Map {
            key: Box::new(PropertyType::Name),
            value: Box::new(PropertyType::Array {
                inner: Box::new(PropertyType::Float),
            }),
        },
        PropertyType::Enum {
            path: path("/Script/Engine.EMovementMode"),
            underlying: Box::new(PropertyType::UInt8),
        },
        PropertyType::Byte {
            path: path("/Script/Engine.ENetRole"),
        },
        PropertyType::Delegate {
            signature: path("/Script/Engine.Foo__DelegateSignature"),
        },
        PropertyType::MulticastInlineDelegate {
            signature: path("/Script/Engine.Bar__DelegateSignature"),
        },
        PropertyType::MulticastSparseDelegate {
            signature: path("/Script/Engine.Baz__DelegateSignature"),
        },
        PropertyType::FieldPath,
        PropertyType::Optional {
            inner: Box::new(PropertyType::Double),
        },
        PropertyType::Unknown {
            type_name: path("FooProperty"),
        },
    ]
}

/// Position of a variant, using a `match` without a wildcard so that adding a
/// variant to [`PropertyType`] fails to compile here until the fixtures are
/// extended.
pub fn variant_index(ty: &PropertyType) -> usize {
    match ty {
        PropertyType::Int8 => 0,
        PropertyType::Int16 => 1,
        PropertyType::Int32 => 2,
        PropertyType::Int64 => 3,
        PropertyType::UInt8 => 4,
        PropertyType::UInt16 => 5,
        PropertyType::UInt32 => 6,
        PropertyType::UInt64 => 7,
        PropertyType::Float => 8,
        PropertyType::Double => 9,
        PropertyType::Bool { .. } => 10,
        PropertyType::Name => 11,
        PropertyType::Str => 12,
        PropertyType::Text => 13,
        PropertyType::Object { .. } => 14,
        PropertyType::Class { .. } => 15,
        PropertyType::WeakObject { .. } => 16,
        PropertyType::LazyObject { .. } => 17,
        PropertyType::SoftObject { .. } => 18,
        PropertyType::SoftClass { .. } => 19,
        PropertyType::Interface { .. } => 20,
        PropertyType::Struct { .. } => 21,
        PropertyType::Array { .. } => 22,
        PropertyType::Set { .. } => 23,
        PropertyType::Map { .. } => 24,
        PropertyType::Enum { .. } => 25,
        PropertyType::Byte { .. } => 26,
        PropertyType::Delegate { .. } => 27,
        PropertyType::MulticastInlineDelegate { .. } => 28,
        PropertyType::MulticastSparseDelegate { .. } => 29,
        PropertyType::FieldPath => 30,
        PropertyType::Optional { .. } => 31,
        PropertyType::Unknown { .. } => 32,
    }
}

/// Number of [`PropertyType`] variants, matching [`variant_index`].
pub const PROPERTY_TYPE_VARIANTS: usize = 33;

/// A dump with several types, functions, enums and diagnostics, in a
/// deliberately unsorted order. Every [`PropertyType`] appears as a field.
pub fn busy_dump() -> Dump {
    let package = "/Script/Engine";
    let mut actor = class(package, "Actor", Some("/Script/CoreUObject.Object"), 0x300);
    actor.fields = every_property_type()
        .into_iter()
        .enumerate()
        .rev()
        .map(|(position, ty)| {
            let offset = 0x40 + u32::try_from(position).unwrap_or(0) * 8;
            field(&format!("Field{position}"), offset, ty)
        })
        .collect();
    actor.functions = vec![
        function(
            &actor.path,
            "Zeta",
            0,
            vec![param("A", 0, PropertyType::Int32, 0)],
        ),
        function(
            &actor.path,
            "Alpha",
            0x0400,
            vec![
                param("Second", 4, PropertyType::Float, 0),
                param("First", 0, PropertyType::Int32, 0),
                return_value(
                    8,
                    PropertyType::Bool {
                        byte_offset: 0,
                        byte_mask: 1,
                        field_mask: 1,
                        field_size: 1,
                    },
                ),
            ],
        ),
    ];

    let mut vector = class("/Script/CoreUObject", "Vector", None, 12);
    vector.kind = StructKind::ScriptStruct;
    vector.fields = vec![
        field("Z", 8, PropertyType::Float),
        field("X", 0, PropertyType::Float),
        field("Y", 4, PropertyType::Float),
    ];

    let mut dump = dump_of(
        vec![actor, vector],
        vec![
            enumeration(
                package,
                "ENetRole",
                &[("ROLE_Authority", 3), ("ROLE_None", 0)],
            ),
            enumeration("/Script/Core", "EAxis", &[("Z", 2), ("X", 0), ("Y", 1)]),
        ],
    );
    dump.packages = vec![
        "/Script/Engine".to_owned(),
        "/Script/CoreUObject".to_owned(),
        "/Script/Core".to_owned(),
        "/Script/Engine".to_owned(),
    ];
    dump.diagnostics = vec![
        Diagnostic {
            path: "/Script/Engine.Zed".to_owned(),
            severity: Severity::Warning,
            message: "skipped property".to_owned(),
        },
        Diagnostic {
            path: "/Script/Engine.Actor".to_owned(),
            severity: Severity::Error,
            message: "bad super chain".to_owned(),
        },
        Diagnostic {
            path: "/Script/Engine.Actor".to_owned(),
            severity: Severity::Info,
            message: "note".to_owned(),
        },
    ];
    dump
}
