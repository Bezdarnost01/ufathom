//! Modular builds: the object array, the name table and the world live in
//! different modules, and the search must find each where it is, in any order
//! of modules, and try the next candidate when one is rejected.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::print_stderr,
    reason = "test code may panic on failure"
)]

mod solve_common;

use solve_common::{absolute, build, peek_u64, poke};
use ufathom_mem::{MemoryReader, Perms, RegionKind, Snapshot, SnapshotBuilder};
use ufathom_model::layout::{Location, NameTableLayout};
use ufathom_scan::{DiscoverOptions, ScanError, discover};
use ufathom_testkit::{Fixture, Part, PartKind, Profile};

/// File names of the modules the fixture's globals are moved to.
const CORE_UOBJECT: &str = "UE4Game-CoreUObject.dll";
const CORE: &str = "UE4Game-Core.dll";
const ENGINE: &str = "UE4Game-Engine.dll";

/// A module to add: its name, base, and the writable data it holds.
struct Moved {
    name: &'static str,
    base: u64,
    data: Vec<u8>,
}

/// Offset of the data inside a moved module.
const DATA_AT: u64 = 0x1000;

fn module_size(data: &[u8]) -> u64 {
    DATA_AT + (data.len() as u64).next_multiple_of(0x1000) + 0x1000
}

fn add_moved(mut builder: SnapshotBuilder, moved: &Moved) -> SnapshotBuilder {
    let mut bytes = moved.data.clone();
    bytes.resize(bytes.len().next_multiple_of(0x1000), 0);
    builder = builder.region_with(moved.base + DATA_AT, bytes, Perms::RW, RegionKind::Image);
    builder.module(moved.name, moved.base, module_size(&moved.data))
}

fn snapshot(fx: &Fixture, parts: &[Part], moved: &[Moved]) -> Snapshot {
    let main = fx.memory.modules().unwrap().remove(0);
    let mut builder = Snapshot::builder();
    for part in parts {
        builder = builder.region_with(part.addr, part.bytes.clone(), part.perms, part.region_kind);
    }
    builder = builder.module(&main.name, main.base, main.size);
    for m in moved {
        builder = add_moved(builder, m);
    }
    builder.build().unwrap()
}

/// Bytes of the fixture's memory at `addr`.
fn bytes_at(parts: &[Part], addr: u64, len: usize) -> Vec<u8> {
    let part = parts
        .iter()
        .find(|p| addr >= p.addr && addr + len as u64 <= p.addr + p.bytes.len() as u64)
        .unwrap();
    let at = (addr - part.addr) as usize;
    part.bytes[at..at + len].to_vec()
}

/// The fixture with its object array, name pool and world slots moved out
/// of the main module into three modules of their own.
struct Modular {
    snapshot: Snapshot,
    /// Where each moved structure starts.
    array: u64,
    pool: u64,
    gworld: u64,
    gengine: u64,
}

fn split(fx: &Fixture) -> Modular {
    let mut parts = fx.parts.clone();
    let data = &fx.anchors.data;
    let NameTableLayout::Pool(pool_layout) = &fx.layout.names else {
        panic!("not a pool");
    };
    let base = fx.addresses.module_base;

    let array_bytes = bytes_at(&parts, data.object_array, 0x40);
    poke(&mut parts, data.object_array, &[0u8; 0x40]);
    let pool_size = pool_layout.blocks as usize + 8192 * 8;
    let pool_bytes = bytes_at(&parts, data.name_anchor, pool_size);
    poke(&mut parts, data.name_anchor, &vec![0u8; pool_size]);
    let gworld = peek_u64(&parts, data.gworld);
    let gengine = peek_u64(&parts, data.gengine);
    poke(&mut parts, data.gworld, &[0u8; 8]);
    poke(&mut parts, data.gengine, &[0u8; 8]);

    let mut engine_data = vec![0u8; 0x100];
    engine_data[0x20..0x28].copy_from_slice(&gworld.to_le_bytes());
    engine_data[0x60..0x68].copy_from_slice(&gengine.to_le_bytes());
    let moved = [
        Moved {
            name: CORE_UOBJECT,
            base: base + 0x2000_0000,
            data: array_bytes,
        },
        Moved {
            name: CORE,
            base: base + 0x1000_0000,
            data: pool_bytes,
        },
        Moved {
            name: ENGINE,
            base: base + 0x3000_0000,
            data: engine_data,
        },
    ];
    let snapshot = snapshot(fx, &parts, &moved);
    Modular {
        snapshot,
        array: moved[0].base + DATA_AT,
        pool: moved[1].base + DATA_AT,
        gworld: moved[2].base + DATA_AT + 0x20,
        gengine: moved[2].base + DATA_AT + 0x60,
    }
}

fn module_of(location: &Location) -> &str {
    match location {
        Location::Rva { module, .. } => module,
        Location::Absolute(_) => "<absolute>",
    }
}

