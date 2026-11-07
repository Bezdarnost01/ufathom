//! Turning a decoded property into a [`PropertyType`].

use ufathom_mem::Addr;
use ufathom_model::PropertyType;

use super::{PropertyKind, PropertySystemReader, RawPayload, RawProperty};
use crate::limits::MAX_PROPERTY_DEPTH;

/// Converts `raw` into a [`PropertyType`].
///
/// Inner properties (array element, map key and value, enum underlying type)
/// are decoded through `system` and converted in turn, to a depth of
/// [`MAX_PROPERTY_DEPTH`]. `path_of` turns the address of an object (a class,
/// struct, enum or function) into its path.
///
/// A reference that cannot be resolved, an inner property that cannot be
/// decoded, a null reference where one is required, a payload that is
/// [`RawPayload::Unknown`], a class this crate does not know and a nesting
/// deeper than the limit all give
/// [`PropertyType::Unknown`] named after the outermost property's class. The
/// result never contains an empty path.
pub fn property_type(
    system: &dyn PropertySystemReader,
    raw: &RawProperty,
    path_of: &dyn Fn(Addr) -> Option<String>,
) -> PropertyType {
    convert(system, raw, path_of, 0).unwrap_or_else(|| PropertyType::Unknown {
        type_name: raw.class_name.clone(),
    })
}

fn convert(
    system: &dyn PropertySystemReader,
    raw: &RawProperty,
    path_of: &dyn Fn(Addr) -> Option<String>,
    depth: usize,
) -> Option<PropertyType> {
    if depth > MAX_PROPERTY_DEPTH {
        return None;
    }
    let path = |addr: Addr| -> Option<String> {
        if addr == 0 {
            return None;
        }
        path_of(addr).filter(|text| !text.is_empty())
    };
    let inner = |addr: Addr| -> Option<Box<PropertyType>> {
        if addr == 0 {
            return None;
        }
        let property = system.decode(addr).ok()?;
        convert(system, &property, path_of, depth + 1).map(Box::new)
    };
    let kind = PropertyKind::from_class_name(&raw.class_name);
    Some(match (kind, raw.payload) {
        (PropertyKind::Int8, _) => PropertyType::Int8,
        (PropertyKind::Int16, _) => PropertyType::Int16,
        (PropertyKind::Int32, _) => PropertyType::Int32,
        (PropertyKind::Int64, _) => PropertyType::Int64,
        (PropertyKind::UInt16, _) => PropertyType::UInt16,
        (PropertyKind::UInt32, _) => PropertyType::UInt32,
        (PropertyKind::UInt64, _) => PropertyType::UInt64,
        (PropertyKind::Float, _) => PropertyType::Float,
        (PropertyKind::Double, _) => PropertyType::Double,
        (PropertyKind::Name, _) => PropertyType::Name,
        (PropertyKind::Str, _) => PropertyType::Str,
        (PropertyKind::Text, _) => PropertyType::Text,
        (PropertyKind::FieldPath, _) => PropertyType::FieldPath,
        (PropertyKind::Byte, RawPayload::Byte { enum_object }) => {
            if enum_object == 0 {
                PropertyType::UInt8
            } else {
                PropertyType::Byte {
                    path: path(enum_object)?,
                }
            }
        }
        (
            PropertyKind::Bool,
            RawPayload::Bool {
                field_size,
                byte_offset,
                byte_mask,
                field_mask,
            },
        ) => PropertyType::Bool {
            byte_offset,
            byte_mask,
            field_mask,
            field_size,
        },
        (PropertyKind::Object, RawPayload::Object { class }) => PropertyType::Object {
            class: path(class)?,
        },
        (PropertyKind::WeakObject, RawPayload::Object { class }) => PropertyType::WeakObject {
            class: path(class)?,
        },
        (PropertyKind::LazyObject, RawPayload::Object { class }) => PropertyType::LazyObject {
            class: path(class)?,
        },
        (PropertyKind::SoftObject, RawPayload::Object { class }) => PropertyType::SoftObject {
            class: path(class)?,
        },
        (PropertyKind::Class, RawPayload::Class { class, meta }) => PropertyType::Class {
            class: path(class)?,
            meta: path(meta)?,
        },
        (PropertyKind::SoftClass, RawPayload::Class { meta, .. }) => {
            PropertyType::SoftClass { meta: path(meta)? }
        }
        (PropertyKind::Interface, RawPayload::Interface { class }) => PropertyType::Interface {
            class: path(class)?,
        },
        (PropertyKind::Struct, RawPayload::Struct { script_struct }) => PropertyType::Struct {
            path: path(script_struct)?,
        },
        (PropertyKind::Array, RawPayload::Array { inner: element }) => PropertyType::Array {
            inner: inner(element)?,
        },
        (PropertyKind::Set, RawPayload::Set { element }) => PropertyType::Set {
            element: inner(element)?,
        },
        (PropertyKind::Map, RawPayload::Map { key, value }) => PropertyType::Map {
            key: inner(key)?,
            value: inner(value)?,
        },
        (
            PropertyKind::Enum,
            RawPayload::Enum {
                underlying,
                enum_object,
            },
        ) => PropertyType::Enum {
            path: path(enum_object)?,
            underlying: inner(underlying)?,
        },
        (PropertyKind::Delegate, RawPayload::Delegate { signature }) => PropertyType::Delegate {
            signature: path(signature)?,
        },
        (PropertyKind::MulticastInlineDelegate, RawPayload::Delegate { signature }) => {
            PropertyType::MulticastInlineDelegate {
                signature: path(signature)?,
            }
        }
        (PropertyKind::MulticastSparseDelegate, RawPayload::Delegate { signature }) => {
            PropertyType::MulticastSparseDelegate {
                signature: path(signature)?,
            }
        }
        (PropertyKind::Optional, RawPayload::Optional { inner: value }) => PropertyType::Optional {
            inner: inner(value)?,
        },
        _ => return None,
    })
}
