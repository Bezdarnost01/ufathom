//! Types, fields, functions and enums of a dump.

use std::cmp::Ordering;
use std::fmt;

use serde::{Deserialize, Serialize};

/// Whether a [`StructDef`] is a class or a plain script struct.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum StructKind {
    /// A `UClass`.
    Class,
    /// A `UScriptStruct`.
    ScriptStruct,
}

/// A class or script struct.
///
/// Field order matters for the derived `Ord`: `path` first, so sorting a list
/// of structs sorts by path.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct StructDef {
    /// Full object path, for example `/Script/Engine.Actor`.
    pub path: String,
    /// Short name without the package.
    pub name: String,
    /// Package the type lives in, for example `/Script/Engine`.
    pub package: String,
    /// Class or script struct.
    pub kind: StructKind,
    /// Path of the parent type. `None` for a root type.
    pub super_path: Option<String>,
    /// `PropertiesSize`, including the parent's members.
    pub size: u32,
    /// Minimum alignment in bytes.
    pub alignment: u32,
    /// Data members declared by this type (not inherited ones).
    pub fields: Vec<FieldDef>,
    /// Functions declared by this type (not inherited ones). Always empty for
    /// script structs.
    pub functions: Vec<FunctionDef>,
    /// Raw engine flags: `EClassFlags` for a class, `EStructFlags` for a
    /// script struct.
    pub flags: u64,
}

/// A data member of a type, or a parameter of a function.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct FieldDef {
    /// Member name.
    pub name: String,
    /// Offset from the start of the owner (or of the parameter block).
    pub offset: u32,
    /// Decoded type.
    pub ty: PropertyType,
    /// Size of one element in bytes.
    pub element_size: u32,
    /// Number of elements: 1 for a plain member, more for a C array.
    pub array_dim: u32,
    /// Raw `EPropertyFlags`.
    pub flags: u64,
}

impl FieldDef {
    /// The order used to print fields: by `(offset, name)`, then by the rest
    /// of the content so equal-looking fields still order deterministically.
    pub(crate) fn output_order(&self, other: &Self) -> Ordering {
        self.offset
            .cmp(&other.offset)
            .then_with(|| self.name.cmp(&other.name))
            .then_with(|| self.cmp(other))
    }
}

/// A function of a class.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct FunctionDef {
    /// Full object path, for example `/Script/Engine.Actor:K2_DestroyActor`.
    pub path: String,
    /// Short name.
    pub name: String,
    /// Raw `EFunctionFlags`.
    pub flags: u32,
    /// Size in bytes of the parameter block, return value included. It is the
    /// `UStruct::PropertiesSize` of the `UFunction` itself, not a sum over
    /// [`params`](Self::params): padding and trailing locals are counted by
    /// the engine, not derived from the listed parameters.
    pub params_size: u32,
    /// Parameters in declared order, the return value (flagged
    /// `CPF_RETURN_PARM`) included. The order is part of the calling
    /// convention, so it is never sorted.
    pub params: Vec<FieldDef>,
    /// RVA of the function's exec entry point (`UFunction::Func`) in the game
    /// module. This is the thunk the script VM calls with an `FFrame`, not
    /// necessarily the native implementation. `None` for script functions and
    /// when the address is not inside the module.
    pub native_rva: Option<u64>,
}

/// An enumeration.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct EnumDef {
    /// Full object path.
    pub path: String,
    /// Short name without the package.
    pub name: String,
    /// Package the enum lives in.
    pub package: String,
    /// Width in bytes of the underlying integer.
    pub underlying_size: u32,
    /// Enumerators.
    pub values: Vec<EnumValue>,
}

/// One enumerator.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct EnumValue {
    /// Enumerator name, without the enum prefix.
    pub name: String,
    /// Numeric value.
    pub value: i64,
}

