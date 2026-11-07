//! Property class names, in every era, mapped to one set of kinds.

/// What a property class stores, independent of the era's class name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PropertyKind {
    /// `Int8Property`.
    Int8,
    /// `Int16Property`.
    Int16,
    /// `IntProperty`.
    Int32,
    /// `Int64Property`.
    Int64,
    /// `UInt16Property`.
    UInt16,
    /// `UInt32Property`.
    UInt32,
    /// `UInt64Property`.
    UInt64,
    /// `FloatProperty`.
    Float,
    /// `DoubleProperty`.
    Double,
    /// `ByteProperty`, with an optional enum.
    Byte,
    /// `BoolProperty`.
    Bool,
    /// `NameProperty`.
    Name,
    /// `StrProperty`.
    Str,
    /// `TextProperty`.
    Text,
    /// `ObjectProperty` and `ObjectPtrProperty`.
    Object,
    /// `ClassProperty` and `ClassPtrProperty`.
    Class,
    /// `WeakObjectProperty`.
    WeakObject,
    /// `LazyObjectProperty`.
    LazyObject,
    /// `SoftObjectProperty`, called `AssetObjectProperty` before UE 4.15.
    SoftObject,
    /// `SoftClassProperty`, called `AssetClassProperty` before UE 4.15.
    SoftClass,
    /// `InterfaceProperty`.
    Interface,
    /// `StructProperty`.
    Struct,
    /// `ArrayProperty`.
    Array,
    /// `SetProperty`.
    Set,
    /// `MapProperty`.
    Map,
    /// `EnumProperty`.
    Enum,
    /// `DelegateProperty`.
    Delegate,
    /// `MulticastInlineDelegateProperty`, and `MulticastDelegateProperty`,
    /// which was the concrete inline class before UE 4.23.
    MulticastInlineDelegate,
    /// `MulticastSparseDelegateProperty`.
    MulticastSparseDelegate,
    /// `FieldPathProperty`.
    FieldPath,
    /// `OptionalProperty`.
    Optional,
    /// A class this crate does not decode.
    Unknown,
}

impl PropertyKind {
    /// Maps a property class name to its kind. Unknown names give
    /// [`PropertyKind::Unknown`].
    #[must_use]
    pub fn from_class_name(name: &str) -> Self {
        match name {
            "Int8Property" => Self::Int8,
            "Int16Property" => Self::Int16,
            "IntProperty" => Self::Int32,
            "Int64Property" => Self::Int64,
            "UInt16Property" => Self::UInt16,
            "UInt32Property" => Self::UInt32,
            "UInt64Property" => Self::UInt64,
            "FloatProperty" => Self::Float,
            "DoubleProperty" => Self::Double,
            "ByteProperty" => Self::Byte,
            "BoolProperty" => Self::Bool,
            "NameProperty" => Self::Name,
            "StrProperty" => Self::Str,
            "TextProperty" => Self::Text,
            "ObjectProperty" | "ObjectPtrProperty" => Self::Object,
            "ClassProperty" | "ClassPtrProperty" => Self::Class,
            "WeakObjectProperty" => Self::WeakObject,
            "LazyObjectProperty" => Self::LazyObject,
            "SoftObjectProperty" | "AssetObjectProperty" => Self::SoftObject,
            "SoftClassProperty" | "AssetClassProperty" => Self::SoftClass,
            "InterfaceProperty" => Self::Interface,
            "StructProperty" => Self::Struct,
            "ArrayProperty" => Self::Array,
            "SetProperty" => Self::Set,
            "MapProperty" => Self::Map,
            "EnumProperty" => Self::Enum,
            "DelegateProperty" => Self::Delegate,
            "MulticastInlineDelegateProperty" | "MulticastDelegateProperty" => {
                Self::MulticastInlineDelegate
            }
            "MulticastSparseDelegateProperty" => Self::MulticastSparseDelegate,
            "FieldPathProperty" => Self::FieldPath,
            "OptionalProperty" => Self::Optional,
            _ => Self::Unknown,
        }
    }
}
