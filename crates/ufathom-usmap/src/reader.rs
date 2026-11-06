//! Parsing of usmap files, versions 0 to 4.

use crate::cursor::Cursor;
use crate::decompress::decode_zstd;
use crate::error::UsmapError;
use crate::format::{
    COMPRESSION_BROTLI, COMPRESSION_NONE, COMPRESSION_OODLE, COMPRESSION_ZSTD, CUSTOM_VERSION_SIZE,
    MAGIC, NO_NAME, tag,
};
use crate::model::{
    Compression, CustomVersion, Usmap, UsmapEnum, UsmapEnumValue, UsmapProperty, UsmapStruct,
    UsmapType, UsmapVersion, VersionInfo,
};

/// Largest decompressed payload the reader accepts, in bytes. The header's
/// claim is checked against it before anything is allocated or decompressed.
/// Real files are a few megabytes; this leaves two orders of magnitude of
/// room.
pub const MAX_DECOMPRESSED_SIZE: u32 = 256 * 1024 * 1024;

/// Deepest nesting of container property types the reader accepts. Real
/// types nest three or four levels (`TMap<K, TArray<V>>`). The limit keeps
/// recursion on hostile input bounded.
pub const MAX_TYPE_DEPTH: usize = 32;

/// Smallest possible encodings, used to reject counts that cannot fit.
const MIN_NAME_SIZE: usize = 1;
const MIN_NAME_SIZE_LONG: usize = 2;
const ENUM_HEADER_SIZE: usize = 4 + 1;
const ENUM_HEADER_SIZE_LARGE: usize = 4 + 2;
const ENUM_VALUE_SIZE: usize = 4;
const ENUM_VALUE_SIZE_EXPLICIT: usize = 4 + 8;
const STRUCT_HEADER_SIZE: usize = 4 + 4 + 2 + 2;
const PROPERTY_HEADER_SIZE: usize = 2 + 1 + 4 + 1;

struct Header {
    version: UsmapVersion,
    version_info: Option<VersionInfo>,
    compression: Compression,
    compressed_size: u32,
    decompressed_size: u32,
}

impl Usmap {
    /// Parses a usmap file.
    ///
    /// Reads format versions 0 to 4, with the payload stored or compressed
    /// with zstd. The input is treated as hostile: every read is bounds
    /// checked, counts are checked against the bytes left before anything is
    /// allocated, the declared decompressed size is capped at
    /// [`MAX_DECOMPRESSED_SIZE`] and must match the real size, name indices are
    /// validated, and property types may nest at most [`MAX_TYPE_DEPTH`]
    /// levels. Bytes after the declared payload are ignored. Bytes after the
    /// struct list are counted in [`Usmap::extension_bytes`] and not
    /// interpreted.
    ///
    /// # Errors
    ///
    /// Returns a [`UsmapError`] naming the problem and, where known, the byte
    /// offset. Oodle and Brotli payloads get
    /// [`UsmapError::UnsupportedCompression`].
    pub fn parse(bytes: &[u8]) -> Result<Self, UsmapError> {
        let mut cursor = Cursor::new(bytes, 0);
        let header = read_header(&mut cursor)?;

        if header.decompressed_size > MAX_DECOMPRESSED_SIZE {
            return Err(UsmapError::DecompressedSizeTooLarge {
                declared: header.decompressed_size,
                limit: MAX_DECOMPRESSED_SIZE,
            });
        }

        let payload_offset = cursor.offset();
        let available = cursor.remaining();
        let stored = usize::try_from(header.compressed_size)
            .ok()
            .filter(|&size| size <= available)
            .ok_or(UsmapError::PayloadTruncated {
                offset: payload_offset,
                declared: header.compressed_size,
                available,
            })?;
        let payload = cursor.take(stored)?;

        match header.compression {
            Compression::None => {
                if header.compressed_size != header.decompressed_size {
                    return Err(UsmapError::UncompressedSizeMismatch {
                        compressed: header.compressed_size,
                        decompressed: header.decompressed_size,
                    });
                }
                parse_body(payload, payload_offset, header)
            }
            Compression::Zstd => {
                let decoded = decode_zstd(payload, header.decompressed_size)?;
                parse_body(&decoded, 0, header)
            }
        }
    }
}