#[test]
fn the_object_array_the_names_and_the_world_are_found_in_three_modules() {
    let fx = build(Profile::ue4_25_case_preserving_measured(), 3, 300);
    let split = split(&fx);
    let d = discover(&split.snapshot, &DiscoverOptions::default()).unwrap();

    // Each result records its own module.
    assert_eq!(module_of(&d.layout.objects.location), CORE_UOBJECT);
    let NameTableLayout::Pool(pool) = &d.layout.names else {
        panic!("not a pool");
    };
    assert_eq!(module_of(&pool.location), CORE);
    let NameTableLayout::Pool(truth) = &fx.layout.names else {
        panic!("not a pool");
    };

    // The fields are where they were moved to.
    let want = &fx.layout.objects;
    let got = &d.layout.objects;
    let moved_by = split.array - fx.anchors.data.object_array;
    let at = |layout: &ufathom_model::layout::ObjectArrayLayout, off: u32| {
        resolve(&split.snapshot, &layout.location) + u64::from(off)
    };
    assert_eq!(
        at(got, got.objects),
        fx.anchors.data.object_array + u64::from(want.objects) + moved_by
    );
    assert_eq!(
        at(got, got.num_elements),
        fx.anchors.data.object_array + u64::from(want.num_elements) + moved_by
    );
    let pool_at = |off: u32| resolve(&split.snapshot, &pool.location) + u64::from(off);
    assert_eq!(pool_at(pool.blocks), split.pool + u64::from(truth.blocks));
    assert_eq!(
        (pool.stride, pool.block_offset_bits, &pool.entry),
        (truth.stride, truth.block_offset_bits, &truth.entry)
    );
    assert_eq!(d.layout.uobject, fx.layout.uobject);
    assert_eq!(d.layout.ustruct, {
        let mut t = fx.layout.ustruct.clone();
        t.min_alignment_width = d.layout.ustruct.min_alignment_width;
        t
    });

    // The world and the engine are in a third module; the code is in the
    // main module.
    assert_eq!(d.globals.module, CORE_UOBJECT);
    assert!(d.globals.gworld.is_empty() && d.globals.gengine.is_empty());
    let slot_addr = |l: &Location| resolve(&split.snapshot, l);
    assert_eq!(
        d.globals
            .gworld_slots
            .iter()
            .map(slot_addr)
            .collect::<Vec<_>>(),
        vec![split.gworld]
    );
    assert_eq!(
        d.globals
            .gengine_slots
            .iter()
            .map(slot_addr)
            .collect::<Vec<_>>(),
        vec![split.gengine]
    );
    assert_eq!(module_of(&d.globals.gworld_slots[0]), ENGINE);
    assert_eq!(
        d.globals.process_event_module.as_deref(),
        Some(fx.module_name())
    );
    assert_eq!(
        d.globals.process_event_rva,
        Some(fx.globals.process_event_rva)
    );
    assert_eq!(
        d.globals.process_event_index,
        Some(fx.globals.process_event_index)
    );
}

fn resolve(mem: &dyn MemoryReader, location: &Location) -> u64 {
    ufathom_engine::resolve_location(mem, location).unwrap()
}

#[test]
fn any_selected_module_finds_the_same_layout() {
    let fx = build(Profile::ue4_25_case_preserving_measured(), 3, 300);
    let split = split(&fx);
    let reference = discover(&split.snapshot, &DiscoverOptions::default()).unwrap();
    for module in [CORE, CORE_UOBJECT, ENGINE, fx.module_name()] {
        let options = DiscoverOptions {
            module: Some(module.to_owned()),
            ..DiscoverOptions::default()
        };
        let d = discover(&split.snapshot, &options).unwrap_or_else(|e| panic!("{module}: {e}"));
        assert_eq!(d.layout, reference.layout, "{module}");
    }
    let _ = absolute;
}

/// A module that holds a plausible object array and nothing else: 1000
/// objects with an image vtable and a class-like link, so that it passes
/// every check of the array itself, but with names that decode to nothing.
fn fake_array_module(
    name: &'static str,
    base: u64,
    vtable: u64,
    heap: u64,
) -> (Moved, Vec<(u64, Vec<u8>)>) {
    const COUNT: u64 = 1000;
    let object = |i: u64| heap + 0x4_0000 + i * 0x40;
    let mut data = vec![0u8; 0x400];
    let put32 =
        |b: &mut Vec<u8>, at: usize, v: u32| b[at..at + 4].copy_from_slice(&v.to_le_bytes());
    let put64 =
        |b: &mut Vec<u8>, at: usize, v: u64| b[at..at + 8].copy_from_slice(&v.to_le_bytes());
    put64(&mut data, 0x100, heap);
    put32(&mut data, 0x110, 0x2_0000);
    put32(&mut data, 0x114, COUNT as u32);
    put32(&mut data, 0x118, 2);
    put32(&mut data, 0x11C, 1);
    let mut table = vec![0u8; 0x1000];
    put64(&mut table, 0, heap + 0x1000);
    let mut items = vec![0u8; 0x8000];
    let mut objects = vec![0u8; 0x1_0000];
    for i in 0..COUNT {
        put64(&mut items, (i * 0x18) as usize, object(i));
        let at = (i * 0x40) as usize;
        put64(&mut objects, at, vtable);
        put32(&mut objects, at + 0xC, i as u32);
        put64(&mut objects, at + 0x10, object(0));
    }
    (
        Moved { name, base, data },
        vec![
            (heap, table),
            (heap + 0x1000, items),
            (heap + 0x4_0000, objects),
        ],
    )
}

