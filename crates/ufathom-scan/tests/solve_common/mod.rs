//! Shared helpers of the integration tests: build fixtures and compare what
//! `discover` returns with what the fixture was built from.

#![allow(
    dead_code,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::print_stderr,
    reason = "test helpers may panic on failure; each test binary uses a different part"
)]

use ufathom_mem::{MemoryReader, Snapshot};
use ufathom_model::PropertyType;
use ufathom_model::layout::{Location, NameTableLayout};
use ufathom_scan::{DiscoverOptions, Discovery, discover};
use ufathom_testkit::{Fixture, FixtureBuilder, Part, Profile};

/// A fixture whose name table spans three blocks or chunks, so the block
/// offset bits and the chunk size are observable.
pub fn build(profile: Profile, seed: u64, fillers: usize) -> Fixture {
    FixtureBuilder::new(profile)
        .seed(seed)
        .filler_objects(fillers)
        .name_blocks(3)
        .build()
}

/// The absolute address of a location of a fixture.
pub fn absolute(fx: &Fixture, location: &Location) -> u64 {
    match location {
        Location::Absolute(addr) => *addr,
        Location::Rva { rva, .. } => fx.at_rva(*rva),
    }
}

/// Runs `discover` with default options.
pub fn run(fx: &Fixture) -> Discovery {
    discover(&fx.memory, &DiscoverOptions::default())
        .unwrap_or_else(|e| panic!("{}: {e}", fx.profile.name))
}

fn walk(ty: &PropertyType, pred: &dyn Fn(&PropertyType) -> bool) -> bool {
    if pred(ty) {
        return true;
    }
    match ty {
        PropertyType::Array { inner } | PropertyType::Optional { inner } => walk(inner, pred),
        PropertyType::Set { element } => walk(element, pred),
        PropertyType::Map { key, value } => walk(key, pred) || walk(value, pred),
        PropertyType::Enum { underlying, .. } => walk(underlying, pred),
        _ => false,
    }
}

/// Whether the image holds any property of a kind.
fn has_type(fx: &Fixture, pred: impl Fn(&PropertyType) -> bool) -> bool {
    let mut fields = fx.truth.structs.iter().flat_map(|s| {
        s.fields
            .iter()
            .chain(s.functions.iter().flat_map(|f| f.params.iter()))
    });
    fields.any(|f| walk(&f.ty, &pred))
}

fn mentions(d: &Discovery, field: &str) -> bool {
    d.warnings.iter().any(|w| w.contains(field))
}

/// Asserts that `d` equals what `fx` was built from, field by field.
///
/// Locations are compared as absolute field addresses: the start of the
/// object array and of the pool is not observable (the bytes before the
/// first field that the layout names belong to fields it has no use for), so
/// `discover` places each location on its lowest named field.
///
/// Two kinds of field are compared with documented exceptions:
///
/// - `ustruct.min_alignment_width` when the target does not show it,
/// - typed payloads of a property kind that has no instance in the image.
pub fn assert_equal_to_truth(fx: &Fixture, d: &Discovery) {
    compare(fx, d, true);
}

/// Like [`assert_equal_to_truth`] for a damaged image: every essential field
/// is still compared exactly, but a non-essential field may be absent (never
/// different) when the damage hid the objects it is solved from, and the width
/// of enum values may follow the tie-break.
pub fn assert_essential_equal(fx: &Fixture, d: &Discovery) {
    compare(fx, d, false);
}

