//! Builds a [`Usmap`] from a dump, recording everything usmap cannot hold.

use std::collections::BTreeSet;

use serde::Serialize;
use ufathom_model::{Dump, EnumDef, FieldDef, StructDef};

use crate::mapping::short_name;
use crate::model::{
    Compression, Usmap, UsmapEnum, UsmapEnumValue, UsmapProperty, UsmapStruct, UsmapType,
    UsmapVersion,
};

/// Longest name the format can store: a 16-bit byte length.
const MAX_NAME_BYTES: usize = u16::MAX as usize;
/// Most enumerators one enum can hold: a 16-bit count.
const MAX_ENUM_VALUES: usize = u16::MAX as usize;
/// Most properties one struct can list: a 16-bit count.
const MAX_PROPERTIES: usize = u16::MAX as usize;
/// Largest array dimension the format can store: one byte.
const MAX_ARRAY_DIM: u32 = u8::MAX as u32;

/// What [`write_usmap_with_report`](crate::write_usmap_with_report) left out
/// or changed because usmap cannot express it. Every list is sorted.
///
/// Information that usmap has no place for at all (functions, sizes,
/// offsets, flags, packages, object classes, bool bits) is not listed here:
/// it is dropped for every type, and the crate documentation says so.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct WriteReport {
    /// Paths of structs and classes left out because an earlier path (in
    /// sorted order) has the same short name. usmap names types without a
    /// package, and a name that appears twice is ambiguous for every reader.
    pub duplicate_structs_skipped: Vec<String>,
    /// Paths of enums left out for the same reason.
    pub duplicate_enums_skipped: Vec<String>,
    /// `Struct.Field` of properties whose type, or a type nested in it, was
    /// unknown to the dump and is written with the `Unknown` tag.
    pub unknown_property_types: Vec<String>,
    /// `Struct.Field` of C arrays with more than 255 elements, written with
    /// dimension 255.
    pub array_dims_clamped: Vec<String>,
    /// Names, enumerators or properties dropped or shortened to fit a 16-bit
    /// length or count, described by owner.
    pub limits_exceeded: Vec<String>,
    /// Number of functions in the dump. usmap describes data members only.
    pub functions_omitted: usize,
}

/// Accumulates the model and the report.
struct Builder {
    report: WriteReport,
}

impl Builder {
    /// Fits a name into the format's 16-bit length, cutting at a character
    /// boundary and recording the cut.
    fn name(&mut self, name: &str, owner: &str) -> String {
        if name.len() <= MAX_NAME_BYTES {
            return name.to_owned();
        }
        let mut end = MAX_NAME_BYTES;
        while !name.is_char_boundary(end) {
            end -= 1;
        }
        self.report
            .limits_exceeded
            .push(format!("{owner}: name of {} bytes shortened", name.len()));
        name.get(..end).unwrap_or_default().to_owned()
    }

    /// Applies [`Builder::name`] to every name inside a type.
    fn type_names(&mut self, ty: UsmapType, owner: &str) -> UsmapType {
        match ty {
            UsmapType::Struct { name } => UsmapType::Struct {
                name: self.name(&name, owner),
            },
            UsmapType::Enum { inner, name } => UsmapType::Enum {
                inner: Box::new(self.type_names(*inner, owner)),
                name: self.name(&name, owner),
            },
            UsmapType::Array { inner } => UsmapType::Array {
                inner: Box::new(self.type_names(*inner, owner)),
            },
            UsmapType::Optional { inner } => UsmapType::Optional {
                inner: Box::new(self.type_names(*inner, owner)),
            },
            UsmapType::Set { element } => UsmapType::Set {
                element: Box::new(self.type_names(*element, owner)),
            },
            UsmapType::Map { key, value } => UsmapType::Map {
                key: Box::new(self.type_names(*key, owner)),
                value: Box::new(self.type_names(*value, owner)),
            },
            other => other,
        }
    }

    fn property(&mut self, owner: &StructDef, field: &FieldDef, index: u32) -> UsmapProperty {
        let label = format!("{}.{}", owner.name, field.name);
        let ty = UsmapType::from_property_type(&field.ty);
        if ty.contains_unknown() {
            self.report.unknown_property_types.push(label.clone());
        }
        if field.array_dim > MAX_ARRAY_DIM {
            self.report
                .array_dims_clamped
                .push(format!("{label} ({})", field.array_dim));
        }
        let array_dim = u8::try_from(field.array_dim.clamp(1, MAX_ARRAY_DIM)).unwrap_or(u8::MAX);
        UsmapProperty {
            name: self.name(&field.name, &label),
            index: u16::try_from(index).unwrap_or(u16::MAX),
            array_dim,
            ty: self.type_names(ty, &label),
        }
    }

