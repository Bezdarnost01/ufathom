//! The plain Rust representation of a usmap file.

use std::fmt;

/// A usmap format version. This crate reads all of them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum UsmapVersion {
    /// Version 0. Names have 8-bit lengths, enums at most 255 values, and
    /// enum values are implied by position.
    Initial,
    /// Version 1. Adds the optional package-version block to the header.
    PackageVersioning,
    /// Version 2. Name lengths are 16 bits.
    LongFName,
    /// Version 3. Enum value counts are 16 bits.
    LargeEnums,
    /// Version 4. Every enum value carries an explicit `i64` number.
    ExplicitEnumValues,
}

impl UsmapVersion {
    /// The version this crate writes.
    pub const LATEST: Self = Self::ExplicitEnumValues;

    /// Converts the header byte, or returns `None` for a version this crate
    /// does not know.
    #[must_use]
    pub fn from_u8(byte: u8) -> Option<Self> {
        match byte {
            0 => Some(Self::Initial),
            1 => Some(Self::PackageVersioning),
            2 => Some(Self::LongFName),
            3 => Some(Self::LargeEnums),
            4 => Some(Self::ExplicitEnumValues),
            _ => None,
        }
    }

    /// The header byte.
    #[must_use]
    pub fn as_u8(self) -> u8 {
        match self {
            Self::Initial => 0,
            Self::PackageVersioning => 1,
            Self::LongFName => 2,
            Self::LargeEnums => 3,
            Self::ExplicitEnumValues => 4,
        }
    }

    /// Whether the header carries the package-version block.
    #[must_use]
    pub fn has_version_block(self) -> bool {
        self >= Self::PackageVersioning
    }

    /// Whether enum values are stored. Before this, they are the position of
    /// the enumerator, which says nothing about the real number.
    #[must_use]
    pub fn has_explicit_enum_values(self) -> bool {
        self >= Self::ExplicitEnumValues
    }
}

impl fmt::Display for UsmapVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_u8())
    }
}

/// How the payload of a file was stored. Only methods this crate can decode
/// are represented: any other is a [`UsmapError`](crate::UsmapError).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Compression {
    /// Stored as is.
    None,
    /// A zstd frame.
    Zstd,
}

/// One entry of the custom-version table of a package-version block.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CustomVersion {
    /// The version's GUID, as stored.
    pub guid: [u8; 16],
    /// The version number.
    pub version: i32,
}

/// The package-version block of a header: the engine's file versions at the
/// time the mappings were taken. Present from format version 1, when the file
/// sets its `bHasVersionInfo` flag.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VersionInfo {
    /// `FileVersionUE4`.
    pub ue4: i32,
    /// `FileVersionUE5`.
    pub ue5: i32,
    /// Custom versions.
    pub custom_versions: Vec<CustomVersion>,
    /// `NetCL`, the engine's network changelist.
    pub net_cl: u32,
}

/// A property type as usmap stores it.
///
/// The format is coarser than the engine: an object property does not name its
/// class, a bool does not carry its bit, and a delegate does not name its
/// signature. Only struct and enum references carry a (short) name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UsmapType {
    /// `ByteProperty`, a plain `uint8`.
    Byte,
    /// `BoolProperty`.
    Bool,
    /// `IntProperty`, a 32-bit signed integer.
    Int32,
    /// `FloatProperty`.
    Float,
    /// `ObjectProperty`. Also stands for class-pointer properties.
    Object,
    /// `NameProperty`.
    Name,
    /// `DelegateProperty`.
    Delegate,
    /// `DoubleProperty`.
    Double,
    /// `ArrayProperty`.
    Array {
        /// Element type.
        inner: Box<UsmapType>,
    },
    /// `StructProperty`.
    Struct {
        /// Short name of the struct.
        name: String,
    },
    /// `StrProperty`.
    Str,
    /// `TextProperty`.
    Text,
    /// `InterfaceProperty`.
    Interface,
    /// `MulticastDelegateProperty`, inline or sparse.
    MulticastDelegate,
    /// `WeakObjectProperty`.
    WeakObject,
    /// `LazyObjectProperty`.
    LazyObject,
    /// `AssetObjectProperty`, the old name of a soft object pointer.
    AssetObject,
    /// `SoftObjectProperty`. Also stands for soft class pointers.
    SoftObject,
    /// `UInt64Property`.
    UInt64,
    /// `UInt32Property`.
    UInt32,
    /// `UInt16Property`.
    UInt16,
    /// `Int64Property`.
    Int64,
    /// `Int16Property`.
    Int16,
    /// `Int8Property`.
    Int8,
    /// `MapProperty`.
    Map {
        /// Key type.
        key: Box<UsmapType>,
        /// Value type.
        value: Box<UsmapType>,
    },
    /// `SetProperty`.
    Set {
        /// Element type.
        element: Box<UsmapType>,
    },
    /// `EnumProperty`. A byte backed by an enum is written this way, with
    /// [`UsmapType::Byte`] as the underlying type.
    Enum {
        /// Underlying integer type.
        inner: Box<UsmapType>,
        /// Short name of the enum.
        name: String,
    },
    /// `FieldPathProperty`.
    FieldPath,
    /// `OptionalProperty`.
    Optional {
        /// Value type.
        inner: Box<UsmapType>,
    },
    /// `Utf8StrProperty`.
    Utf8Str,
    /// `AnsiStrProperty`.
    AnsiStr,
    /// A property the generator could not classify (tag `0xFF`).
    Unknown,
}

