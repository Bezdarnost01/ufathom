//! A complete walk over a fixture with every view of this crate, recording
//! results instead of stopping at errors, and the comparison against truth.

#![allow(
    dead_code,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "each test binary uses a different part of this module; test helpers may panic"
)]

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::sync::mpsc;
use std::time::Duration;

use rayon::ThreadPool;
use ufathom_engine::{
    EngineError, FunctionView, LoadOptions, NameTable, ObjectArrayView, ObjectHeader, ObjectSlot,
    PropertySystemReader, RawProperty, StructView, for_layout, object_path, property_type,
    read_enum_entries, read_function, read_header, read_struct, short_enum_value_name, super_chain,
};
use ufathom_mem::Addr;
use ufathom_model::{FieldDef, FunctionDef, PropertyType, StructDef};
use ufathom_testkit::{Fixture, FixtureBuilder, Profile};

/// The members of one owner, decoded.
#[derive(Default)]
pub struct FieldWalk {
    pub raw: Vec<RawProperty>,
    pub types: Vec<PropertyType>,
    pub errors: Vec<EngineError>,
}

pub struct FunctionWalk {
    pub addr: Addr,
    pub path: Result<String, EngineError>,
    pub view: Result<FunctionView, EngineError>,
    pub body: Result<StructView, EngineError>,
    pub params: FieldWalk,
}

pub struct StructWalk {
    pub addr: Addr,
    pub view: Result<StructView, EngineError>,
    pub super_path: Option<Result<String, EngineError>>,
    pub fields: FieldWalk,
    pub function_errors: Vec<EngineError>,
    pub functions: Vec<FunctionWalk>,
    pub super_chain: Vec<Result<Addr, EngineError>>,
}

/// Everything a full pass over one fixture produced.
pub struct Walk {
    pub names: Result<NameTable, EngineError>,
    pub array: Result<(), EngineError>,
    pub num: u32,
    pub null_slots: u32,
    pub chunk_errors: Vec<EngineError>,
    pub slots: Vec<ObjectSlot>,
    pub headers: BTreeMap<u32, ObjectHeader>,
    pub header_errors: Vec<(u32, EngineError)>,
    /// Slots whose object stores a different index.
    pub rejected: Vec<u32>,
    pub paths: BTreeMap<u32, String>,
    pub path_errors: Vec<(u32, EngineError)>,
    /// Truth struct path to its walk.
    pub structs: BTreeMap<String, StructWalk>,
    /// Truth enum path to its entries (prefix stripped, sorted by value).
    pub enums: BTreeMap<String, Result<Vec<(String, i64)>, EngineError>>,
}

pub fn small(profile: Profile) -> Fixture {
    FixtureBuilder::new(profile)
        .seed(3)
        .filler_objects(150)
        .build()
}

fn walk_fields(
    system: &dyn PropertySystemReader,
    owner: Addr,
    path_of: &dyn Fn(Addr) -> Option<String>,
) -> FieldWalk {
    let mut out = FieldWalk::default();
    for item in system.fields(owner) {
        match item.and_then(|field| system.decode(field)) {
            Ok(raw) => {
                out.types.push(property_type(system, &raw, path_of));
                out.raw.push(raw);
            }
            Err(error) => out.errors.push(error),
        }
    }
    out
}

