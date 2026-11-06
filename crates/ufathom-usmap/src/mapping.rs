//! Translation of the dump's property types into usmap property types.

use ufathom_model::PropertyType;

use crate::model::UsmapType;

/// The short name of an object path: the text after the last `.`.
///
/// `/Script/Engine.Actor` gives `Actor`. A path without a package is its own
/// short name.
#[must_use]
pub fn short_name(path: &str) -> &str {
    path.rsplit_once('.').map_or(path, |(_, name)| name)
}

impl UsmapType {
    /// The usmap type that stands for a dump type.
    ///
    /// usmap is coarser than the dump, so the mapping loses information, and
    /// each loss is deliberate:
    ///
    /// - Class pointers ([`PropertyType::Class`]) become `ObjectProperty` and
    ///   soft class pointers become `SoftObjectProperty`, since usmap has no
    ///   type for either.
    /// - Inline and sparse multicast delegates both become
    ///   `MulticastDelegateProperty`.
    /// - A [`PropertyType::Bool`] loses its bit mask, an object loses its
    ///   class and a delegate its signature. usmap has no place for them.
    /// - A [`PropertyType::Byte`] backed by an enum becomes an
    ///   `EnumProperty` over `ByteProperty`, which is how usmap spells it, so
    ///   it reads back the same as an [`PropertyType::Enum`] over
    ///   [`PropertyType::UInt8`].
    /// - A [`PropertyType::Unknown`] becomes the `Unknown` tag.
    /// - Struct and enum references keep only their short name.
    #[must_use]
    pub fn from_property_type(ty: &PropertyType) -> Self {
        match ty {
            PropertyType::Int8 => Self::Int8,
            PropertyType::Int16 => Self::Int16,
            PropertyType::Int32 => Self::Int32,
            PropertyType::Int64 => Self::Int64,
            PropertyType::UInt8 => Self::Byte,
            PropertyType::UInt16 => Self::UInt16,
            PropertyType::UInt32 => Self::UInt32,
            PropertyType::UInt64 => Self::UInt64,
            PropertyType::Float => Self::Float,
            PropertyType::Double => Self::Double,
            PropertyType::Bool { .. } => Self::Bool,
            PropertyType::Name => Self::Name,
            PropertyType::Str => Self::Str,
            PropertyType::Text => Self::Text,
            PropertyType::Object { .. } | PropertyType::Class { .. } => Self::Object,
            PropertyType::WeakObject { .. } => Self::WeakObject,
            PropertyType::LazyObject { .. } => Self::LazyObject,
            PropertyType::SoftObject { .. } | PropertyType::SoftClass { .. } => Self::SoftObject,
            PropertyType::Interface { .. } => Self::Interface,
            PropertyType::Struct { path } => Self::Struct {
                name: short_name(path).to_owned(),
            },
            PropertyType::Array { inner } => Self::Array {
                inner: Box::new(Self::from_property_type(inner)),
            },
            PropertyType::Set { element } => Self::Set {
                element: Box::new(Self::from_property_type(element)),
            },
            PropertyType::Map { key, value } => Self::Map {
                key: Box::new(Self::from_property_type(key)),
                value: Box::new(Self::from_property_type(value)),
            },
            PropertyType::Enum { path, underlying } => Self::Enum {
                inner: Box::new(Self::from_property_type(underlying)),
                name: short_name(path).to_owned(),
            },
            PropertyType::Byte { path } => Self::Enum {
                inner: Box::new(Self::Byte),
                name: short_name(path).to_owned(),
            },
            PropertyType::Delegate { .. } => Self::Delegate,
            PropertyType::MulticastInlineDelegate { .. }
            | PropertyType::MulticastSparseDelegate { .. } => Self::MulticastDelegate,
            PropertyType::FieldPath => Self::FieldPath,
            PropertyType::Optional { inner } => Self::Optional {
                inner: Box::new(Self::from_property_type(inner)),
            },
            PropertyType::Unknown { .. } => Self::Unknown,
        }
    }

    /// Whether this type or any type nested in it is [`UsmapType::Unknown`].
    #[must_use]
    pub fn contains_unknown(&self) -> bool {
        match self {
            Self::Unknown => true,
            Self::Array { inner } | Self::Optional { inner } | Self::Enum { inner, .. } => {
                inner.contains_unknown()
            }
            Self::Set { element } => element.contains_unknown(),
            Self::Map { key, value } => key.contains_unknown() || value.contains_unknown(),
            _ => false,
        }
    }
}