impl EnumValue {
    /// The order used to print enumerators: by `(value, name)`.
    pub(crate) fn output_order(&self, other: &Self) -> Ordering {
        self.value
            .cmp(&other.value)
            .then_with(|| self.name.cmp(&other.name))
    }
}

/// The four fields of a bit-field `bool` property.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct BoolBits {
    /// Which byte of the storage unit holds the bit.
    pub byte_offset: u8,
    /// Mask of the bit inside that byte.
    pub byte_mask: u8,
    /// Mask of the bit inside the whole field.
    pub field_mask: u8,
    /// Size in bytes of the storage unit.
    pub field_size: u8,
}

/// The decoded type of a field or parameter.
///
/// Types that refer to another type do so by object path. A reference that
/// could not be resolved is stored as [`PropertyType::Unknown`], never as an
/// empty path.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum PropertyType {
    /// Signed 8-bit integer.
    Int8,
    /// Signed 16-bit integer.
    Int16,
    /// Signed 32-bit integer.
    Int32,
    /// Signed 64-bit integer.
    Int64,
    /// Unsigned 8-bit integer.
    UInt8,
    /// Unsigned 16-bit integer.
    UInt16,
    /// Unsigned 32-bit integer.
    UInt32,
    /// Unsigned 64-bit integer.
    UInt64,
    /// 32-bit float.
    Float,
    /// 64-bit float.
    Double,
    /// Bit-field boolean.
    Bool {
        /// Which byte of the storage unit holds the bit.
        byte_offset: u8,
        /// Mask of the bit inside that byte.
        byte_mask: u8,
        /// Mask of the bit inside the whole field.
        field_mask: u8,
        /// Size in bytes of the storage unit.
        field_size: u8,
    },
    /// `FName`.
    Name,
    /// `FString`.
    Str,
    /// `FText`.
    Text,
    /// Hard object pointer.
    Object {
        /// Path of the pointed-to class.
        class: String,
    },
    /// `UClass` pointer restricted to a base class.
    Class {
        /// Path of the property's class (usually `Class`).
        class: String,
        /// Path of the meta class the value must derive from.
        meta: String,
    },
    /// `TWeakObjectPtr`.
    WeakObject {
        /// Path of the pointed-to class.
        class: String,
    },
    /// `TLazyObjectPtr`.
    LazyObject {
        /// Path of the pointed-to class.
        class: String,
    },
    /// `TSoftObjectPtr`.
    SoftObject {
        /// Path of the pointed-to class.
        class: String,
    },
    /// `TSoftClassPtr`.
    SoftClass {
        /// Path of the meta class.
        meta: String,
    },
    /// Interface reference.
    Interface {
        /// Path of the interface class.
        class: String,
    },
    /// Struct by value.
    Struct {
        /// Path of the script struct.
        path: String,
    },
    /// `TArray`.
    Array {
        /// Element type.
        inner: Box<PropertyType>,
    },
    /// `TSet`.
    Set {
        /// Element type.
        element: Box<PropertyType>,
    },
    /// `TMap`.
    Map {
        /// Key type.
        key: Box<PropertyType>,
        /// Value type.
        value: Box<PropertyType>,
    },
    /// `enum class` property with an explicit underlying type.
    Enum {
        /// Path of the enum.
        path: String,
        /// Underlying integer type.
        underlying: Box<PropertyType>,
    },
    /// Byte property backed by an enum. A plain byte is [`PropertyType::UInt8`].
    Byte {
        /// Path of the enum.
        path: String,
    },
    /// Single-cast delegate.
    Delegate {
        /// Path of the signature function.
        signature: String,
    },
    /// Multicast delegate stored inline.
    MulticastInlineDelegate {
        /// Path of the signature function.
        signature: String,
    },
    /// Multicast delegate stored sparsely.
    MulticastSparseDelegate {
        /// Path of the signature function.
        signature: String,
    },
    /// `FFieldPath`.
    FieldPath,
    /// `TOptional`.
    Optional {
        /// Value type.
        inner: Box<PropertyType>,
    },
    /// A property class this tool does not decode. The size is still correct
    /// because it comes from the property itself.
    Unknown {
        /// Engine class name of the property, for example `FooProperty`.
        type_name: String,
    },
}