/// Walks everything. Never panics on target data.
pub fn walk(fx: &Fixture, pool: Option<&ThreadPool>) -> Walk {
    let layout = &fx.layout;
    let mem = &fx.memory;
    let options = LoadOptions::for_layout(layout, pool);
    let names = NameTable::load(mem, &layout.names, &options);
    let mut out = Walk {
        names: Err(EngineError::Malformed {
            what: "placeholder",
            reason: String::new(),
        }),
        array: Ok(()),
        num: 0,
        null_slots: 0,
        chunk_errors: Vec::new(),
        slots: Vec::new(),
        headers: BTreeMap::new(),
        header_errors: Vec::new(),
        rejected: Vec::new(),
        paths: BTreeMap::new(),
        path_errors: Vec::new(),
        structs: BTreeMap::new(),
        enums: BTreeMap::new(),
    };
    // An empty table stands in for a failed load so the rest of the walk
    // shows what an unresolvable name does everywhere.
    let table = match &names {
        Ok(_) => None,
        Err(_) => Some(NameTable::default()),
    };
    let names_ref: &NameTable = match (&names, &table) {
        (Ok(names), _) => names,
        (Err(_), Some(empty)) => empty,
        (Err(_), None) => unreachable!("a failed load has an empty stand-in"),
    };

    match ObjectArrayView::new(mem, &layout.objects) {
        Err(error) => out.array = Err(error),
        Ok(view) => {
            out.num = view.num();
            for chunk in 0..view.chunk_count() {
                match view.objects_in_chunk(chunk) {
                    Ok(chunk) => {
                        out.null_slots += chunk.null_slots;
                        out.slots.extend(chunk.objects);
                    }
                    Err(error) => out.chunk_errors.push(error),
                }
            }
        }
    }
    for slot in &out.slots {
        match read_header(mem, &layout.uobject, &layout.fname, slot.object) {
            Ok(header) => {
                if header.expect_index(slot.index).is_err() {
                    out.rejected.push(slot.index);
                }
                out.headers.insert(slot.index, header);
            }
            Err(error) => out.header_errors.push((slot.index, error)),
        }
        match object_path(mem, names_ref, &layout.uobject, &layout.fname, slot.object) {
            Ok(path) => {
                out.paths.insert(slot.index, path);
            }
            Err(error) => out.path_errors.push((slot.index, error)),
        }
    }

    let system = for_layout(mem, layout, names_ref);
    let cache: RefCell<HashMap<Addr, Option<String>>> = RefCell::new(HashMap::new());
    let path_of = |addr: Addr| -> Option<String> {
        cache
            .borrow_mut()
            .entry(addr)
            .or_insert_with(|| {
                object_path(mem, names_ref, &layout.uobject, &layout.fname, addr).ok()
            })
            .clone()
    };
    for def in &fx.truth.structs {
        let Some(record) = fx.object(&def.path) else {
            continue;
        };
        let addr = record.address;
        let view = read_struct(mem, &layout.ustruct, addr);
        let super_path = view.as_ref().ok().and_then(|v| {
            (v.super_struct != 0).then(|| {
                object_path(
                    mem,
                    names_ref,
                    &layout.uobject,
                    &layout.fname,
                    v.super_struct,
                )
            })
        });
        let fields = walk_fields(system.as_ref(), addr, &path_of);
        let mut function_errors = Vec::new();
        let mut functions = Vec::new();
        for item in system.functions(addr) {
            match item {
                Err(error) => function_errors.push(error),
                Ok(function) => functions.push(FunctionWalk {
                    addr: function,
                    path: object_path(mem, names_ref, &layout.uobject, &layout.fname, function),
                    view: read_function(mem, &layout.ufunction, function),
                    body: read_struct(mem, &layout.ustruct, function),
                    params: walk_fields(system.as_ref(), function, &path_of),
                }),
            }
        }
        let chain = super_chain(mem, &layout.ustruct, addr).collect();
        out.structs.insert(
            def.path.clone(),
            StructWalk {
                addr,
                view,
                super_path,
                fields,
                function_errors,
                functions,
                super_chain: chain,
            },
        );
    }
    for def in &fx.truth.enums {
        let Some(record) = fx.object(&def.path) else {
            continue;
        };
        let entries = read_enum_entries(mem, &layout.uenum, &layout.fname, record.address)
            .and_then(|entries| {
                let mut out = Vec::new();
                for entry in entries {
                    let full = names_ref.resolve(&layout.fname, entry.name.as_slice())?;
                    out.push((short_enum_value_name(&full).to_owned(), entry.value));
                }
                out.sort_by(|a, b| (a.1, &a.0).cmp(&(b.1, &b.0)));
                Ok(out)
            });
        out.enums.insert(def.path.clone(), entries);
    }
    drop(system);
    out.names = names;
    out
}

fn sorted_fields(fields: &[FieldDef]) -> Vec<FieldDef> {
    let mut sorted = fields.to_vec();
    sorted.sort_by(|a, b| (a.offset, &a.name).cmp(&(b.offset, &b.name)));
    sorted
}

