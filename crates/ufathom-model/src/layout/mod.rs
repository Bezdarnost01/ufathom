//! Engine layouts: every offset needed to read one Unreal Engine build.
//!
//! A layout is plain data. It is produced by discovery (or loaded from a user
//! file), validated against the target, and then consumed by the typed views
//! that read target memory. Offsets are `u32` and named; none of them is a
//! pointer into the target.

mod classes;
mod names;
mod object_array;
mod property;


pub use classes::{
    UClassLayout, UEnumLayout, UFieldLayout, UFunctionLayout, UObjectLayout, UStructLayout,
};
pub use names::{
    FNameLayout, NameArrayLayout, NameEntryArrayLayout, NamePoolLayout, NameTableLayout,
    PoolEntryLayout,
};
pub use object_array::{ObjectArrayKind, ObjectArrayLayout, TransformOp};
pub use property::{
    BoolPayloadLayout, FFieldClassLayout, FFieldLayout, PropertyLayout, PropertySystem,
    TypedPayloadLayout,
};

use serde::{Deserialize, Serialize};

/// Where a global structure lives in the target.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Location {
    /// Relative to the load address of a named module. Stable across ASLR.
    Rva {
        /// Module file name, for example the game executable.
        module: String,
        /// Offset from the module base.
        rva: u64,
    },
    /// A fixed virtual address, valid only for one process image.
    Absolute(u64),
}

/// Everything needed to read one build of the engine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EngineLayout {
    /// Size of a target pointer in bytes (4 or 8).
    pub pointer_size: u8,
    /// Where names are stored and how an entry is decoded.
    pub names: NameTableLayout,
    /// The global object array.
    pub objects: ObjectArrayLayout,
    /// Shape of `FName`.
    pub fname: FNameLayout,
    /// Fields of `UObject`.
    pub uobject: UObjectLayout,
    /// Fields of `UField`.
    pub ufield: UFieldLayout,
    /// Fields of `UStruct`.
    pub ustruct: UStructLayout,
    /// Fields of `UClass`.
    pub uclass: UClassLayout,
    /// Fields of `UFunction`.
    pub ufunction: UFunctionLayout,
    /// How `UEnum` stores its names and values.
    pub uenum: UEnumLayout,
    /// Properties, in either the `UProperty` or the `FProperty` system.
    pub properties: PropertyLayout,
}
