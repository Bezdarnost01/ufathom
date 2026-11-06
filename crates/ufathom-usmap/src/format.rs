//! Constants of the usmap binary format.
//!
//! The format is described by the community "Unreal Mappings" specification
//! and is implemented by several tools. The values here are facts about that
//! format; the code that uses them is independent.

/// First two bytes of every usmap file, as a little-endian `u16`.
pub(crate) const MAGIC: u16 = 0x30C4;

/// Marks an absent name, used for the parent of a root struct.
pub(crate) const NO_NAME: u32 = u32::MAX;

/// Compression method byte: payload stored as is.
pub(crate) const COMPRESSION_NONE: u8 = 0;
/// Compression method byte: Oodle. Recognised only to name it in an error.
pub(crate) const COMPRESSION_OODLE: u8 = 1;
/// Compression method byte: Brotli. Recognised only to name it in an error.
pub(crate) const COMPRESSION_BROTLI: u8 = 2;
/// Compression method byte: zstd.
pub(crate) const COMPRESSION_ZSTD: u8 = 3;

/// Bytes of one entry of the custom-version table in a version header: a
/// 16-byte GUID and an `i32`.
pub(crate) const CUSTOM_VERSION_SIZE: usize = 20;

/// Property type tags (`EPropertyType`).
pub(crate) mod tag {
    pub(crate) const BYTE: u8 = 0;
    pub(crate) const BOOL: u8 = 1;
    pub(crate) const INT: u8 = 2;
    pub(crate) const FLOAT: u8 = 3;
    pub(crate) const OBJECT: u8 = 4;
    pub(crate) const NAME: u8 = 5;
    pub(crate) const DELEGATE: u8 = 6;
    pub(crate) const DOUBLE: u8 = 7;
    pub(crate) const ARRAY: u8 = 8;
    pub(crate) const STRUCT: u8 = 9;
    pub(crate) const STR: u8 = 10;
    pub(crate) const TEXT: u8 = 11;
    pub(crate) const INTERFACE: u8 = 12;
    pub(crate) const MULTICAST_DELEGATE: u8 = 13;
    pub(crate) const WEAK_OBJECT: u8 = 14;
    pub(crate) const LAZY_OBJECT: u8 = 15;
    pub(crate) const ASSET_OBJECT: u8 = 16;
    pub(crate) const SOFT_OBJECT: u8 = 17;
    pub(crate) const UINT64: u8 = 18;
    pub(crate) const UINT32: u8 = 19;
    pub(crate) const UINT16: u8 = 20;
    pub(crate) const INT64: u8 = 21;
    pub(crate) const INT16: u8 = 22;
    pub(crate) const INT8: u8 = 23;
    pub(crate) const MAP: u8 = 24;
    pub(crate) const SET: u8 = 25;
    pub(crate) const ENUM: u8 = 26;
    pub(crate) const FIELD_PATH: u8 = 27;
    pub(crate) const OPTIONAL: u8 = 28;
    pub(crate) const UTF8_STR: u8 = 29;
    pub(crate) const ANSI_STR: u8 = 30;
    pub(crate) const UNKNOWN: u8 = 0xFF;
}