/// What a decoded member must equal.
pub fn field_of(raw: &RawProperty, ty: &PropertyType) -> FieldDef {
    FieldDef {
        name: raw.name.clone(),
        offset: raw.offset,
        ty: ty.clone(),
        element_size: raw.element_size,
        array_dim: raw.array_dim,
        flags: raw.flags,
    }
}

/// Whether `ty` contains a plain byte somewhere.
fn has_plain_byte(ty: &PropertyType) -> bool {
    match ty {
        PropertyType::UInt8 => true,
        PropertyType::Array { inner } | PropertyType::Optional { inner } => has_plain_byte(inner),
        PropertyType::Set { element } => has_plain_byte(element),
        PropertyType::Map { key, value } => has_plain_byte(key) || has_plain_byte(value),
        PropertyType::Enum { underlying, .. } => has_plain_byte(underlying),
        _ => false,
    }
}

/// Compares decoded members with the truth.
///
/// Known gap in the testkit: a `ByteProperty` without an enum is allocated
/// with no room for its `Enum` pointer, so the pointer slot holds the first
/// bytes of the next allocation. The reader cannot tell, and gives
/// `Unknown`. That outcome is accepted only where the truth contains a plain
/// byte, and every other member must match exactly.
pub fn assert_fields(mine: &[FieldDef], truth: &[FieldDef], label: &str) {
    assert_eq!(mine.len(), truth.len(), "{label}: member count");
    for (got, want) in mine.iter().zip(truth) {
        let location = format!("{label}: {}", want.name);
        assert_eq!(got.name, want.name, "{location}");
        assert_eq!(got.offset, want.offset, "{location}: offset");
        assert_eq!(got.element_size, want.element_size, "{location}: size");
        assert_eq!(got.array_dim, want.array_dim, "{location}: dim");
        assert_eq!(got.flags, want.flags, "{location}: flags");
        let plain_byte_gap =
            matches!(got.ty, PropertyType::Unknown { .. }) && has_plain_byte(&want.ty);
        assert!(
            got.ty == want.ty || plain_byte_gap,
            "{location}: {:?} != {:?}",
            got.ty,
            want.ty
        );
    }
}

pub fn decoded_fields(walk: &FieldWalk) -> Vec<FieldDef> {
    sorted_fields(
        &walk
            .raw
            .iter()
            .zip(&walk.types)
            .map(|(raw, ty)| field_of(raw, ty))
            .collect::<Vec<_>>(),
    )
}

/// Asserts that a clean walk reproduces the truth exactly.
pub fn assert_matches_truth(fx: &Fixture, walk: &Walk, label: &str) {
    let names = walk
        .names
        .as_ref()
        .unwrap_or_else(|e| panic!("{label}: {e}"));
    assert!(walk.array.is_ok(), "{label}: {:?}", walk.array);
    assert!(walk.chunk_errors.is_empty(), "{label}");
    assert!(walk.header_errors.is_empty(), "{label}");
    let stats = names.stats();
    assert!(stats.unreadable_blocks.is_empty(), "{label}: {stats:?}");
    assert!(stats.damaged_blocks.is_empty(), "{label}: {stats:?}");
    assert_eq!(stats.unreadable_entries, 0, "{label}");
    assert!(names.len() > 100, "{label}");

    // Objects: every record is found at its slot with its address and path.
    assert_eq!(
        walk.slots.len(),
        fx.objects.len(),
        "{label}: occupied slots"
    );
    assert_eq!(
        u64::from(walk.null_slots) + walk.slots.len() as u64,
        u64::from(walk.num),
        "{label}"
    );
    assert!(walk.rejected.is_empty(), "{label}: {:?}", walk.rejected);
    let property_objects = !fx.profile.is_modern();
    for record in &fx.objects {
        let slot = walk
            .slots
            .iter()
            .find(|slot| slot.index == record.index)
            .unwrap_or_else(|| panic!("{label}: slot {} of {}", record.index, record.path));
        assert_eq!(slot.object, record.address, "{label}: {}", record.path);
        let path = walk
            .paths
            .get(&record.index)
            .unwrap_or_else(|| panic!("{label}: no path for {}", record.path));
        // The testkit joins the members of a function with `:`, where the
        // engine prints `.` after the second level. Legacy property objects
        // of functions are the only objects affected.
        let function_member = property_objects
            && record.class_path.ends_with("Property")
            && record.path.matches(':').count() >= 2;
        if function_member {
            assert_eq!(
                path.replace(':', "."),
                record.path.replace(':', "."),
                "{label}"
            );
        } else {
            assert_eq!(path, &record.path, "{label}");
        }
    }

    // Structs and functions.
    for def in &fx.truth.structs {
        let found = walk
            .structs
            .get(&def.path)
            .unwrap_or_else(|| panic!("{label}: {} not walked", def.path));
        assert_struct(fx, found, def, label);
    }
    // Enums.
    for def in &fx.truth.enums {
        let entries = walk
            .enums
            .get(&def.path)
            .unwrap_or_else(|| panic!("{label}: {} not walked", def.path))
            .as_ref()
            .unwrap_or_else(|e| panic!("{label}: {}: {e}", def.path));
        let mut expected: Vec<(String, i64)> = def
            .values
            .iter()
            .map(|v| (v.name.clone(), v.value))
            .collect();
        expected.sort_by(|a, b| (a.1, &a.0).cmp(&(b.1, &b.0)));
        assert_eq!(entries, &expected, "{label}: enum {}", def.path);
    }
}

