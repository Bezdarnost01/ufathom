//! The object array, locations and object paths on hand-built and patched
//! memory.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "test code may panic on failure; clippy.toml only exempts #[test] functions, not helpers"
)]

mod common;

use common::{Img, patched, small};
use ufathom_engine::limits::MAX_PATH_DEPTH;
use ufathom_engine::{
    EngineError, LoadOptions, NameTable, ObjectArrayView, ObjectSlot, object_path, read_header,
    resolve_location,
};
use ufathom_model::layout::{Location, ObjectArrayKind, ObjectArrayLayout, TransformOp};
use ufathom_testkit::Profile;

const BASE: u64 = 0x20_0000;
const ARRAY: u64 = BASE + 0x100;
const TABLE: u64 = BASE + 0x200;
const CHUNK0: u64 = BASE + 0x1000;
const CHUNK1: u64 = BASE + 0x2000;
const ITEM: u64 = 0x18;
const PER_CHUNK: u32 = 4;

fn layout(kind: ObjectArrayKind, transform: Vec<TransformOp>) -> ObjectArrayLayout {
    ObjectArrayLayout {
        location: Location::Absolute(ARRAY),
        kind,
        objects: 0,
        max_elements: 8,
        num_elements: 0xC,
        max_chunks: Some(0x10),
        num_chunks: Some(0x14),
        item_size: ITEM as u32,
        item_object: 0,
        item_serial: Some(0x10),
        pointer_transform: transform,
    }
}

fn chunked() -> ObjectArrayKind {
    ObjectArrayKind::Chunked {
        elements_per_chunk: PER_CHUNK,
    }
}

fn transform() -> Vec<TransformOp> {
    vec![
        TransformOp::Xor(0x5A5A_0000_0000_0000),
        TransformOp::Add(0x1000),
    ]
}

/// The pointer as stored when the layout's transform recovers `real`.
fn obfuscate(real: u64) -> u64 {
    (real - 0x1000) ^ 0x5A5A_0000_0000_0000
}

/// Six objects in two chunks of four: slots 1 and 4 are null. Chunk pointers
/// are stored scrambled when `scrambled`.
fn chunked_image(scrambled: bool) -> Img {
    let mut img = Img::new(BASE, 0x4000);
    let store = |real: u64| if scrambled { obfuscate(real) } else { real };
    img.u32(ARRAY + 8, 8);
    img.u32(ARRAY + 0xC, 6);
    img.u32(ARRAY + 0x10, 4);
    img.u32(ARRAY + 0x14, 2);
    img.u64(ARRAY, TABLE);
    img.u64(TABLE, store(CHUNK0));
    img.u64(TABLE + 8, store(CHUNK1));
    for slot in 0..6u64 {
        if slot == 1 || slot == 4 {
            continue;
        }
        let chunk = if slot < 4 { CHUNK0 } else { CHUNK1 };
        let item = chunk + (slot % 4) * ITEM;
        img.u64(item, 0x7FF0_0000 + slot * 0x100);
        img.u32(item + 0x10, 100 + slot as u32);
    }
    img
}

fn slot(index: u32) -> ObjectSlot {
    ObjectSlot {
        index,
        object: 0x7FF0_0000 + u64::from(index) * 0x100,
        serial: Some(100 + index),
    }
}

#[test]
fn a_chunked_array_reads_by_slot_and_by_chunk() {
    let snapshot = chunked_image(false).snapshot("game.exe");
    let layout = layout(chunked(), Vec::new());
    let mut view = ObjectArrayView::new(&snapshot, &layout).unwrap();
    assert_eq!(view.num(), 6);
    assert_eq!(view.chunk_count(), 2);
    assert_eq!(view.refresh().unwrap(), 6);
    assert_eq!(view.slot_address(5).unwrap(), CHUNK1 + ITEM);
    assert_eq!(view.object_at(0).unwrap(), Some(slot(0).object));
    assert_eq!(view.object_at(1).unwrap(), None);
    assert_eq!(view.object_at(5).unwrap(), Some(slot(5).object));
    let first = view.objects_in_chunk(0).unwrap();
    assert_eq!(first.objects, [slot(0), slot(2), slot(3)]);
    assert_eq!(first.null_slots, 1);
    // The last chunk holds only the two slots that exist.
    let second = view.objects_in_chunk(1).unwrap();
    assert_eq!(second.objects, [slot(5)]);
    assert_eq!(second.null_slots, 1);
    // Out of range, never a read past the end.
    for error in [
        view.slot_address(6).unwrap_err(),
        view.object_at(u32::MAX).unwrap_err(),
        view.objects_in_chunk(2).unwrap_err(),
    ] {
        assert!(matches!(error, EngineError::Limit { .. }), "{error}");
    }
}