fn read_header(cursor: &mut Cursor<'_>) -> Result<Header, UsmapError> {
    let magic = cursor.u16()?;
    if magic != MAGIC {
        return Err(UsmapError::BadMagic { found: magic });
    }
    let version_byte = cursor.u8()?;
    let version = UsmapVersion::from_u8(version_byte).ok_or(UsmapError::UnsupportedVersion {
        version: version_byte,
    })?;

    let version_info = if version.has_version_block() {
        read_version_block(cursor)?
    } else {
        None
    };

    let method_offset = cursor.offset();
    let method = cursor.u8()?;
    let compression = match method {
        COMPRESSION_NONE => Compression::None,
        COMPRESSION_ZSTD => Compression::Zstd,
        COMPRESSION_OODLE => return Err(unsupported(method, "Oodle", method_offset)),
        COMPRESSION_BROTLI => return Err(unsupported(method, "Brotli", method_offset)),
        _ => return Err(unsupported(method, "unknown", method_offset)),
    };
    let compressed_size = cursor.u32()?;
    let decompressed_size = cursor.u32()?;
    Ok(Header {
        version,
        version_info,
        compression,
        compressed_size,
        decompressed_size,
    })
}

fn unsupported(method: u8, name: &'static str, offset: usize) -> UsmapError {
    UsmapError::UnsupportedCompression {
        method,
        name,
        offset,
    }
}

/// Reads `bHasVersionInfo` and, when set, the block it announces.
fn read_version_block(cursor: &mut Cursor<'_>) -> Result<Option<VersionInfo>, UsmapError> {
    if cursor.u32()? == 0 {
        return Ok(None);
    }
    let ue4 = cursor.i32()?;
    let ue5 = cursor.i32()?;
    let count = cursor.count("custom version", CUSTOM_VERSION_SIZE)?;
    let mut custom_versions = Vec::with_capacity(count);
    for _ in 0..count {
        custom_versions.push(CustomVersion {
            guid: cursor.guid()?,
            version: cursor.i32()?,
        });
    }
    let net_cl = cursor.u32()?;
    Ok(Some(VersionInfo {
        ue4,
        ue5,
        custom_versions,
        net_cl,
    }))
}

fn parse_body(body: &[u8], base: usize, header: Header) -> Result<Usmap, UsmapError> {
    let mut cursor = Cursor::new(body, base);
    let names = read_names(&mut cursor, header.version)?;
    let enums = read_enums(&mut cursor, header.version, &names)?;
    let structs = read_structs(&mut cursor, &names)?;
    Ok(Usmap {
        version: header.version,
        compression: header.compression,
        version_info: header.version_info,
        names,
        enums,
        structs,
        extension_bytes: cursor.remaining(),
    })
}

fn read_names(cursor: &mut Cursor<'_>, version: UsmapVersion) -> Result<Vec<String>, UsmapError> {
    let long = version >= UsmapVersion::LongFName;
    let min_size = if long {
        MIN_NAME_SIZE_LONG
    } else {
        MIN_NAME_SIZE
    };
    let count = cursor.count("name", min_size)?;
    let mut names = Vec::with_capacity(count);
    for _ in 0..count {
        let length = if long {
            usize::from(cursor.u16()?)
        } else {
            usize::from(cursor.u8()?)
        };
        let offset = cursor.offset();
        let text = std::str::from_utf8(cursor.take(length)?)
            .map_err(|_| UsmapError::InvalidUtf8 { offset })?;
        names.push(text.to_owned());
    }
    Ok(names)
}

/// Reads a name index and resolves it. Indices are `u32` in the file.
fn read_name(cursor: &mut Cursor<'_>, names: &[String]) -> Result<String, UsmapError> {
    let offset = cursor.offset();
    let index = cursor.u32()?;
    resolve(names, index, offset).cloned()
}

fn resolve(names: &[String], index: u32, offset: usize) -> Result<&String, UsmapError> {
    usize::try_from(index)
        .ok()
        .and_then(|position| names.get(position))
        .ok_or(UsmapError::NameIndexOutOfRange {
            index,
            offset,
            count: names.len(),
        })
}

fn read_enums(
    cursor: &mut Cursor<'_>,
    version: UsmapVersion,
    names: &[String],
) -> Result<Vec<UsmapEnum>, UsmapError> {
    let large = version >= UsmapVersion::LargeEnums;
    let explicit = version.has_explicit_enum_values();
    let header_size = if large {
        ENUM_HEADER_SIZE_LARGE
    } else {
        ENUM_HEADER_SIZE
    };
    let value_size = if explicit {
        ENUM_VALUE_SIZE_EXPLICIT
    } else {
        ENUM_VALUE_SIZE
    };

    let count = cursor.count("enum", header_size)?;
    let mut enums = Vec::with_capacity(count);
    for _ in 0..count {
        let name = read_name(cursor, names)?;
        let value_count_offset = cursor.offset();
        let value_count = if large {
            u64::from(cursor.u16()?)
        } else {
            u64::from(cursor.u8()?)
        };
        let value_count =
            cursor.check_count("enum value", value_count, value_count_offset, value_size)?;
        let mut values = Vec::with_capacity(value_count);
        for position in 0..value_count {
            let value = if explicit {
                cursor.i64()?
            } else {
                i64::try_from(position).unwrap_or(i64::MAX)
            };
            values.push(UsmapEnumValue {
                name: read_name(cursor, names)?,
                value,
            });
        }
        enums.push(UsmapEnum { name, values });
    }
    Ok(enums)
}