fn assert_struct(fx: &Fixture, found: &StructWalk, def: &StructDef, label: &str) {
    let path = &def.path;
    let view = found
        .view
        .as_ref()
        .unwrap_or_else(|e| panic!("{label}: {path}: {e}"));
    assert_eq!(view.properties_size, def.size, "{label}: {path} size");
    assert_eq!(
        view.min_alignment, def.alignment,
        "{label}: {path} alignment"
    );
    match (&def.super_path, &found.super_path) {
        (None, None) => assert_eq!(view.super_struct, 0, "{label}: {path}"),
        (Some(expected), Some(Ok(actual))) => {
            assert_eq!(actual, expected, "{label}: {path} super")
        }
        other => panic!("{label}: {path} super mismatch: {other:?}"),
    }
    assert!(
        found.fields.errors.is_empty(),
        "{label}: {path}: {:?}",
        found.fields.errors
    );
    assert_fields(
        &decoded_fields(&found.fields),
        &sorted_fields(&def.fields),
        &format!("{label}: {path} fields"),
    );
    assert!(
        ufathom_engine::validate_members(def.size, &found.fields.raw).is_empty(),
        "{label}: {path} members"
    );
    assert!(found.function_errors.is_empty(), "{label}: {path}");
    let mut got: Vec<&FunctionWalk> = found.functions.iter().collect();
    got.sort_by(|a, b| a.path.as_ref().ok().cmp(&b.path.as_ref().ok()));
    let mut expected: Vec<&FunctionDef> = def.functions.iter().collect();
    expected.sort_by(|a, b| a.path.cmp(&b.path));
    assert_eq!(got.len(), expected.len(), "{label}: {path} functions");
    for (function, truth) in got.into_iter().zip(expected) {
        assert_function(fx, function, truth, label);
    }
    let chain: Vec<Addr> = found
        .super_chain
        .iter()
        .map(|item| {
            *item
                .as_ref()
                .unwrap_or_else(|e| panic!("{label}: {path}: {e}"))
        })
        .collect();
    assert_eq!(chain.first().copied(), Some(found.addr), "{label}: {path}");
    let depth = {
        let mut depth = 1;
        let mut at = def.super_path.as_deref();
        while let Some(parent) = at {
            depth += 1;
            at = fx
                .truth
                .structs
                .iter()
                .find(|s| s.path == parent)
                .and_then(|s| s.super_path.as_deref());
        }
        depth
    };
    assert_eq!(chain.len(), depth, "{label}: {path} chain");
}