impl PropertyType {
    /// The bit-field layout if this is a [`PropertyType::Bool`].
    #[must_use]
    pub fn bool_bits(&self) -> Option<BoolBits> {
        match *self {
            Self::Bool {
                byte_offset,
                byte_mask,
                field_mask,
                field_size,
            } => Some(BoolBits {
                byte_offset,
                byte_mask,
                field_mask,
                field_size,
            }),
            _ => None,
        }
    }
}

impl fmt::Display for BoolBits {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "bool(byte {}, byte_mask {:#04x}, field_mask {:#04x}, size {})",
            self.byte_offset, self.byte_mask, self.field_mask, self.field_size
        )
    }
}

impl fmt::Display for PropertyType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Int8 => f.write_str("int8"),
            Self::Int16 => f.write_str("int16"),
            Self::Int32 => f.write_str("int32"),
            Self::Int64 => f.write_str("int64"),
            Self::UInt8 => f.write_str("uint8"),
            Self::UInt16 => f.write_str("uint16"),
            Self::UInt32 => f.write_str("uint32"),
            Self::UInt64 => f.write_str("uint64"),
            Self::Float => f.write_str("float"),
            Self::Double => f.write_str("double"),
            Self::Bool { .. } => match self.bool_bits() {
                Some(bits) => fmt::Display::fmt(&bits, f),
                None => f.write_str("bool"),
            },
            Self::Name => f.write_str("FName"),
            Self::Str => f.write_str("FString"),
            Self::Text => f.write_str("FText"),
            Self::Object { class } => write!(f, "Object<{class}>"),
            Self::Class { class, meta } => write!(f, "Class<{class}, meta {meta}>"),
            Self::WeakObject { class } => write!(f, "WeakObject<{class}>"),
            Self::LazyObject { class } => write!(f, "LazyObject<{class}>"),
            Self::SoftObject { class } => write!(f, "SoftObject<{class}>"),
            Self::SoftClass { meta } => write!(f, "SoftClass<{meta}>"),
            Self::Interface { class } => write!(f, "Interface<{class}>"),
            Self::Struct { path } => write!(f, "Struct<{path}>"),
            Self::Array { inner } => write!(f, "Array<{inner}>"),
            Self::Set { element } => write!(f, "Set<{element}>"),
            Self::Map { key, value } => write!(f, "Map<{key}, {value}>"),
            Self::Enum { path, underlying } => write!(f, "Enum<{path}: {underlying}>"),
            Self::Byte { path } => write!(f, "Byte<{path}>"),
            Self::Delegate { signature } => write!(f, "Delegate<{signature}>"),
            Self::MulticastInlineDelegate { signature } => {
                write!(f, "MulticastInlineDelegate<{signature}>")
            }
            Self::MulticastSparseDelegate { signature } => {
                write!(f, "MulticastSparseDelegate<{signature}>")
            }
            Self::FieldPath => f.write_str("FieldPath"),
            Self::Optional { inner } => write!(f, "Optional<{inner}>"),
            Self::Unknown { type_name } => write!(f, "Unknown({type_name})"),
        }
    }
}

/// How serious a [`Diagnostic`] is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Severity {
    /// Worth knowing, changes nothing.
    Info,
    /// Something was skipped or degraded, the rest of the dump is intact.
    Warning,
    /// Part of the dump is missing or unreliable.
    Error,
}

/// A problem met while building a dump, tied to the object that caused it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Diagnostic {
    /// Path of the object concerned. Empty for a problem that is not tied to
    /// one object.
    pub path: String,
    /// How serious it is.
    pub severity: Severity,
    /// What happened.
    pub message: String,
}