impl fmt::Display for UsmapType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Byte => f.write_str("Byte"),
            Self::Bool => f.write_str("Bool"),
            Self::Int32 => f.write_str("Int32"),
            Self::Float => f.write_str("Float"),
            Self::Object => f.write_str("Object"),
            Self::Name => f.write_str("Name"),
            Self::Delegate => f.write_str("Delegate"),
            Self::Double => f.write_str("Double"),
            Self::Array { inner } => write!(f, "Array<{inner}>"),
            Self::Struct { name } => write!(f, "Struct<{name}>"),
            Self::Str => f.write_str("Str"),
            Self::Text => f.write_str("Text"),
            Self::Interface => f.write_str("Interface"),
            Self::MulticastDelegate => f.write_str("MulticastDelegate"),
            Self::WeakObject => f.write_str("WeakObject"),
            Self::LazyObject => f.write_str("LazyObject"),
            Self::AssetObject => f.write_str("AssetObject"),
            Self::SoftObject => f.write_str("SoftObject"),
            Self::UInt64 => f.write_str("UInt64"),
            Self::UInt32 => f.write_str("UInt32"),
            Self::UInt16 => f.write_str("UInt16"),
            Self::Int64 => f.write_str("Int64"),
            Self::Int16 => f.write_str("Int16"),
            Self::Int8 => f.write_str("Int8"),
            Self::Map { key, value } => write!(f, "Map<{key}, {value}>"),
            Self::Set { element } => write!(f, "Set<{element}>"),
            Self::Enum { inner, name } => write!(f, "Enum<{name}: {inner}>"),
            Self::FieldPath => f.write_str("FieldPath"),
            Self::Optional { inner } => write!(f, "Optional<{inner}>"),
            Self::Utf8Str => f.write_str("Utf8Str"),
            Self::AnsiStr => f.write_str("AnsiStr"),
            Self::Unknown => f.write_str("Unknown"),
        }
    }
}

/// A property of a [`UsmapStruct`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsmapProperty {
    /// Property name.
    pub name: String,
    /// Position of the property in the struct's schema. Each property takes
    /// `array_dim` consecutive slots, so the next property's index is
    /// this one's plus its dimension.
    pub index: u16,
    /// Number of elements of a C array, 1 for a plain member. The format
    /// stores one byte, so a larger array is stored as 255.
    pub array_dim: u8,
    /// Property type.
    pub ty: UsmapType,
}

/// A struct or class. The format makes no difference between the two.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsmapStruct {
    /// Short name, without a package.
    pub name: String,
    /// Short name of the parent, `None` for a root type.
    pub super_name: Option<String>,
    /// `PropCount`: the number of schema slots, array elements included.
    /// Non-serializable properties are counted but not listed, so this can
    /// exceed the sum of [`UsmapProperty::array_dim`].
    pub property_count: u16,
    /// Serializable properties declared by this type, not inherited ones.
    pub properties: Vec<UsmapProperty>,
}

/// One enumerator of a [`UsmapEnum`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsmapEnumValue {
    /// Enumerator name.
    pub name: String,
    /// Number. For a file older than
    /// [`ExplicitEnumValues`](UsmapVersion::ExplicitEnumValues) this is the
    /// position, not a number read from the file.
    pub value: i64,
}

/// An enumeration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsmapEnum {
    /// Short name, without a package.
    pub name: String,
    /// Enumerators in file order.
    pub values: Vec<UsmapEnumValue>,
}

/// A parsed usmap file with every name resolved to a string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Usmap {
    /// Format version of the file.
    pub version: UsmapVersion,
    /// How the payload was stored.
    pub compression: Compression,
    /// The package-version block, if the file has one with its flag set.
    pub version_info: Option<VersionInfo>,
    /// The name table as stored, in file order. Duplicates are kept, since
    /// some generators write them.
    pub names: Vec<String>,
    /// Enumerations in file order.
    pub enums: Vec<UsmapEnum>,
    /// Structs and classes in file order.
    pub structs: Vec<UsmapStruct>,
    /// Bytes after the struct list. They hold optional extension chunks
    /// (`CEXT`, `PPTH`, `EATR`, `ENVP`), which this crate does not interpret.
    pub extension_bytes: usize,
}

impl Usmap {
    /// Whether the enum numbers are stored values rather than positions.
    #[must_use]
    pub fn has_explicit_enum_values(&self) -> bool {
        self.version.has_explicit_enum_values()
    }
}
