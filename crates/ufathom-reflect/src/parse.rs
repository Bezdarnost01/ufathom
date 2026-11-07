//! Reading one type: a struct, a class or an enum.

use std::collections::HashMap;

use ufathom_engine::limits::MAX_PROPERTY_DEPTH;
use ufathom_engine::{
    EngineError, NameTable, ObjectArrayView, PropertyKind, PropertySystemReader, RawProperty,
    property_type, read_enum_entries, read_function, read_header, read_struct, validate_members,
};
use ufathom_mem::{Addr, MemoryReader};
use ufathom_model::flags::FUNC_NATIVE;
use ufathom_model::{
    Diagnostic, EngineLayout, EnumDef, EnumValue, FieldDef, FunctionDef, PropertyType, Severity,
    StructDef, StructKind,
};

use crate::classify::Kind;
use crate::ctx::diagnostic;
use crate::objects::{Entry, ObjectTable};

/// The module whose exec thunks are reported as RVAs.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ModuleSpan {
    pub(crate) base: Addr,
    pub(crate) size: u64,
}

impl ModuleSpan {
    fn rva_of(&self, addr: Addr) -> Option<u64> {
        addr.checked_sub(self.base).filter(|rva| *rva < self.size)
    }
}

/// Everything a type needs to be read, shared by all workers.
pub(crate) struct Env<'a> {
    pub(crate) layout: &'a EngineLayout,
    pub(crate) names: &'a NameTable,
    pub(crate) table: &'a ObjectTable,
    /// Path of every classified type and function, by address.
    pub(crate) paths: &'a HashMap<Addr, String>,
    pub(crate) view: &'a ObjectArrayView<'a>,
    pub(crate) raw: &'a dyn MemoryReader,
    pub(crate) module: Option<ModuleSpan>,
}

/// One way to read the target: a memory reader and the property reader on it.
pub(crate) struct Reader<'a> {
    pub(crate) mem: &'a dyn MemoryReader,
    pub(crate) props: Box<dyn PropertySystemReader + 'a>,
}

/// A type read from the target.
#[derive(Debug)]
pub(crate) enum Item {
    Struct(StructDef),
    Enum(EnumDef),
}

/// What reading one type produced.
#[derive(Debug)]
pub(crate) struct Parsed {
    pub(crate) item: Item,
    pub(crate) diagnostics: Vec<Diagnostic>,
    /// `(enum path, size of the underlying integer)` of every `EnumProperty`
    /// met in the type's members and parameters.
    pub(crate) enum_uses: Vec<(String, u32)>,
    pub(crate) functions: u64,
}

/// Reads the type at `entry`.
pub(crate) fn parse_type(
    env: &Env<'_>,
    reader: &Reader<'_>,
    entry: &Entry,
    kind: Kind,
    path: &str,
) -> Result<Parsed, EngineError> {
    match kind {
        Kind::Class => parse_struct(env, reader, entry, StructKind::Class, path),
        Kind::ScriptStruct => parse_struct(env, reader, entry, StructKind::ScriptStruct, path),
        _ => parse_enum(env, reader, entry, path),
    }
}

/// The part of `path` after the last `.` or `:`.
fn short_name(path: &str) -> &str {
    path.rsplit(['.', ':']).next().unwrap_or(path)
}

/// The package of a top-level object: the part of its path before the first
/// `.`, which is the name of the outermost object.
fn package_of(path: &str) -> &str {
    path.split_once('.').map_or(path, |(package, _)| package)
}