fn read_structs(cursor: &mut Cursor<'_>, names: &[String]) -> Result<Vec<UsmapStruct>, UsmapError> {
    let count = cursor.count("struct", STRUCT_HEADER_SIZE)?;
    let mut structs = Vec::with_capacity(count);
    for _ in 0..count {
        let name = read_name(cursor, names)?;
        let super_offset = cursor.offset();
        let super_index = cursor.u32()?;
        let super_name = if super_index == NO_NAME {
            None
        } else {
            Some(resolve(names, super_index, super_offset)?.clone())
        };
        let property_count = cursor.u16()?;
        let listed_offset = cursor.offset();
        let listed = u64::from(cursor.u16()?);
        let listed = cursor.check_count("property", listed, listed_offset, PROPERTY_HEADER_SIZE)?;
        let mut properties = Vec::with_capacity(listed);
        for _ in 0..listed {
            let index = cursor.u16()?;
            let array_dim = cursor.u8()?;
            let name = read_name(cursor, names)?;
            let ty = read_type(cursor, names, 0)?;
            properties.push(UsmapProperty {
                name,
                index,
                array_dim,
                ty,
            });
        }
        structs.push(UsmapStruct {
            name,
            super_name,
            property_count,
            properties,
        });
    }
    Ok(structs)
}

fn read_type(
    cursor: &mut Cursor<'_>,
    names: &[String],
    depth: usize,
) -> Result<UsmapType, UsmapError> {
    let offset = cursor.offset();
    if depth >= MAX_TYPE_DEPTH {
        return Err(UsmapError::TypeNestingTooDeep {
            offset,
            limit: MAX_TYPE_DEPTH,
        });
    }
    let kind = cursor.u8()?;
    let next = depth + 1;
    let ty = match kind {
        tag::BYTE => UsmapType::Byte,
        tag::BOOL => UsmapType::Bool,
        tag::INT => UsmapType::Int32,
        tag::FLOAT => UsmapType::Float,
        tag::OBJECT => UsmapType::Object,
        tag::NAME => UsmapType::Name,
        tag::DELEGATE => UsmapType::Delegate,
        tag::DOUBLE => UsmapType::Double,
        tag::ARRAY => UsmapType::Array {
            inner: Box::new(read_type(cursor, names, next)?),
        },
        tag::STRUCT => UsmapType::Struct {
            name: read_name(cursor, names)?,
        },
        tag::STR => UsmapType::Str,
        tag::TEXT => UsmapType::Text,
        tag::INTERFACE => UsmapType::Interface,
        tag::MULTICAST_DELEGATE => UsmapType::MulticastDelegate,
        tag::WEAK_OBJECT => UsmapType::WeakObject,
        tag::LAZY_OBJECT => UsmapType::LazyObject,
        tag::ASSET_OBJECT => UsmapType::AssetObject,
        tag::SOFT_OBJECT => UsmapType::SoftObject,
        tag::UINT64 => UsmapType::UInt64,
        tag::UINT32 => UsmapType::UInt32,
        tag::UINT16 => UsmapType::UInt16,
        tag::INT64 => UsmapType::Int64,
        tag::INT16 => UsmapType::Int16,
        tag::INT8 => UsmapType::Int8,
        tag::MAP => {
            let key = Box::new(read_type(cursor, names, next)?);
            let value = Box::new(read_type(cursor, names, next)?);
            UsmapType::Map { key, value }
        }
        tag::SET => UsmapType::Set {
            element: Box::new(read_type(cursor, names, next)?),
        },
        tag::ENUM => {
            let inner = Box::new(read_type(cursor, names, next)?);
            let name = read_name(cursor, names)?;
            UsmapType::Enum { inner, name }
        }
        tag::FIELD_PATH => UsmapType::FieldPath,
        tag::OPTIONAL => UsmapType::Optional {
            inner: Box::new(read_type(cursor, names, next)?),
        },
        tag::UTF8_STR => UsmapType::Utf8Str,
        tag::ANSI_STR => UsmapType::AnsiStr,
        tag::UNKNOWN => UsmapType::Unknown,
        other => return Err(UsmapError::UnknownPropertyType { tag: other, offset }),
    };
    Ok(ty)
}