#[test]
fn a_pointer_transform_is_applied_when_configured_and_fails_cleanly_when_not() {
    let scrambled = chunked_image(true).snapshot("game.exe");
    let configured = layout(chunked(), transform());
    let view = ObjectArrayView::new(&scrambled, &configured).unwrap();
    assert_eq!(
        view.objects_in_chunk(0).unwrap().objects,
        [slot(0), slot(2), slot(3)]
    );
    assert_eq!(view.object_at(5).unwrap(), Some(slot(5).object));
    assert_eq!(view.slot_address(4).unwrap(), CHUNK1);

    // The same image without the transform: the scrambled chunk pointers
    // point nowhere, which is an error and not a panic.
    let unconfigured = layout(chunked(), Vec::new());
    let view = ObjectArrayView::new(&scrambled, &unconfigured).unwrap();
    for chunk in 0..view.chunk_count() {
        let error = view.objects_in_chunk(chunk).unwrap_err();
        assert!(matches!(error.root(), EngineError::Read { .. }), "{error}");
    }
    assert!(view.object_at(0).is_err() && view.object_at(3).is_err());

    // A plain image read with the transform configured: the other way round.
    let plain = chunked_image(false).snapshot("game.exe");
    let view = ObjectArrayView::new(&plain, &configured).unwrap();
    assert!(view.objects_in_chunk(0).is_err());
}

#[test]
fn a_fixed_array_applies_the_transform_to_its_storage_pointer() {
    let mut img = Img::new(BASE, 0x4000);
    img.u32(ARRAY + 8, 16);
    img.u32(ARRAY + 0xC, 3);
    img.u64(ARRAY, obfuscate(CHUNK0));
    for slot in 0..3u64 {
        img.u64(CHUNK0 + slot * ITEM, 0x1000 + slot);
    }
    let snapshot = img.snapshot("game.exe");
    let mut fixed = layout(ObjectArrayKind::Fixed, transform());
    fixed.max_chunks = None;
    fixed.num_chunks = None;
    let view = ObjectArrayView::new(&snapshot, &fixed).unwrap();
    assert_eq!((view.num(), view.chunk_count()), (3, 1));
    let objects: Vec<u64> = view
        .objects_in_chunk(0)
        .unwrap()
        .objects
        .iter()
        .map(|s| s.object)
        .collect();
    assert_eq!(objects, [0x1000, 0x1001, 0x1002]);
    assert_eq!(view.object_at(2).unwrap(), Some(0x1002));
    assert!(view.objects_in_chunk(1).is_err());
}

#[test]
fn counts_above_capacity_are_refused_before_any_slot_is_read() {
    let build = |edit: &dyn Fn(&mut Img), layout: &ObjectArrayLayout| {
        let mut img = chunked_image(false);
        edit(&mut img);
        ObjectArrayView::new(&img.snapshot("game.exe"), layout).map(|_| ())
    };
    let layout = layout(chunked(), Vec::new());
    let limit = |what: &str, edit: &dyn Fn(&mut Img)| {
        let error = build(edit, &layout).unwrap_err();
        assert!(
            matches!(error, EngineError::Limit { .. }),
            "{what}: {error}"
        );
    };
    limit("elements above capacity", &|i| i.u32(ARRAY + 0xC, 9));
    limit("chunks above chunk capacity", &|i| i.u32(ARRAY + 0x14, 5));
    // The capacity allows 9 but two chunks of four hold 8.
    limit("elements above the chunks' room", &|i| {
        i.u32(ARRAY + 8, 20);
        i.u32(ARRAY + 0xC, 9);
    });
    limit("elements above the named limit", &|i| {
        for offset in [8, 0xC, 0x10, 0x14] {
            i.u32(ARRAY + offset, u32::MAX);
        }
    });
    // Layouts that cannot describe an array.
    let mut bad = layout.clone();
    bad.item_size = 0;
    assert!(matches!(
        build(&|_| {}, &bad).unwrap_err(),
        EngineError::InvalidLayout { .. }
    ));
    let mut bad = layout.clone();
    bad.item_object = 0x18;
    assert!(matches!(
        build(&|_| {}, &bad).unwrap_err(),
        EngineError::InvalidLayout { .. }
    ));
    let mut bad = layout.clone();
    bad.kind = ObjectArrayKind::Chunked {
        elements_per_chunk: 0,
    };
    assert!(matches!(
        build(&|_| {}, &bad).unwrap_err(),
        EngineError::InvalidLayout { .. }
    ));
    let mut bad = layout.clone();
    bad.max_chunks = None;
    assert!(matches!(
        build(&|_| {}, &bad).unwrap_err(),
        EngineError::InvalidLayout { .. }
    ));
    // An unreadable array header.
    let snapshot = Img::new(0x1000_0000, 0x100).snapshot("game.exe");
    assert!(matches!(
        ObjectArrayView::new(&snapshot, &layout).unwrap_err(),
        EngineError::Read { .. }
    ));
}