fn assert_function(fx: &Fixture, function: &FunctionWalk, truth: &FunctionDef, label: &str) {
    let path = &truth.path;
    assert_eq!(function.path.as_ref().unwrap(), path, "{label}");
    let view = function
        .view
        .as_ref()
        .unwrap_or_else(|e| panic!("{label}: {path}: {e}"));
    assert_eq!(view.flags, truth.flags, "{label}: {path} flags");
    let rva = view.native_func.map(|addr| addr - fx.addresses.module_base);
    assert_eq!(rva, truth.native_rva, "{label}: {path} native");
    let body = function
        .body
        .as_ref()
        .unwrap_or_else(|e| panic!("{label}: {path}: {e}"));
    assert_eq!(body.properties_size, truth.params_size, "{label}: {path}");
    assert!(function.params.errors.is_empty(), "{label}: {path}");
    assert_fields(
        &decoded_fields(&function.params),
        &sorted_fields(&truth.params),
        &format!("{label}: {path} params"),
    );
}

/// Runs `job` on its own thread and fails the test when it does not finish
/// within `seconds`, instead of hanging the test run.
pub fn with_watchdog<T: Send + 'static>(
    label: &str,
    seconds: u64,
    job: impl FnOnce() -> T + Send + 'static,
) -> T {
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = sender.send(job());
    });
    match receiver.recv_timeout(Duration::from_secs(seconds)) {
        Ok(value) => value,
        Err(mpsc::RecvTimeoutError::Timeout) => panic!("{label}: did not finish in {seconds}s"),
        Err(mpsc::RecvTimeoutError::Disconnected) => panic!("{label}: the walk panicked"),
    }
}

pub fn shared(fx: Fixture) -> Arc<Fixture> {
    Arc::new(fx)
}

/// The kinds of type decoded without falling back to `Unknown`, over every
/// struct and function of a walk, and how many members fell back.
pub fn type_tally(walk: &Walk) -> (std::collections::BTreeSet<String>, usize, usize) {
    let mut kinds = std::collections::BTreeSet::new();
    let (mut total, mut unknown) = (0, 0);
    let mut note = |fields: &FieldWalk| {
        for ty in &fields.types {
            total += 1;
            match ty {
                PropertyType::Unknown { .. } => unknown += 1,
                other => {
                    let text = format!("{other:?}");
                    let end = text.find([' ', '(']).unwrap_or(text.len());
                    kinds.insert(text[..end].to_owned());
                }
            }
        }
    };
    for found in walk.structs.values() {
        note(&found.fields);
        for function in &found.functions {
            note(&function.params);
        }
    }
    (kinds, total, unknown)
}

/// A hand-built address space: one zeroed readable region, written byte by
/// byte, plus a module so that `Rva` locations resolve.
pub struct Img {
    pub base: u64,
    bytes: Vec<u8>,
}

impl Img {
    pub fn new(base: u64, size: usize) -> Self {
        Self {
            base,
            bytes: vec![0; size],
        }
    }

    pub fn put(&mut self, addr: u64, data: &[u8]) {
        let at = usize::try_from(addr - self.base).unwrap();
        self.bytes[at..at + data.len()].copy_from_slice(data);
    }

    pub fn u16(&mut self, addr: u64, value: u16) {
        self.put(addr, &value.to_le_bytes());
    }

    pub fn u32(&mut self, addr: u64, value: u32) {
        self.put(addr, &value.to_le_bytes());
    }

    pub fn u64(&mut self, addr: u64, value: u64) {
        self.put(addr, &value.to_le_bytes());
    }

    pub fn snapshot(&self, module: &str) -> ufathom_mem::Snapshot {
        ufathom_mem::Snapshot::builder()
            .region(self.base, self.bytes.clone())
            .module(module, self.base, self.bytes.len() as u64)
            .build()
            .unwrap()
    }
}

/// The same image as `fx`, with some bytes replaced.
pub fn patched(fx: &Fixture, patches: &[(u64, Vec<u8>)]) -> ufathom_mem::Snapshot {
    let mut builder = ufathom_mem::Snapshot::builder();
    for part in &fx.parts {
        let mut bytes = part.bytes.clone();
        for (addr, data) in patches {
            if *addr >= part.addr && *addr + data.len() as u64 <= part.addr + bytes.len() as u64 {
                let at = (*addr - part.addr) as usize;
                bytes[at..at + data.len()].copy_from_slice(data);
            }
        }
        builder = builder.region_with(part.addr, bytes, part.perms, part.region_kind);
    }
    builder
        .module(fx.module_name(), fx.addresses.module_base, 0x100_0000)
        .build()
        .unwrap()
}
