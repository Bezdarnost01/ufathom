//! Typed views over target memory: names, objects and properties, per engine generation.
//!
//! Every view takes a `&dyn MemoryReader` and the layout pieces it needs, so
//! the layout solver can use them with a partly solved layout. Nothing here
//! dereferences a target pointer: all reads go through the reader, so a wrong
//! offset is an [`EngineError`], never a fault. Every count, pointer and
//! string read from the target is untrusted and checked against a named
//! limit (see [`limits`]).
//!
//! - [`resolve_location`] turns a layout location into an address.
//! - [`NameTable`] loads every name once and resolves `FName` values.
//! - [`ObjectArrayView`] reads the global object array; [`read_header`] and
//!   [`object_path`] read objects.
//! - [`read_struct`], [`read_function`] and [`read_enum_entries`] read the
//!   reflection classes.
//! - [`PropertySystemReader`] walks and decodes properties in both reflection
//!   generations, and [`property_type`] converts them to dump types.

#![forbid(unsafe_code)]

mod address;
mod error;
mod header;
pub mod limits;
mod names;
mod object_array;
mod props;
mod structs;
mod window;

pub use address::resolve_location;
pub use error::EngineError;
pub use header::{FNameBytes, ObjectHeader, object_path, read_header};
pub use names::{BlockProblem, LoadOptions, LoadStats, NameTable};
pub use object_array::{ChunkObjects, ObjectArrayView, ObjectSlot};
pub use props::{
    FieldIter, LegacyProperties, ModernProperties, PropertyKind, PropertySystemReader, RawPayload,
    RawProperty, for_layout, property_type, validate_members,
};
pub use structs::{
    EnumEntry, FunctionView, StructView, SuperChain, read_enum_entries, read_function, read_struct,
    short_enum_value_name, super_chain,
};
