//! Serialization of a [`Usmap`] to bytes at the latest version, uncompressed.

use std::collections::HashMap;

use crate::format::{COMPRESSION_NONE, MAGIC, NO_NAME, tag};
use crate::model::{Usmap, UsmapType, UsmapVersion};

/// Interns names in the order they are first written, so the table is as
/// deterministic as the model.
#[derive(Default)]
struct NameTable {
    names: Vec<String>,
    index: HashMap<String, u32>,
}

impl NameTable {
    fn intern(&mut self, name: &str) -> u32 {
        if let Some(&index) = self.index.get(name) {
            return index;
        }
        let index = u32::try_from(self.names.len()).unwrap_or(u32::MAX);
        self.names.push(name.to_owned());
        self.index.insert(name.to_owned(), index);
        index
    }
}

fn put_u8(out: &mut Vec<u8>, value: u8) {
    out.push(value);
}

fn put_u16(out: &mut Vec<u8>, value: u16) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn put_u32(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn put_i64(out: &mut Vec<u8>, value: i64) {
    out.extend_from_slice(&value.to_le_bytes());
}

/// Writes a length or count that the builder has already fitted into 16 bits.
fn put_len16(out: &mut Vec<u8>, length: usize) {
    put_u16(out, u16::try_from(length).unwrap_or(u16::MAX));
}

fn put_len32(out: &mut Vec<u8>, length: usize) {
    put_u32(out, u32::try_from(length).unwrap_or(u32::MAX));
}

fn put_type(out: &mut Vec<u8>, names: &mut NameTable, ty: &UsmapType) {
    match ty {
        UsmapType::Byte => put_u8(out, tag::BYTE),
        UsmapType::Bool => put_u8(out, tag::BOOL),
        UsmapType::Int32 => put_u8(out, tag::INT),
        UsmapType::Float => put_u8(out, tag::FLOAT),
        UsmapType::Object => put_u8(out, tag::OBJECT),
        UsmapType::Name => put_u8(out, tag::NAME),
        UsmapType::Delegate => put_u8(out, tag::DELEGATE),
        UsmapType::Double => put_u8(out, tag::DOUBLE),
        UsmapType::Array { inner } => {
            put_u8(out, tag::ARRAY);
            put_type(out, names, inner);
        }
        UsmapType::Struct { name } => {
            put_u8(out, tag::STRUCT);
            put_u32(out, names.intern(name));
        }
        UsmapType::Str => put_u8(out, tag::STR),
        UsmapType::Text => put_u8(out, tag::TEXT),
        UsmapType::Interface => put_u8(out, tag::INTERFACE),
        UsmapType::MulticastDelegate => put_u8(out, tag::MULTICAST_DELEGATE),
        UsmapType::WeakObject => put_u8(out, tag::WEAK_OBJECT),
        UsmapType::LazyObject => put_u8(out, tag::LAZY_OBJECT),
        UsmapType::AssetObject => put_u8(out, tag::ASSET_OBJECT),
        UsmapType::SoftObject => put_u8(out, tag::SOFT_OBJECT),
        UsmapType::UInt64 => put_u8(out, tag::UINT64),
        UsmapType::UInt32 => put_u8(out, tag::UINT32),
        UsmapType::UInt16 => put_u8(out, tag::UINT16),
        UsmapType::Int64 => put_u8(out, tag::INT64),
        UsmapType::Int16 => put_u8(out, tag::INT16),
        UsmapType::Int8 => put_u8(out, tag::INT8),
        UsmapType::Map { key, value } => {
            put_u8(out, tag::MAP);
            put_type(out, names, key);
            put_type(out, names, value);
        }
        UsmapType::Set { element } => {
            put_u8(out, tag::SET);
            put_type(out, names, element);
        }
        UsmapType::Enum { inner, name } => {
            put_u8(out, tag::ENUM);
            put_type(out, names, inner);
            put_u32(out, names.intern(name));
        }
        UsmapType::FieldPath => put_u8(out, tag::FIELD_PATH),
        UsmapType::Optional { inner } => {
            put_u8(out, tag::OPTIONAL);
            put_type(out, names, inner);
        }
        UsmapType::Utf8Str => put_u8(out, tag::UTF8_STR),
        UsmapType::AnsiStr => put_u8(out, tag::ANSI_STR),
        UsmapType::Unknown => put_u8(out, tag::UNKNOWN),
    }
}

/// Encodes enums and structs of `usmap` as a version 4, uncompressed file
/// with no package-version block. `usmap.names` and the other header fields
/// are ignored: the table is rebuilt in writing order.
///
/// The model must already fit the format's limits (16-bit counts, 8-bit array
/// dimensions); [`build_usmap`](crate::build::build_usmap) guarantees it.
pub(crate) fn encode(usmap: &Usmap) -> Vec<u8> {
    let mut names = NameTable::default();
    let mut body = Vec::new();

    put_len32(&mut body, usmap.enums.len());
    for def in &usmap.enums {
        put_u32(&mut body, names.intern(&def.name));
        put_len16(&mut body, def.values.len());
        for value in &def.values {
            put_i64(&mut body, value.value);
            put_u32(&mut body, names.intern(&value.name));
        }
    }

    put_len32(&mut body, usmap.structs.len());
    for def in &usmap.structs {
        put_u32(&mut body, names.intern(&def.name));
        let super_index = def
            .super_name
            .as_deref()
            .map_or(NO_NAME, |name| names.intern(name));
        put_u32(&mut body, super_index);
        put_u16(&mut body, def.property_count);
        put_len16(&mut body, def.properties.len());
        for property in &def.properties {
            put_u16(&mut body, property.index);
            put_u8(&mut body, property.array_dim);
            put_u32(&mut body, names.intern(&property.name));
            put_type(&mut body, &mut names, &property.ty);
        }
    }

    let mut payload = Vec::new();
    put_len32(&mut payload, names.names.len());
    for name in &names.names {
        put_len16(&mut payload, name.len());
        payload.extend_from_slice(name.as_bytes());
    }
    payload.extend_from_slice(&body);

    let mut file = Vec::with_capacity(payload.len().saturating_add(16));
    put_u16(&mut file, MAGIC);
    put_u8(&mut file, UsmapVersion::LATEST.as_u8());
    // bHasVersionInfo: the dump records no engine file versions.
    put_u32(&mut file, 0);
    put_u8(&mut file, COMPRESSION_NONE);
    put_len32(&mut file, payload.len());
    put_len32(&mut file, payload.len());
    file.extend_from_slice(&payload);
    file
}