fn compare(fx: &Fixture, d: &Discovery, strict: bool) {
    let name = fx.profile.name.clone();
    let got = &d.layout;
    let want = &fx.layout;

    // Object array.
    let (g, w) = (&got.objects, &want.objects);
    let (gb, wb) = (absolute(fx, &g.location), absolute(fx, &w.location));
    assert_eq!(
        gb + u64::from(g.objects),
        wb + u64::from(w.objects),
        "{name} objects.objects"
    );
    assert_eq!(
        gb + u64::from(g.max_elements),
        wb + u64::from(w.max_elements),
        "{name} max_elements"
    );
    assert_eq!(
        gb + u64::from(g.num_elements),
        wb + u64::from(w.num_elements),
        "{name} num_elements"
    );
    assert_eq!(
        g.max_chunks.map(|o| gb + u64::from(o)),
        w.max_chunks.map(|o| wb + u64::from(o)),
        "{name} max_chunks"
    );
    assert_eq!(
        g.num_chunks.map(|o| gb + u64::from(o)),
        w.num_chunks.map(|o| wb + u64::from(o)),
        "{name} num_chunks"
    );
    assert_eq!(g.kind, w.kind, "{name} kind");
    assert_eq!(g.item_size, w.item_size, "{name} item_size");
    assert_eq!(g.item_object, w.item_object, "{name} item_object");
    assert_eq!(g.item_serial, w.item_serial, "{name} item_serial");
    assert_eq!(g.pointer_transform, w.pointer_transform, "{name} transform");

    // Names.
    match (&got.names, &want.names) {
        (NameTableLayout::Pool(g), NameTableLayout::Pool(w)) => {
            let (gb, wb) = (absolute(fx, &g.location), absolute(fx, &w.location));
            assert_eq!(
                gb + u64::from(g.blocks),
                wb + u64::from(w.blocks),
                "{name} blocks"
            );
            assert_eq!(
                gb + u64::from(g.current_block),
                wb + u64::from(w.current_block),
                "{name} current"
            );
            assert_eq!(
                gb + u64::from(g.cursor),
                wb + u64::from(w.cursor),
                "{name} cursor"
            );
            assert_eq!(g.stride, w.stride, "{name} stride");
            assert_eq!(g.block_offset_bits, w.block_offset_bits, "{name} bits");
            assert_eq!(g.entry, w.entry, "{name} entry");
        }
        (NameTableLayout::Array(g), NameTableLayout::Array(w)) => {
            assert_eq!(
                absolute(fx, &g.location),
                absolute(fx, &w.location),
                "{name} array location"
            );
            assert_eq!(g.indirect, w.indirect, "{name} indirect");
            assert_eq!(g.num_elements, w.num_elements, "{name} num_elements");
            assert_eq!(g.num_chunks, w.num_chunks, "{name} num_chunks");
            assert_eq!(
                g.elements_per_chunk, w.elements_per_chunk,
                "{name} per chunk"
            );
            assert_eq!(g.entry, w.entry, "{name} entry");
        }
        other => panic!("{name}: name table kind {other:?}"),
    }

    assert_eq!(got.pointer_size, want.pointer_size, "{name}");
    assert_eq!(got.fname, want.fname, "{name} fname");
    assert_eq!(got.uobject, want.uobject, "{name} uobject");
    assert_eq!(got.ufield, want.ufield, "{name} ufield");

    let mut gs = got.ustruct.clone();
    let ws = want.ustruct.clone();
    if mentions(d, "ustruct.min_alignment_width") {
        gs.min_alignment_width = ws.min_alignment_width;
    }
    assert_eq!(gs, ws, "{name} ustruct");
    let optional = |field: &str, got: Option<u32>, want: Option<u32>| {
        if strict {
            assert_eq!(got, want, "{name} {field}");
        } else {
            assert!(
                got.is_none() || got == want,
                "{name} {field}: {got:?} is not {want:?}"
            );
        }
    };
    optional("cast_flags", got.uclass.cast_flags, want.uclass.cast_flags);
    optional(
        "default_object",
        got.uclass.default_object,
        want.uclass.default_object,
    );
    assert_eq!(
        got.ufunction.flags, want.ufunction.flags,
        "{name} function flags"
    );
    optional(
        "native_func",
        got.ufunction.native_func,
        want.ufunction.native_func,
    );
    if strict {
        assert_eq!(got.uenum, want.uenum, "{name} uenum");
    } else {
        use ufathom_model::layout::UEnumLayout as E;
        match (&got.uenum, &want.uenum) {
            (
                E::PairArray {
                    offset: a,
                    stride: b,
                    value_offset: c,
                    ..
                },
                E::PairArray {
                    offset: x,
                    stride: y,
                    value_offset: z,
                    ..
                },
            ) => assert_eq!((a, b, c), (x, y, z), "{name} uenum"),
            (g, w) => assert_eq!(g, w, "{name} uenum"),
        }
    }

    // Properties.
    let (g, w) = (&got.properties, &want.properties);
    assert_eq!(g.system, w.system, "{name} system");
    assert_eq!(
        (g.array_dim, g.element_size, g.flags),
        (w.array_dim, w.element_size, w.flags),
        "{name} core"
    );
    assert_eq!(
        (g.offset_internal, g.size),
        (w.offset_internal, w.size),
        "{name} core 2"
    );
    let (gt, wt) = (&g.typed, &w.typed);
    use PropertyType as T;
    let check = |field: &str, got: Option<u32>, want: Option<u32>, present: bool| {
        if present && strict {
            assert_eq!(got, want, "{name} typed {field}");
        } else if present {
            assert!(
                got.is_none() || got == want,
                "{name} typed {field}: {got:?} is not {want:?}"
            );
        } else {
            assert!(
                got.is_none() || got == want,
                "{name} typed {field}: {got:?} without an instance"
            );
        }
    };
    check(
        "object_class",
        gt.object_class,
        wt.object_class,
        has_type(fx, |t| {
            matches!(
                t,
                T::Object { .. }
                    | T::WeakObject { .. }
                    | T::LazyObject { .. }
                    | T::SoftObject { .. }
            )
        }),
    );
    check(
        "class_meta",
        gt.class_meta,
        wt.class_meta,
        has_type(fx, |t| matches!(t, T::Class { .. } | T::SoftClass { .. })),
    );
    check(
        "script_struct",
        gt.script_struct,
        wt.script_struct,
        has_type(fx, |t| matches!(t, T::Struct { .. })),
    );
    check(
        "array_inner",
        gt.array_inner,
        wt.array_inner,
        has_type(fx, |t| matches!(t, T::Array { .. })),
    );
    check(
        "set_element",
        gt.set_element,
        wt.set_element,
        has_type(fx, |t| matches!(t, T::Set { .. })),
    );
    check(
        "map_key",
        gt.map_key,
        wt.map_key,
        has_type(fx, |t| matches!(t, T::Map { .. })),
    );
    check(
        "map_value",
        gt.map_value,
        wt.map_value,
        has_type(fx, |t| matches!(t, T::Map { .. })),
    );
    check(
        "enum_underlying",
        gt.enum_underlying,
        wt.enum_underlying,
        has_type(fx, |t| matches!(t, T::Enum { .. })),
    );
    check(
        "enum_type",
        gt.enum_type,
        wt.enum_type,
        has_type(fx, |t| matches!(t, T::Enum { .. })),
    );
    check(
        "byte_enum",
        gt.byte_enum,
        wt.byte_enum,
        has_type(fx, |t| matches!(t, T::Byte { .. })),
    );
    check(
        "delegate_signature",
        gt.delegate_signature,
        wt.delegate_signature,
        has_type(fx, |t| {
            matches!(
                t,
                T::Delegate { .. }
                    | T::MulticastInlineDelegate { .. }
                    | T::MulticastSparseDelegate { .. }
            )
        }),
    );
    check(
        "interface_class",
        gt.interface_class,
        wt.interface_class,
        has_type(fx, |t| matches!(t, T::Interface { .. })),
    );
    check(
        "optional_value",
        gt.optional_value,
        wt.optional_value,
        has_type(fx, |t| matches!(t, T::Optional { .. })),
    );
    let has_bool = has_type(fx, |t| matches!(t, T::Bool { .. }));
    if strict {
        assert_eq!(gt.bool_property.is_some(), has_bool, "{name} bool present");
    }
    if has_bool && (strict || gt.bool_property.is_some()) {
        assert_eq!(gt.bool_property, wt.bool_property, "{name} bool");
    }

    // Globals.
    assert_eq!(d.globals.module, fx.globals.module, "{name}");
    assert_eq!(
        d.globals.gworld,
        vec![fx.globals.gworld_rva],
        "{name} gworld"
    );
    assert_eq!(
        d.globals.gengine,
        vec![fx.globals.gengine_rva],
        "{name} gengine"
    );
    assert_eq!(
        d.globals.process_event_index,
        Some(fx.globals.process_event_index),
        "{name} pe index"
    );
    assert_eq!(
        d.globals.process_event_rva,
        Some(fx.globals.process_event_rva),
        "{name} pe rva"
    );

    assert!(
        !mentions(d, "names.block_offset_bits") && !mentions(d, "elements_per_chunk"),
        "{name}: a value was ordered, not observed: {:?}",
        d.warnings
    );
    for field in [
        "objects.objects",
        "uobject.class",
        "ustruct.super_struct",
        "properties.size",
    ] {
        assert!(
            d.evidence_for(field).is_some(),
            "{name}: no evidence for {field}"
        );
    }
}

