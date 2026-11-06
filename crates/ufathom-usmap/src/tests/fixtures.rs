//! Builders shared by the tests: dumps, usmap models and hand-written bytes.

use ufathom_model::layout::{
    EngineLayout, FNameLayout, Location, NameArrayLayout, NameEntryArrayLayout, NameTableLayout,
    ObjectArrayKind, ObjectArrayLayout, PropertyLayout, PropertySystem, TypedPayloadLayout,
    UClassLayout, UEnumLayout, UFieldLayout, UFunctionLayout, UObjectLayout, UStructLayout,
};
use ufathom_model::{
    Dump, EngineInfo, EnumDef, EnumValue, FieldDef, FunctionDef, PropertyType, StructDef,
    StructKind, TargetInfo, ToolInfo,
};

use crate::{
    Compression, Usmap, UsmapEnum, UsmapEnumValue, UsmapProperty, UsmapStruct, UsmapType,
    UsmapVersion,
};

fn layout() -> EngineLayout {
    EngineLayout {
        pointer_size: 8,
        names: NameTableLayout::Array(NameArrayLayout {
            location: Location::Absolute(0x1000),
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
            location: Location::Absolute(0x2000),
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
            typed: TypedPayloadLayout {
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
            },
        },
    }
}

/// An empty dump.
pub fn empty_dump() -> Dump {
    Dump::new(
        ToolInfo {
            name: "ufathom".to_owned(),
            version: "0.1.0".to_owned(),
        },
        TargetInfo {
            module: "Game-Win64-Shipping.exe".to_owned(),
            build_id: "build-0001".to_owned(),
        },
        EngineInfo::default(),
        layout(),
    )
}

/// A dump with exactly these structs and enums.
pub fn dump_of(structs: Vec<StructDef>, enums: Vec<EnumDef>) -> Dump {
    let mut dump = empty_dump();
    dump.structs = structs;
    dump.enums = enums;
    dump
}

/// A class in `/Script/Test`, with the parent given by short name.
pub fn class(name: &str, parent: Option<&str>) -> StructDef {
    StructDef {
        path: format!("/Script/Test.{name}"),
        name: name.to_owned(),
        package: "/Script/Test".to_owned(),
        kind: StructKind::Class,
        super_path: parent.map(|parent| format!("/Script/Test.{parent}")),
        size: 0x100,
        alignment: 8,
        fields: Vec::new(),
        functions: Vec::new(),
        flags: 0,
    }
}

/// A field of 4 bytes.
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

/// A class with these fields, laid out at consecutive offsets.
pub fn class_with(name: &str, parent: Option<&str>, fields: &[(&str, PropertyType)]) -> StructDef {
    let mut def = class(name, parent);
    def.fields = fields
        .iter()
        .enumerate()
        .map(|(position, (field_name, ty))| {
            field(
                field_name,
                0x40 + u32::try_from(position).unwrap_or(0) * 8,
                ty.clone(),
            )
        })
        .collect();
    def
}

/// An enum in `/Script/Test`.
pub fn enumeration(name: &str, values: &[(&str, i64)]) -> EnumDef {
    EnumDef {
        path: format!("/Script/Test.{name}"),
        name: name.to_owned(),
        package: "/Script/Test".to_owned(),
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

/// A function, to check that functions are reported as omitted.
pub fn function(owner: &str, name: &str) -> FunctionDef {
    FunctionDef {
        path: format!("/Script/Test.{owner}:{name}"),
        name: name.to_owned(),
        flags: 0,
        params_size: 0,
        params: Vec::new(),
        native_rva: None,
    }
}

/// One value of every [`PropertyType`] variant, with the usmap type each must
/// become. The expectations are written out by hand, not derived from the
/// mapping function.
pub fn every_property_type() -> Vec<(PropertyType, UsmapType)> {
    let path = |text: &str| text.to_owned();
    let boxed = |ty: UsmapType| Box::new(ty);
    vec![
        (PropertyType::Int8, UsmapType::Int8),
        (PropertyType::Int16, UsmapType::Int16),
        (PropertyType::Int32, UsmapType::Int32),
        (PropertyType::Int64, UsmapType::Int64),
        (PropertyType::UInt8, UsmapType::Byte),
        (PropertyType::UInt16, UsmapType::UInt16),
        (PropertyType::UInt32, UsmapType::UInt32),
        (PropertyType::UInt64, UsmapType::UInt64),
        (PropertyType::Float, UsmapType::Float),
        (PropertyType::Double, UsmapType::Double),
        (
            PropertyType::Bool {
                byte_offset: 1,
                byte_mask: 4,
                field_mask: 4,
                field_size: 1,
            },
            UsmapType::Bool,
        ),
        (PropertyType::Name, UsmapType::Name),
        (PropertyType::Str, UsmapType::Str),
        (PropertyType::Text, UsmapType::Text),
        (
            PropertyType::Object {
                class: path("/Script/Engine.Actor"),
            },
            UsmapType::Object,
        ),
        (
            PropertyType::Class {
                class: path("/Script/CoreUObject.Class"),
                meta: path("/Script/Engine.Actor"),
            },
            UsmapType::Object,
        ),
        (
            PropertyType::WeakObject {
                class: path("/Script/Engine.Pawn"),
            },
            UsmapType::WeakObject,
        ),
        (
            PropertyType::LazyObject {
                class: path("/Script/Engine.Pawn"),
            },
            UsmapType::LazyObject,
        ),
        (
            PropertyType::SoftObject {
                class: path("/Script/Engine.Texture"),
            },
            UsmapType::SoftObject,
        ),
        (
            PropertyType::SoftClass {
                meta: path("/Script/Engine.Actor"),
            },
            UsmapType::SoftObject,
        ),
        (
            PropertyType::Interface {
                class: path("/Script/Engine.Interface_Test"),
            },
            UsmapType::Interface,
        ),
        (
            PropertyType::Struct {
                path: path("/Script/CoreUObject.Vector"),
            },
            UsmapType::Struct {
                name: path("Vector"),
            },
        ),
        (
            PropertyType::Array {
                inner: Box::new(PropertyType::Int32),
            },
            UsmapType::Array {
                inner: boxed(UsmapType::Int32),
            },
        ),
        (
            PropertyType::Set {
                element: Box::new(PropertyType::Name),
            },
            UsmapType::Set {
                element: boxed(UsmapType::Name),
            },
        ),
        (
            PropertyType::Map {
                key: Box::new(PropertyType::Name),
                value: Box::new(PropertyType::Array {
                    inner: Box::new(PropertyType::Float),
                }),
            },
            UsmapType::Map {
                key: boxed(UsmapType::Name),
                value: boxed(UsmapType::Array {
                    inner: boxed(UsmapType::Float),
                }),
            },
        ),
        (
            PropertyType::Enum {
                path: path("/Script/Engine.EMovementMode"),
                underlying: Box::new(PropertyType::UInt8),
            },
            UsmapType::Enum {
                inner: boxed(UsmapType::Byte),
                name: path("EMovementMode"),
            },
        ),
        (
            PropertyType::Byte {
                path: path("/Script/Engine.ENetRole"),
            },
            UsmapType::Enum {
                inner: boxed(UsmapType::Byte),
                name: path("ENetRole"),
            },
        ),
        (
            PropertyType::Delegate {
                signature: path("/Script/Engine.Foo__DelegateSignature"),
            },
            UsmapType::Delegate,
        ),
        (
            PropertyType::MulticastInlineDelegate {
                signature: path("/Script/Engine.Bar__DelegateSignature"),
            },
            UsmapType::MulticastDelegate,
        ),
        (
            PropertyType::MulticastSparseDelegate {
                signature: path("/Script/Engine.Baz__DelegateSignature"),
            },
            UsmapType::MulticastDelegate,
        ),
        (PropertyType::FieldPath, UsmapType::FieldPath),
        (
            PropertyType::Optional {
                inner: Box::new(PropertyType::Double),
            },
            UsmapType::Optional {
                inner: boxed(UsmapType::Double),
            },
        ),
        (
            PropertyType::Unknown {
                type_name: path("FooProperty"),
            },
            UsmapType::Unknown,
        ),
    ]
}

/// Position of a variant. The `match` has no wildcard, so adding a variant to
/// [`PropertyType`] stops this from compiling until the fixtures cover it.
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

/// A usmap model with these contents, version 4.
pub fn usmap_of(structs: Vec<UsmapStruct>, enums: Vec<UsmapEnum>) -> Usmap {
    Usmap {
        version: UsmapVersion::LATEST,
        compression: Compression::None,
        version_info: None,
        names: Vec::new(),
        enums,
        structs,
        extension_bytes: 0,
    }
}

/// A usmap struct with these properties, at consecutive indices.
pub fn usmap_struct(name: &str, parent: Option<&str>, props: &[(&str, UsmapType)]) -> UsmapStruct {
    let properties: Vec<UsmapProperty> = props
        .iter()
        .enumerate()
        .map(|(position, (prop_name, ty))| UsmapProperty {
            name: (*prop_name).to_owned(),
            index: u16::try_from(position).unwrap_or(0),
            array_dim: 1,
            ty: ty.clone(),
        })
        .collect();
    UsmapStruct {
        name: name.to_owned(),
        super_name: parent.map(str::to_owned),
        property_count: u16::try_from(properties.len()).unwrap_or(0),
        properties,
    }
}

/// A usmap enum with these values.
pub fn usmap_enum(name: &str, values: &[(&str, i64)]) -> UsmapEnum {
    UsmapEnum {
        name: name.to_owned(),
        values: values
            .iter()
            .map(|&(value_name, value)| UsmapEnumValue {
                name: value_name.to_owned(),
                value,
            })
            .collect(),
    }
}

/// Little-endian byte writer for hand-built files.
#[derive(Default)]
pub struct Bytes(pub Vec<u8>);

impl Bytes {
    pub fn u8(mut self, value: u8) -> Self {
        self.0.push(value);
        self
    }

    pub fn u16(mut self, value: u16) -> Self {
        self.0.extend_from_slice(&value.to_le_bytes());
        self
    }

    pub fn u32(mut self, value: u32) -> Self {
        self.0.extend_from_slice(&value.to_le_bytes());
        self
    }

    pub fn i32(mut self, value: i32) -> Self {
        self.0.extend_from_slice(&value.to_le_bytes());
        self
    }

    pub fn i64(mut self, value: i64) -> Self {
        self.0.extend_from_slice(&value.to_le_bytes());
        self
    }

    pub fn raw(mut self, bytes: &[u8]) -> Self {
        self.0.extend_from_slice(bytes);
        self
    }
}

/// A header up to and including the compression method, for `version`.
/// `version_block` is the bytes after the version byte for version 1 and up.
pub fn header_prefix(version: u8, version_block: &[u8], method: u8) -> Bytes {
    let mut bytes = Bytes::default().u16(0x30C4).u8(version);
    if version >= 1 {
        bytes = bytes.raw(version_block);
    }
    bytes.u8(method)
}

/// A complete file: header with the given sizes, then `payload`.
pub fn file(prefix: Bytes, compressed: u32, decompressed: u32, payload: &[u8]) -> Vec<u8> {
    prefix.u32(compressed).u32(decompressed).raw(payload).0
}