#[test]
fn a_rejected_candidate_is_set_aside_and_the_next_one_is_tried() {
    let fx = build(Profile::ue4_25_case_preserving_measured(), 3, 300);
    let split = split(&fx);
    // A decoy array in a module that is searched before CoreUObject: its name
    // says Core, and it sits at a lower address.
    let base = fx.addresses.module_base;
    let heap = fx.addresses.heap + fx.heap_span + 0x100_0000;
    let (decoy, heap_regions) =
        fake_array_module("AAA-Core.dll", base + 0x800_0000, fx.addresses.text, heap);

    let mut parts = fx.parts.clone();
    let data = &fx.anchors.data;
    let NameTableLayout::Pool(pool_layout) = &fx.layout.names else {
        panic!("not a pool");
    };
    // Rebuild the split image with the decoy added.
    let array_bytes = bytes_at(&parts, data.object_array, 0x40);
    poke(&mut parts, data.object_array, &[0u8; 0x40]);
    let pool_size = pool_layout.blocks as usize + 8192 * 8;
    let pool_bytes = bytes_at(&parts, data.name_anchor, pool_size);
    poke(&mut parts, data.name_anchor, &vec![0u8; pool_size]);
    for (addr, bytes) in heap_regions {
        parts.push(Part {
            kind: PartKind::Heap,
            addr,
            bytes,
            perms: Perms::RW,
            region_kind: RegionKind::Private,
        });
    }
    let moved = [
        decoy,
        Moved {
            name: CORE_UOBJECT,
            base: base + 0x2000_0000,
            data: array_bytes,
        },
        Moved {
            name: CORE,
            base: base + 0x1000_0000,
            data: pool_bytes,
        },
    ];
    let snapshot = snapshot(&fx, &parts, &moved);
    let d = discover(&snapshot, &DiscoverOptions::default()).unwrap();
    assert_eq!(module_of(&d.layout.objects.location), CORE_UOBJECT);
    assert_eq!(d.layout.uobject, fx.layout.uobject);
    // What the rejected candidate recorded is not evidence.
    assert!(
        d.evidence
            .iter()
            .filter(|e| e.field == "objects.num_elements")
            .count()
            == 1
    );
    let _ = split;
}

#[test]
fn when_every_candidate_is_rejected_the_error_lists_them_all() {
    let fx = build(Profile::ue4_25_case_preserving_measured(), 3, 300);
    let base = fx.addresses.module_base;
    let mut parts: Vec<Part> = Vec::new();
    let (a, heap_a) = fake_array_module(
        "One-Core.dll",
        base + 0x100_0000,
        fx.addresses.text,
        0x2_0000_0000,
    );
    let (b, heap_b) = fake_array_module(
        "Two-CoreUObject.dll",
        base + 0x200_0000,
        fx.addresses.text,
        0x2_8000_0000,
    );
    for (addr, bytes) in heap_a.into_iter().chain(heap_b) {
        parts.push(Part {
            kind: PartKind::Heap,
            addr,
            bytes,
            perms: Perms::RW,
            region_kind: RegionKind::Private,
        });
    }
    // Keep the main module and its code, so the vtables are in an image.
    let text: Vec<Part> = fx
        .parts
        .iter()
        .filter(|p| p.kind == PartKind::Text)
        .cloned()
        .collect();
    let data: Vec<Part> = fx
        .parts
        .iter()
        .filter(|p| p.kind == PartKind::Data)
        .cloned()
        .collect();
    let mut all = text;
    all.extend(data.into_iter().map(|mut p| {
        p.bytes.fill(0);
        p
    }));
    all.append(&mut parts);
    let snapshot = snapshot(&fx, &all, &[a, b]);
    match discover(&snapshot, &DiscoverOptions::default()) {
        Err(ScanError::CandidatesRejected { rejected }) => {
            let modules: Vec<&str> = rejected.iter().map(|r| r.module.as_str()).collect();
            assert_eq!(modules, ["One-Core.dll", "Two-CoreUObject.dll"]);
            assert!(rejected.iter().all(|r| r.objects == 1000));
            let text = ScanError::CandidatesRejected { rejected }.to_string();
            assert!(text.contains("One-Core.dll") && text.contains("Two-CoreUObject.dll"));
        }
        other => panic!(
            "expected every candidate rejected, got {:?}",
            other.map(|_| ())
        ),
    }
}