fn parse_struct(
    env: &Env<'_>,
    reader: &Reader<'_>,
    entry: &Entry,
    kind: StructKind,
    path: &str,
) -> Result<Parsed, EngineError> {
    let addr = entry.header.addr;
    let view = read_struct(reader.mem, &env.layout.ustruct, addr)?;
    let mut diagnostics = Vec::new();
    let mut enum_uses = Vec::new();

    let members = collect_members(reader, addr, path, &mut diagnostics);
    for problem in validate_members(view.properties_size, &members) {
        diagnostics.push(diagnostic(path, Severity::Warning, problem.to_string()));
    }
    let fields = to_fields(env, reader, members, path, &mut diagnostics, &mut enum_uses);

    let mut functions = Vec::new();
    if kind == StructKind::Class {
        for item in reader.props.functions(addr) {
            match item {
                Ok(function) => {
                    if let Some(def) = parse_function(
                        env,
                        reader,
                        function,
                        path,
                        &mut diagnostics,
                        &mut enum_uses,
                    ) {
                        functions.push(def);
                    }
                }
                Err(error) => diagnostics.push(list_problem(path, "function list", &error)),
            }
        }
    }

    let super_path = match view.super_struct {
        0 => None,
        parent => {
            let found = env.paths.get(&parent).cloned();
            if found.is_none() {
                diagnostics.push(diagnostic(
                    path,
                    Severity::Warning,
                    format!(
                        "super struct at {parent:#x} is not a known type, the parent is left out"
                    ),
                ));
            }
            found
        }
    };
    let function_count = functions.len() as u64;
    Ok(Parsed {
        item: Item::Struct(StructDef {
            path: path.to_owned(),
            name: short_name(path).to_owned(),
            package: package_of(path).to_owned(),
            kind,
            super_path,
            size: view.properties_size,
            alignment: view.min_alignment,
            fields,
            functions,
            // The layout has no offset for ClassFlags or StructFlags.
            flags: 0,
        }),
        diagnostics,
        enum_uses,
        functions: function_count,
    })
}

/// A problem with a member list. A cycle is not a loss: every member reachable
/// was seen once, which is all there is.
fn list_problem(path: &str, what: &str, error: &EngineError) -> Diagnostic {
    let severity = if matches!(error.root(), EngineError::Cycle { .. }) {
        Severity::Warning
    } else {
        Severity::Error
    };
    diagnostic(path, severity, format!("{what}: {error}"))
}

/// Decodes the properties of `owner`, in chain order.
fn collect_members(
    reader: &Reader<'_>,
    owner: Addr,
    path: &str,
    diagnostics: &mut Vec<Diagnostic>,
) -> Vec<RawProperty> {
    let mut members = Vec::new();
    for item in reader.props.fields(owner) {
        match item {
            Ok(field) => match reader.props.decode(field) {
                Ok(raw) => members.push(raw),
                Err(error) => diagnostics.push(diagnostic(
                    path,
                    Severity::Error,
                    format!("member at {field:#x} cannot be decoded: {error}"),
                )),
            },
            Err(error) => diagnostics.push(list_problem(path, "member list", &error)),
        }
    }
    members
}

/// Converts decoded members to fields. An unresolved reference is reported and
/// left as [`PropertyType::Unknown`].
fn to_fields(
    env: &Env<'_>,
    reader: &Reader<'_>,
    members: Vec<RawProperty>,
    path: &str,
    diagnostics: &mut Vec<Diagnostic>,
    enum_uses: &mut Vec<(String, u32)>,
) -> Vec<FieldDef> {
    let path_of = |addr: Addr| env.paths.get(&addr).cloned();
    let mut fields = Vec::with_capacity(members.len());
    for raw in members {
        let ty = property_type(&*reader.props, &raw, &path_of);
        if matches!(ty, PropertyType::Unknown { .. })
            && PropertyKind::from_class_name(&raw.class_name) != PropertyKind::Unknown
        {
            diagnostics.push(diagnostic(
                path,
                Severity::Warning,
                format!(
                    "member {} of class {} refers to something unresolved, its type is unknown",
                    raw.name, raw.class_name
                ),
            ));
        }
        note_enums(&ty, enum_uses, 0);
        fields.push(FieldDef {
            name: raw.name,
            offset: raw.offset,
            ty,
            element_size: raw.element_size,
            array_dim: raw.array_dim,
            flags: raw.flags,
        });
    }
    fields
}

/// Records every `EnumProperty` inside `ty` with the width of its integer.
fn note_enums(ty: &PropertyType, uses: &mut Vec<(String, u32)>, depth: usize) {
    if depth > MAX_PROPERTY_DEPTH {
        return;
    }
    match ty {
        PropertyType::Enum { path, underlying } => {
            if let Some(size) = integer_size(underlying) {
                uses.push((path.clone(), size));
            }
            note_enums(underlying, uses, depth + 1);
        }
        PropertyType::Array { inner } | PropertyType::Optional { inner } => {
            note_enums(inner, uses, depth + 1);
        }
        PropertyType::Set { element } => note_enums(element, uses, depth + 1),
        PropertyType::Map { key, value } => {
            note_enums(key, uses, depth + 1);
            note_enums(value, uses, depth + 1);
        }
        _ => {}
    }
}

