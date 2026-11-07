//! Reading the typed payload of a property.

use ufathom_mem::{Addr, MemoryReader, MemoryReaderExt};
use ufathom_model::layout::TypedPayloadLayout;

use super::PropertyKind;
use super::RawPayload;
use crate::window::Window;

/// Reads the payload of the property at `field`.
///
/// Never fails. A payload offset the layout does not have, or a read that
/// fails, gives [`RawPayload::Unknown`]: the property's offset, size and
/// dimension do not depend on its payload, so the field is still usable.
pub(crate) fn read_payload(
    mem: &dyn MemoryReader,
    typed: &TypedPayloadLayout,
    kind: PropertyKind,
    field: Addr,
) -> RawPayload {
    let ptr = |offset: Option<u32>| -> Option<Addr> {
        let at = field.checked_add(u64::from(offset?))?;
        mem.read_u64(at).ok()
    };
    let payload =
        match kind {
            PropertyKind::Int8
            | PropertyKind::Int16
            | PropertyKind::Int32
            | PropertyKind::Int64
            | PropertyKind::UInt16
            | PropertyKind::UInt32
            | PropertyKind::UInt64
            | PropertyKind::Float
            | PropertyKind::Double
            | PropertyKind::Name
            | PropertyKind::Str
            | PropertyKind::Text
            | PropertyKind::FieldPath => Some(RawPayload::Scalar),
            PropertyKind::Unknown => None,
            PropertyKind::Bool => read_bool(mem, typed, field),
            PropertyKind::Byte => {
                ptr(typed.byte_enum).map(|enum_object| RawPayload::Byte { enum_object })
            }
            PropertyKind::Object
            | PropertyKind::WeakObject
            | PropertyKind::LazyObject
            | PropertyKind::SoftObject => {
                ptr(typed.object_class).map(|class| RawPayload::Object { class })
            }
            PropertyKind::Class | PropertyKind::SoftClass => ptr(typed.object_class)
                .zip(ptr(typed.class_meta))
                .map(|(class, meta)| RawPayload::Class { class, meta }),
            PropertyKind::Interface => {
                ptr(typed.interface_class).map(|class| RawPayload::Interface { class })
            }
            PropertyKind::Struct => {
                ptr(typed.script_struct).map(|script_struct| RawPayload::Struct { script_struct })
            }
            PropertyKind::Array => ptr(typed.array_inner).map(|inner| RawPayload::Array { inner }),
            PropertyKind::Set => ptr(typed.set_element).map(|element| RawPayload::Set { element }),
            PropertyKind::Map => ptr(typed.map_key)
                .zip(ptr(typed.map_value))
                .map(|(key, value)| RawPayload::Map { key, value }),
            PropertyKind::Enum => ptr(typed.enum_underlying).zip(ptr(typed.enum_type)).map(
                |(underlying, enum_object)| RawPayload::Enum {
                    underlying,
                    enum_object,
                },
            ),
            PropertyKind::Delegate
            | PropertyKind::MulticastInlineDelegate
            | PropertyKind::MulticastSparseDelegate => {
                ptr(typed.delegate_signature).map(|signature| RawPayload::Delegate { signature })
            }
            PropertyKind::Optional => {
                ptr(typed.optional_value).map(|inner| RawPayload::Optional { inner })
            }
        };
    payload.unwrap_or(RawPayload::Unknown)
}

fn read_bool(
    mem: &dyn MemoryReader,
    typed: &TypedPayloadLayout,
    field: Addr,
) -> Option<RawPayload> {
    let layout = typed.bool_property.as_ref()?;
    let window = Window::read(
        mem,
        field,
        "bool property",
        &[
            (layout.field_size, 1),
            (layout.byte_offset, 1),
            (layout.byte_mask, 1),
            (layout.field_mask, 1),
        ],
    )
    .ok()?;
    Some(RawPayload::Bool {
        field_size: window.u8(layout.field_size).ok()?,
        byte_offset: window.u8(layout.byte_offset).ok()?,
        byte_mask: window.u8(layout.byte_mask).ok()?,
        field_mask: window.u8(layout.field_mask).ok()?,
    })
}
