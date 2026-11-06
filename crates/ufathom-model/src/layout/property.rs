//! Property layouts for both reflection generations.

use serde::{Deserialize, Serialize};

/// Which property system the build uses.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PropertySystem {
    /// `UProperty` objects linked through `UStruct::Children`.
    Legacy,
    /// `FProperty` fields linked through `UStruct::ChildProperties`.
    Modern {
        /// Offsets in `FField`.
        ffield: FFieldLayout,
        /// Offsets in `FFieldClass`.
        field_class: FFieldClassLayout,
    },
}

/// Fields of `FField`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FFieldLayout {
    /// Offset of `ClassPrivate`, the pointer to the `FFieldClass`.
    pub class: u32,
    /// Offset of `Next`.
    pub next: u32,
    /// Offset of `NamePrivate`.
    pub name: u32,
    /// Offset of `FlagsPrivate`. `None` when not yet found.
    pub flags: Option<u32>,
}

/// Fields of `FFieldClass`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FFieldClassLayout {
    /// Offset of the class name, such as `IntProperty`. It moved in UE 5.8.
    pub name: u32,
    /// Offset of `CastFlags`. `None` when not yet found.
    pub cast_flags: Option<u32>,
}

/// Fields of `UProperty` / `FProperty` and of the typed subclasses.
///
/// Offsets in [`typed`](Self::typed) are measured from the start of the
/// property object, in whichever system [`system`](Self::system) names.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PropertyLayout {
    /// Legacy or modern property system.
    pub system: PropertySystem,
    /// Offset of `ArrayDim`.
    pub array_dim: u32,
    /// Offset of `ElementSize`.
    pub element_size: u32,
    /// Offset of `PropertyFlags` (`EPropertyFlags`).
    pub flags: u32,
    /// Offset of `Offset_Internal`, the member offset in the owner.
    pub offset_internal: u32,
    /// `sizeof(UProperty)` / `sizeof(FProperty)`: where subclass payloads
    /// usually start.
    pub size: u32,
    /// Payload offsets of the typed subclasses.
    pub typed: TypedPayloadLayout,
}

/// Offsets of the typed payloads, each kept separately because they do not
/// all sit at [`PropertyLayout::size`] in every version. The array inner
/// property moved in UE 5.3, for example.
///
/// Every offset is optional, by design. "Refuse rather than guess" applies to
/// essential offsets: a missing property size or member offset would corrupt
/// the layout of every type. A missing payload offset corrupts nothing,
/// because sizes and offsets come from the property base. The reader decodes
/// such a field as [`PropertyType::Unknown`](crate::PropertyType::Unknown)
/// with its correct size and records a diagnostic, so one anchor missing from
/// a game does not fail the whole dump. `None` means the build has no such
/// property class or the solver found no evidence for the offset.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TypedPayloadLayout {
    /// `BoolProperty` fields.
    pub bool_property: Option<BoolPayloadLayout>,
    /// Offset of `PropertyClass` in object-like properties.
    pub object_class: Option<u32>,
    /// Offset of `MetaClass` in class properties.
    pub class_meta: Option<u32>,
    /// Offset of `Struct` in struct properties.
    pub script_struct: Option<u32>,
    /// Offset of `Inner` in array properties.
    pub array_inner: Option<u32>,
    /// Offset of `ElementProp` in set properties.
    pub set_element: Option<u32>,
    /// Offset of `KeyProp` in map properties.
    pub map_key: Option<u32>,
    /// Offset of `ValueProp` in map properties.
    pub map_value: Option<u32>,
    /// Offset of `UnderlyingProp` in enum properties.
    pub enum_underlying: Option<u32>,
    /// Offset of `Enum` in enum properties.
    pub enum_type: Option<u32>,
    /// Offset of `Enum` in byte properties.
    pub byte_enum: Option<u32>,
    /// Offset of `SignatureFunction` in delegate properties.
    pub delegate_signature: Option<u32>,
    /// Offset of `InterfaceClass` in interface properties.
    pub interface_class: Option<u32>,
    /// Offset of the value property in optional properties (UE 5.x only).
    pub optional_value: Option<u32>,
}

/// Offsets of the four `BoolProperty` fields.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BoolPayloadLayout {
    /// Offset of `FieldSize`.
    pub field_size: u32,
    /// Offset of `ByteOffset`.
    pub byte_offset: u32,
    /// Offset of `ByteMask`.
    pub byte_mask: u32,
    /// Offset of `FieldMask`.
    pub field_mask: u32,
}