fn integer_size(ty: &PropertyType) -> Option<u32> {
    match ty {
        PropertyType::Int8 | PropertyType::UInt8 => Some(1),
        PropertyType::Int16 | PropertyType::UInt16 => Some(2),
        PropertyType::Int32 | PropertyType::UInt32 => Some(4),
        PropertyType::Int64 | PropertyType::UInt64 => Some(8),
        _ => None,
    }
}

fn parse_function(
    env: &Env<'_>,
    reader: &Reader<'_>,
    function: Addr,
    owner_path: &str,
    diagnostics: &mut Vec<Diagnostic>,
    enum_uses: &mut Vec<(String, u32)>,
) -> Option<FunctionDef> {
    let Some(path) = env.paths.get(&function) else {
        diagnostics.push(diagnostic(
            owner_path,
            Severity::Error,
            format!("function at {function:#x} has no path and is left out"),
        ));
        return None;
    };
    let read = read_function(reader.mem, &env.layout.ufunction, function).and_then(|view| {
        read_struct(reader.mem, &env.layout.ustruct, function).map(|owner| (view, owner))
    });
    let (view, owner) = match read {
        Ok(pair) => pair,
        Err(error) => {
            diagnostics.push(diagnostic(
                path,
                Severity::Error,
                format!("function cannot be read and is left out: {error}"),
            ));
            return None;
        }
    };
    let mut members = collect_members(reader, function, path, diagnostics);
    // The chain may run backwards. Parameters are laid out in declaration
    // order, so the offset restores it; the sort is stable for equal offsets.
    members.sort_by_key(|member| member.offset);
    for problem in validate_members(owner.properties_size, &members) {
        diagnostics.push(diagnostic(path, Severity::Warning, problem.to_string()));
    }
    let params = to_fields(env, reader, members, path, diagnostics, enum_uses);
    let native_rva = view
        .native_func
        .filter(|_| view.flags & FUNC_NATIVE != 0)
        .and_then(|func| env.module.and_then(|module| module.rva_of(func)));
    Some(FunctionDef {
        path: path.clone(),
        name: short_name(path).to_owned(),
        flags: view.flags,
        params_size: owner.properties_size,
        params,
        native_rva,
    })
}

fn parse_enum(
    env: &Env<'_>,
    reader: &Reader<'_>,
    entry: &Entry,
    path: &str,
) -> Result<Parsed, EngineError> {
    let entries = read_enum_entries(
        reader.mem,
        &env.layout.uenum,
        &env.layout.fname,
        entry.header.addr,
    )?;
    let name = short_name(path);
    let mut diagnostics = Vec::new();
    let mut values = Vec::with_capacity(entries.len());
    for enumerator in entries {
        match env
            .names
            .resolve(&env.layout.fname, enumerator.name.as_slice())
        {
            Ok(full) => values.push(EnumValue {
                name: strip_enum_prefix(&full, name).to_owned(),
                value: enumerator.value,
            }),
            Err(error) => diagnostics.push(diagnostic(
                path,
                Severity::Error,
                format!(
                    "an enumerator of value {} has no name: {error}",
                    enumerator.value
                ),
            )),
        }
    }
    Ok(Parsed {
        item: Item::Enum(EnumDef {
            path: path.to_owned(),
            name: name.to_owned(),
            package: package_of(path).to_owned(),
            // Replaced once every struct has been read; see `finish_enum_sizes`.
            underlying_size: 1,
            values,
        }),
        diagnostics,
        enum_uses: Vec::new(),
        functions: 0,
    })
}

/// `ENetRole::ROLE_None` becomes `ROLE_None` for the enum `ENetRole`. A name
/// that does not carry this enum's prefix is kept whole.
fn strip_enum_prefix<'a>(full: &'a str, enum_name: &str) -> &'a str {
    match full.rsplit_once("::") {
        Some((prefix, short)) if prefix.eq_ignore_ascii_case(enum_name) => short,
        _ => full,
    }
}

/// Whether the object in `entry`'s slot is still that object.
///
/// Reads the slot and the header directly, never through the cache: the point
/// is to see what the target holds now.
pub(crate) fn still_current(env: &Env<'_>, entry: &Entry) -> bool {
    let header = &entry.header;
    let in_slot = matches!(env.view.object_at(header.index), Ok(Some(addr)) if addr == header.addr);
    in_slot
        && read_header(env.raw, &env.layout.uobject, &env.layout.fname, header.addr)
            .is_ok_and(|now| now.index == header.index && now.class == header.class)
}