/// Rebuilds the image from its parts after `edit` changed them. The module
/// keeps its name, base and size; the PE headers and resources are not part of
/// `Fixture::parts`, so the result has none.
pub fn rebuild(fx: &Fixture, edit: impl FnOnce(&mut Vec<Part>)) -> Snapshot {
    let mut parts = fx.parts.clone();
    edit(&mut parts);
    let module = fx.memory.modules().unwrap().remove(0);
    let mut builder = Snapshot::builder();
    for part in &parts {
        builder = builder.region_with(part.addr, part.bytes.clone(), part.perms, part.region_kind);
    }
    builder
        .module(&module.name, module.base, module.size)
        .build()
        .unwrap()
}

/// Writes `bytes` at `addr` inside whichever part holds it.
pub fn poke(parts: &mut [Part], addr: u64, bytes: &[u8]) {
    let part = parts
        .iter_mut()
        .find(|p| addr >= p.addr && addr + bytes.len() as u64 <= p.addr + p.bytes.len() as u64)
        .unwrap();
    let at = (addr - part.addr) as usize;
    part.bytes[at..at + bytes.len()].copy_from_slice(bytes);
}

/// Reads eight bytes at `addr` from the parts.
pub fn peek_u64(parts: &[Part], addr: u64) -> u64 {
    let part = parts
        .iter()
        .find(|p| addr >= p.addr && addr + 8 <= p.addr + p.bytes.len() as u64)
        .unwrap();
    let at = (addr - part.addr) as usize;
    u64::from_le_bytes(part.bytes[at..at + 8].try_into().unwrap())
}