#[test]
fn locations_resolve_against_the_module_list() {
    let snapshot = Img::new(BASE, 0x1000).snapshot("Game.exe");
    let rva = |module: &str, rva: u64| Location::Rva {
        module: module.to_owned(),
        rva,
    };
    assert_eq!(
        resolve_location(&snapshot, &Location::Absolute(7)).unwrap(),
        7
    );
    assert_eq!(
        resolve_location(&snapshot, &rva("game.EXE", 0x10)).unwrap(),
        BASE + 0x10
    );
    assert_eq!(
        resolve_location(&snapshot, &rva(r"D:\Steam\Game\GAME.exe", 0)).unwrap(),
        BASE
    );
    assert!(matches!(
        resolve_location(&snapshot, &rva("other.exe", 0)).unwrap_err(),
        EngineError::ModuleNotFound { .. }
    ));
    assert!(matches!(
        resolve_location(&snapshot, &rva("game.exe", u64::MAX)).unwrap_err(),
        EngineError::Inconsistent { .. }
    ));
}

#[test]
fn object_paths_stop_at_cycles_and_at_the_depth_limit() {
    let fx = small(Profile::ue4_25_case_preserving_measured());
    let layout = &fx.layout;
    let outer = u64::from(layout.uobject.outer);
    let names = NameTable::load(
        &fx.memory,
        &layout.names,
        &LoadOptions::for_layout(layout, None),
    )
    .unwrap();
    let fillers: Vec<u64> = fx
        .objects
        .iter()
        .filter(|r| r.path.starts_with("/Game/Maps/Main.Main:PersistentLevel."))
        .map(|r| r.address)
        .collect();
    assert!(fillers.len() > MAX_PATH_DEPTH + 10);
    let path = |memory: &ufathom_mem::Snapshot, addr: u64| {
        object_path(memory, &names, &layout.uobject, &layout.fname, addr)
    };

    // An object that is its own outer.
    let own = fillers[0];
    let memory = patched(&fx, &[(own + outer, own.to_le_bytes().to_vec())]);
    let error = path(&memory, own).unwrap_err();
    assert!(
        matches!(error, EngineError::Cycle { addr, .. } if addr == own),
        "{error}"
    );

    // A chain of exactly the limit resolves; one more does not.
    let link = |count: usize| {
        let patches: Vec<(u64, Vec<u8>)> = fillers[..count]
            .windows(2)
            .map(|pair| (pair[0] + outer, pair[1].to_le_bytes().to_vec()))
            .chain(std::iter::once((
                fillers[count - 1] + outer,
                0u64.to_le_bytes().to_vec(),
            )))
            .collect();
        patched(&fx, &patches)
    };
    let memory = link(MAX_PATH_DEPTH);
    assert!(path(&memory, fillers[0]).is_ok());
    let memory = link(MAX_PATH_DEPTH + 1);
    let error = path(&memory, fillers[0]).unwrap_err();
    assert!(matches!(error, EngineError::Limit { .. }), "{error}");
}

#[test]
fn a_header_read_reports_its_layout_and_memory_problems() {
    let fx = small(Profile::ue4_25_case_preserving_measured());
    let layout = &fx.layout;
    let record = fx.object("/Script/Engine.Actor").unwrap();
    let header = read_header(&fx.memory, &layout.uobject, &layout.fname, record.address).unwrap();
    assert_eq!((header.addr, header.index), (record.address, record.index));
    assert_eq!(header.name.as_slice().len() as u32, layout.fname.size);
    header.expect_index(record.index).unwrap();
    assert!(matches!(
        header.expect_index(record.index + 1).unwrap_err(),
        EngineError::Inconsistent { .. }
    ));
    let mut wide = layout.fname.clone();
    wide.size = 17;
    assert!(matches!(
        read_header(&fx.memory, &layout.uobject, &wide, record.address).unwrap_err(),
        EngineError::InvalidLayout { .. }
    ));
    assert!(matches!(
        read_header(
            &fx.memory,
            &layout.uobject,
            &layout.fname,
            fx.anchors.unmapped
        )
        .unwrap_err(),
        EngineError::Read { .. }
    ));
    // An address at the very top of the space: an error, not an overflow.
    assert!(
        read_header(&fx.memory, &layout.uobject, &layout.fname, u64::MAX - 4).is_err(),
        "overflowing header"
    );
}
