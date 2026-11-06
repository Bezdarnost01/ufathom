//! Offsets inside `UObject` and the reflection classes derived from it.

use serde::{Deserialize, Serialize};

/// Fields of `UObject`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UObjectLayout {
    /// Offset of `ObjectFlags` (`EObjectFlags`).
    pub flags: u32,
    /// Offset of `InternalIndex`, the slot in the global object array.
    pub index: u32,
    /// Offset of `ClassPrivate`.
    pub class: u32,
    /// Offset of `NamePrivate`.
    pub name: u32,
    /// Offset of `OuterPrivate`. It is 0x20 on builds with an 8-byte `FName`
    /// and 0x28 on builds with a 12-byte one.
    pub outer: u32,
    /// `sizeof(UObject)`.
    pub size: u32,
}

/// Fields of `UField`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UFieldLayout {
    /// Offset of `Next`, the next field in the owner's member list.
    pub next: u32,
}

/// Fields of `UStruct`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UStructLayout {
    /// Offset of `SuperStruct`.
    pub super_struct: u32,
    /// Offset of `Children`, the list of `UField` members (functions, and
    /// properties on builds before `FField`).
    pub children: u32,
    /// Offset of `ChildProperties`. `None` before the `FField` system.
    pub child_properties: Option<u32>,
    /// Offset of `PropertiesSize`.
    pub properties_size: u32,
    /// Offset of `MinAlignment`.
    pub min_alignment: u32,
    /// Width in bytes of `MinAlignment`: 4 up to UE 5.5, 2 from UE 5.6, which
    /// made it a 16-bit value.
    pub min_alignment_width: u32,
}

/// Fields of `UClass`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UClassLayout {
    /// Offset of `ClassCastFlags`. `None` when not yet found.
    pub cast_flags: Option<u32>,
    /// Offset of `ClassDefaultObject`. `None` when not yet found.
    pub default_object: Option<u32>,
}

/// Fields of `UFunction`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UFunctionLayout {
    /// Offset of `FunctionFlags` (`EFunctionFlags`).
    pub flags: u32,
    /// Offset of `Func`, the exec entry point. `None` when not yet found.
    pub native_func: Option<u32>,
}

/// How `UEnum` stores the names and values of its entries.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum UEnumLayout {
    /// One `TArray` of `(FName, value)` pairs.
    PairArray {
        /// Offset of the `TArray`.
        offset: u32,
        /// Size in bytes of one pair.
        stride: u32,
        /// Offset of the value inside a pair. The name sits at offset 0.
        value_offset: u32,
        /// Width in bytes of the value: 1, 2, 4 or 8.
        value_width: u32,
    },
    /// Two parallel arrays, as stored from UE 5.7. The array pointers carry
    /// tag bits in their low bits, which a reader must mask off.
    SplitArrays {
        /// Offset of the tagged pointer to the array of names.
        names: u32,
        /// Offset of the tagged pointer to the array of values.
        values: u32,
        /// Offset of the entry count shared by both arrays.
        count: u32,
    },
}
