//! Reading properties, in either reflection generation.
//!
//! [`PropertySystemReader`] hides the difference between `UProperty` objects
//! linked through `UStruct::Children` and `FProperty` fields linked through
//! `UStruct::ChildProperties`. [`for_layout`] picks one from an
//! [`EngineLayout`]. A decoded property is a [`RawProperty`] whose references
//! are addresses; [`property_type`] turns one into a
//! [`PropertyType`](ufathom_model::PropertyType).

mod chain;
mod classes;
mod convert;
mod kinds;
mod legacy;
mod modern;
mod payload;
mod validate;

use ufathom_mem::{Addr, MemoryReader};
use ufathom_model::EngineLayout;
use ufathom_model::layout::{PropertyLayout, PropertySystem};

pub use chain::FieldIter;
pub use convert::property_type;
pub use kinds::PropertyKind;
pub use legacy::LegacyProperties;
pub use modern::ModernProperties;
pub use validate::validate_members;

use crate::error::EngineError;
use crate::names::NameTable;
use crate::window::Window;

/// The unresolved typed data of a property. Addresses are zero when null.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RawPayload {
    /// A bit-field boolean.
    Bool {
        /// Size in bytes of the storage unit.
        field_size: u8,
        /// Byte of the unit that holds the bit.
        byte_offset: u8,
        /// Mask of the bit inside that byte.
        byte_mask: u8,
        /// Mask of the bit inside the whole unit.
        field_mask: u8,
    },
    /// An object-like property: object, weak, lazy or soft.
    Object {
        /// The `UClass` the property is restricted to.
        class: Addr,
    },
    /// A class-like property: class or soft class.
    Class {
        /// The property's own class (usually `Class`).
        class: Addr,
        /// The meta class the value must derive from.
        meta: Addr,
    },
    /// A struct by value.
    Struct {
        /// The `UScriptStruct`.
        script_struct: Addr,
    },
    /// A `TArray`.
    Array {
        /// The inner property (an address in the active property system).
        inner: Addr,
    },
    /// A `TSet`.
    Set {
        /// The element property.
        element: Addr,
    },
    /// A `TMap`.
    Map {
        /// The key property.
        key: Addr,
        /// The value property.
        value: Addr,
    },
    /// An `enum class` property.
    Enum {
        /// The underlying integer property.
        underlying: Addr,
        /// The `UEnum`.
        enum_object: Addr,
    },
    /// A byte, with an optional `UEnum`.
    Byte {
        /// The `UEnum`, or zero for a plain byte.
        enum_object: Addr,
    },
    /// A delegate of any kind.
    Delegate {
        /// The signature `UFunction`.
        signature: Addr,
    },
    /// An interface reference.
    Interface {
        /// The interface `UClass`.
        class: Addr,
    },
    /// A `TOptional`.
    Optional {
        /// The value property.
        inner: Addr,
    },
    /// A property with no payload to resolve: integers, floats, name, string,
    /// text and field path.
    Scalar,
    /// The payload could not be read: the layout has no offset for it, the
    /// read failed, or the class is not known. The rest of the property is
    /// still correct.
    Unknown,
}

/// A decoded property.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawProperty {
    /// Address of the property object or field.
    pub address: Addr,
    /// Member name.
    pub name: String,
    /// Property class name, such as `IntProperty`.
    pub class_name: String,
    /// `Offset_Internal`, the member offset in its owner.
    pub offset: u32,
    /// `ElementSize`.
    pub element_size: u32,
    /// `ArrayDim`.
    pub array_dim: u32,
    /// `EPropertyFlags`.
    pub flags: u64,
    /// The typed payload.
    pub payload: RawPayload,
}

/// One interface over both property systems.
pub trait PropertySystemReader: Send + Sync {
    /// The properties of `owner` (a struct or function), as addresses to
    /// pass to [`decode`](Self::decode), in chain order.
    ///
    /// Items are errors for links that cannot be read, cycles and limits; see
    /// [`FieldIter`].
    fn fields(&self, owner: Addr) -> FieldIter<'_>;

    /// The `UFunction` members of `owner`: every `Children` link whose class
    /// chain reaches `Function`, which includes delegate signature functions.
    fn functions(&self, owner: Addr) -> FieldIter<'_>;

    /// Decodes the property at `field`.
    ///
    /// A missing or unreadable payload gives [`RawPayload::Unknown`], not an
    /// error. An error means the property's own fields, name or class could
    /// not be read.
    ///
    /// # Errors
    ///
    /// [`EngineError::Read`], [`EngineError::Null`] or a wrapped
    /// [`EngineError::UnresolvedName`].
    fn decode(&self, field: Addr) -> Result<RawProperty, EngineError>;
}

/// Picks the property system `layout` describes.
///
/// Never fails: a layout that cannot work (a modern system without
/// `ChildProperties`, say) makes the returned reader yield error items.
#[must_use]
pub fn for_layout<'a>(
    mem: &'a dyn MemoryReader,
    layout: &'a EngineLayout,
    names: &'a NameTable,
) -> Box<dyn PropertySystemReader + 'a> {
    match layout.properties.system {
        PropertySystem::Legacy => Box::new(LegacyProperties::new(mem, layout, names)),
        PropertySystem::Modern { .. } => Box::new(ModernProperties::new(mem, layout, names)),
    }
}

/// Reads the members every property has and attaches the typed payload.
pub(crate) fn decode_body(
    mem: &dyn MemoryReader,
    props: &PropertyLayout,
    address: Addr,
    name: String,
    class_name: String,
) -> Result<RawProperty, EngineError> {
    let window = Window::read(
        mem,
        address,
        "property",
        &[
            (props.array_dim, 4),
            (props.element_size, 4),
            (props.flags, 8),
            (props.offset_internal, 4),
        ],
    )?;
    let kind = PropertyKind::from_class_name(&class_name);
    Ok(RawProperty {
        address,
        offset: window.u32(props.offset_internal)?,
        element_size: window.u32(props.element_size)?,
        array_dim: window.u32(props.array_dim)?,
        flags: window.u64(props.flags)?,
        payload: payload::read_payload(mem, &props.typed, kind, address),
        name,
        class_name,
    })
}

/// Walks `Children` and keeps the functions. Shared by both systems.
pub(crate) fn function_walk<'a>(
    mem: &'a dyn MemoryReader,
    layout: &'a EngineLayout,
    chains: &'a classes::ObjectChains<'a>,
    owner: Addr,
) -> FieldIter<'a> {
    FieldIter::walk(
        mem,
        "function list",
        owner,
        layout.ustruct.children,
        layout.ufield.next,
        Box::new(move |object| chains.is_function(object)),
    )
}