    fn structure(&mut self, def: &StructDef) -> UsmapStruct {
        let mut fields: Vec<&FieldDef> = def.fields.iter().collect();
        fields.sort_by(|a, b| {
            a.offset
                .cmp(&b.offset)
                .then_with(|| a.name.cmp(&b.name))
                .then_with(|| a.cmp(b))
        });
        if fields.len() > MAX_PROPERTIES {
            self.report.limits_exceeded.push(format!(
                "{}: {} properties listed, {} dropped",
                def.name,
                MAX_PROPERTIES,
                fields.len() - MAX_PROPERTIES
            ));
            fields.truncate(MAX_PROPERTIES);
        }

        let mut slot: u32 = 0;
        let mut properties = Vec::with_capacity(fields.len());
        for field in fields {
            let property = self.property(def, field, slot);
            slot = slot.saturating_add(u32::from(property.array_dim));
            properties.push(property);
        }
        UsmapStruct {
            name: self.name(&def.name, &def.path),
            super_name: def
                .super_path
                .as_deref()
                .map(|path| self.name(short_name(path), &def.path)),
            property_count: u16::try_from(slot).unwrap_or(u16::MAX),
            properties,
        }
    }

    fn enumeration(&mut self, def: &EnumDef) -> UsmapEnum {
        let mut values: Vec<_> = def.values.iter().collect();
        values.sort_by(|a, b| a.value.cmp(&b.value).then_with(|| a.name.cmp(&b.name)));
        if values.len() > MAX_ENUM_VALUES {
            self.report.limits_exceeded.push(format!(
                "{}: {} enumerators listed, {} dropped",
                def.name,
                MAX_ENUM_VALUES,
                values.len() - MAX_ENUM_VALUES
            ));
            values.truncate(MAX_ENUM_VALUES);
        }
        UsmapEnum {
            name: self.name(&def.name, &def.path),
            values: values
                .into_iter()
                .map(|value| UsmapEnumValue {
                    name: self.name(&value.name, &def.path),
                    value: value.value,
                })
                .collect(),
        }
    }
}

/// Converts a dump into a model at [`UsmapVersion::LATEST`], uncompressed.
///
/// The `names` of the result are empty: the encoder builds the name table in
/// the order it writes names.
pub(crate) fn build_usmap(dump: &Dump) -> (Usmap, WriteReport) {
    let mut builder = Builder {
        report: WriteReport {
            functions_omitted: dump.structs.iter().map(|def| def.functions.len()).sum(),
            ..WriteReport::default()
        },
    };

    // Sorting by path first makes the output independent of dump order and
    // makes "the first of a duplicate name" a stable choice.
    let mut struct_defs: Vec<&StructDef> = dump.structs.iter().collect();
    struct_defs.sort_by(|a, b| a.path.cmp(&b.path).then_with(|| a.cmp(b)));
    let mut seen = BTreeSet::new();
    let mut structs = Vec::new();
    for def in struct_defs {
        if seen.insert(def.name.as_str()) {
            structs.push(builder.structure(def));
        } else {
            builder
                .report
                .duplicate_structs_skipped
                .push(def.path.clone());
        }
    }

    let mut enum_defs: Vec<&EnumDef> = dump.enums.iter().collect();
    enum_defs.sort_by(|a, b| a.path.cmp(&b.path).then_with(|| a.cmp(b)));
    let mut seen = BTreeSet::new();
    let mut enums = Vec::new();
    for def in enum_defs {
        if seen.insert(def.name.as_str()) {
            enums.push(builder.enumeration(def));
        } else {
            builder
                .report
                .duplicate_enums_skipped
                .push(def.path.clone());
        }
    }

    let mut report = builder.report;
    report.unknown_property_types.sort();
    report.array_dims_clamped.sort();
    report.limits_exceeded.sort();

    let usmap = Usmap {
        version: UsmapVersion::LATEST,
        compression: Compression::None,
        version_info: None,
        names: Vec::new(),
        enums,
        structs,
        extension_bytes: 0,
    };
    (usmap, report)
}
