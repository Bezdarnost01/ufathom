//! Name table behaviour on hand-built memory: encodings, outline numbers,
//! damaged blocks, and resolution rules.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "test code may panic on failure; clippy.toml only exempts #[test] functions, not helpers"
)]

mod common;

use common::Img;
use ufathom_engine::limits::MAX_NAME_LENGTH;
use ufathom_engine::{EngineError, LoadOptions, NameTable};
use ufathom_mem::Snapshot;
use ufathom_model::layout::{
    FNameLayout, Location, NameArrayLayout, NameEntryArrayLayout, NamePoolLayout, NameTableLayout,
    PoolEntryLayout,
};

const BASE: u64 = 0x10_0000;
const SIZE: usize = 0x2_0000;
const POOL: u64 = BASE + 0x100;
const BLOCK: u64 = BASE + 0x1000;

fn pool_layout(bits: u32) -> NamePoolLayout {
    NamePoolLayout {
        // A different spelling of the module name than the snapshot's.
        location: Location::Rva {
            module: r"C:\Games\GAME.EXE".to_owned(),
            rva: POOL - BASE,
        },
        blocks: 0x10,
        current_block: 8,
        cursor: 0xC,
        stride: 4,
        block_offset_bits: bits,
        entry: PoolEntryLayout {
            header_offset: 0,
            string_offset: 2,
            len_shift: 1,
            wide_mask: 1,
        },
    }
}

/// Writes a narrow entry at `at` and returns the offset after it, aligned to
/// the stride of 4.
fn narrow(img: &mut Img, at: u64, text: &[u8]) -> u64 {
    img.u16(at, (text.len() as u16) << 1);
    img.put(at + 2, text);
    (at + 2 + text.len() as u64).next_multiple_of(4)
}

fn wide(img: &mut Img, at: u64, units: &[u16]) -> u64 {
    img.u16(at, ((units.len() as u16) << 1) | 1);
    for (i, unit) in units.iter().enumerate() {
        img.u16(at + 2 + i as u64 * 2, *unit);
    }
    (at + 2 + units.len() as u64 * 2).next_multiple_of(4)
}

fn finish_pool(img: &mut Img, cursor: u64) -> Snapshot {
    img.u64(POOL + 0x10, BLOCK);
    img.u32(POOL + 8, 0);
    img.u32(POOL + 0xC, cursor as u32);
    img.snapshot("game.exe")
}

fn load(snapshot: &Snapshot, layout: NamePoolLayout, outline: bool) -> NameTable {
    NameTable::load(
        snapshot,
        &NameTableLayout::Pool(layout),
        &LoadOptions {
            pool: None,
            outline_numbers: outline,
        },
    )
    .unwrap()
}

#[test]
fn outline_number_entries_follow_the_indirection() {
    let mut img = Img::new(BASE, SIZE);
    let mut at = BLOCK;
    at = narrow(&mut img, at, b"None"); // index 0
    at = narrow(&mut img, at, b"Foo"); // index 2
    // A zero-length entry: entry id 2 and number 5, stored after the header.
    img.u16(at, 0);
    img.u32(at + 2, 2);
    img.u32(at + 6, 5);
    at = (at + 10).next_multiple_of(4); // index 4
    at = wide(&mut img, at, &[0xDC, 0x6E, 0xEF]); // "Ünï", index 7
    let snapshot = finish_pool(&mut img, at - BLOCK);

    let names = load(&snapshot, pool_layout(6), true);
    assert_eq!(names.resolve_index(0), Some("None"));
    assert_eq!(names.resolve_index(2), Some("Foo"));
    assert_eq!(
        names.resolve_index(4),
        Some("Foo_4"),
        "number 5 is suffix _4"
    );
    assert_eq!(names.resolve_index(7), Some("\u{dc}n\u{ef}"));
    assert_eq!(names.len(), 4);
    assert!(names.stats().damaged_blocks.is_empty());
    assert_eq!(names.stats().unresolved_numbered, 0);

    // An FName of 4 bytes with no number field, as such builds have.
    let fname = FNameLayout {
        size: 4,
        comparison_index: 0,
        display_index: None,
        number: None,
    };
    assert_eq!(names.resolve(&fname, &4u32.to_le_bytes()).unwrap(), "Foo_4");

    // Without the build option a zero length ends the block, so the
    // indirection and everything after it are not read, and nothing breaks.
    let plain = load(&snapshot, pool_layout(6), false);
    assert_eq!(plain.len(), 2);
    assert!(plain.resolve_index(4).is_none() && !plain.is_entry_start(4));
    assert!(plain.stats().damaged_blocks.is_empty());
}

#[test]
fn an_indirection_to_nothing_or_to_another_indirection_is_unresolved() {
    let mut img = Img::new(BASE, SIZE);
    let mut at = BLOCK;
    at = narrow(&mut img, at, b"None"); // 0
    let missing = at;
    img.u16(at, 0);
    img.u32(at + 2, 99);
    img.u32(at + 6, 1);
    at = (at + 10).next_multiple_of(4);
    let chained = at;
    img.u16(at, 0);
    img.u32(at + 2, ((missing - BLOCK) / 4) as u32);
    img.u32(at + 6, 2);
    at = (at + 10).next_multiple_of(4);
    let snapshot = finish_pool(&mut img, at - BLOCK);
    let names = load(&snapshot, pool_layout(6), true);
    assert_eq!(names.len(), 1);
    assert_eq!(names.stats().unresolved_numbered, 2);
    for entry in [missing, chained] {
        let index = ((entry - BLOCK) / 4) as u32;
        assert!(names.is_entry_start(index), "the entry itself was seen");
        assert!(names.resolve_index(index).is_none());
    }
}

#[test]
fn a_malformed_entry_ends_its_block_and_is_reported() {
    // An entry that claims more characters than any name may have.
    let mut img = Img::new(BASE, SIZE);
    let mut at = narrow(&mut img, BLOCK, b"None");
    img.u16(at, (MAX_NAME_LENGTH as u16 + 1) << 1);
    at += 8;
    img.put(at, b"Lost");
    let snapshot = finish_pool(&mut img, 64);
    let names = load(&snapshot, pool_layout(6), false);
    assert_eq!(names.len(), 1);
    let damaged = &names.stats().damaged_blocks;
    assert_eq!(damaged.len(), 1);
    assert!(damaged[0].reason.contains("characters"), "{damaged:?}");

    // An entry that runs past the used part of the last block.
    let mut img = Img::new(BASE, SIZE);
    let at = narrow(&mut img, BLOCK, b"None");
    img.u16(at, 40 << 1);
    let snapshot = finish_pool(&mut img, 20);
    let names = load(&snapshot, pool_layout(6), false);
    assert_eq!(names.len(), 1);
    assert_eq!(names.stats().damaged_blocks.len(), 1);
    assert!(
        names.stats().damaged_blocks[0]
            .reason
            .contains("past the end")
    );
}

#[test]
fn pool_headers_with_impossible_values_are_errors() {
    let mut img = Img::new(BASE, SIZE);
    narrow(&mut img, BLOCK, b"None");
    let snapshot = finish_pool(&mut img, 8);
    let load_with = |edit: &dyn Fn(&mut NamePoolLayout)| {
        let mut layout = pool_layout(6);
        edit(&mut layout);
        NameTable::load(
            &snapshot,
            &NameTableLayout::Pool(layout),
            &LoadOptions::default(),
        )
    };
    for (what, result) in [
        ("zero stride", load_with(&|l| l.stride = 0)),
        ("zero bits", load_with(&|l| l.block_offset_bits = 0)),
        ("huge bits", load_with(&|l| l.block_offset_bits = 32)),
        ("block too big", load_with(&|l| l.block_offset_bits = 31)),
        ("shift", load_with(&|l| l.entry.len_shift = 16)),
        (
            "string in header",
            load_with(&|l| l.entry.string_offset = 1),
        ),
        (
            "module",
            load_with(&|l| {
                l.location = Location::Rva {
                    module: "other.exe".to_owned(),
                    rva: 0,
                }
            }),
        ),
    ] {
        let error = result.unwrap_err();
        assert!(
            matches!(
                error,
                EngineError::InvalidLayout { .. } | EngineError::ModuleNotFound { .. }
            ),
            "{what}: {error}"
        );
    }
    // A current block above the limit, and one whose index cannot fit 32 bits.
    let mut img = Img::new(BASE, SIZE);
    img.u64(POOL + 0x10, BLOCK);
    img.u32(POOL + 8, 100_000);
    let snapshot = img.snapshot("game.exe");
    let error = NameTable::load(
        &snapshot,
        &NameTableLayout::Pool(pool_layout(6)),
        &LoadOptions::default(),
    )
    .unwrap_err();
    assert!(matches!(error, EngineError::Limit { .. }), "{error}");
    let mut img = Img::new(BASE, SIZE);
    img.u32(POOL + 8, 8000);
    let snapshot = img.snapshot("game.exe");
    let error = NameTable::load(
        &snapshot,
        &NameTableLayout::Pool(pool_layout(20)),
        &LoadOptions::default(),
    )
    .unwrap_err();
    assert!(
        matches!(error, EngineError::InvalidLayout { .. }),
        "{error}"
    );
}

const GNAMES: u64 = BASE + 0x100;
const TABLE: u64 = BASE + 0x200;
const CHUNK0: u64 = BASE + 0x300;
const CHUNK1: u64 = BASE + 0x340;
const ENTRIES: u64 = BASE + 0x1000;
const SPACING: u64 = 0x800;

fn array_layout() -> NameArrayLayout {
    NameArrayLayout {
        location: Location::Absolute(GNAMES),
        indirect: true,
        num_elements: 0x10,
        num_chunks: 0x14,
        elements_per_chunk: 4,
        entry: NameEntryArrayLayout {
            index_offset: 0,
            string_offset: 0x10,
            wide_mask: 1,
        },
    }
}

fn entry_at(n: u64) -> u64 {
    ENTRIES + n * SPACING
}

fn put_entry(img: &mut Img, at: u64, index: u32, chars: &[u8], is_wide: bool) {
    img.u32(at, (index << 1) | u32::from(is_wide));
    img.put(at + 0x10, chars);
}

/// Eight names: ordinary, Latin-1, a lone surrogate, a long one, a null
/// pointer, an unmapped pointer, an unterminated one, and one at the very
/// end of the mapping.
fn array_image() -> Img {
    let mut img = Img::new(BASE, SIZE);
    img.u64(GNAMES, TABLE);
    img.u32(TABLE + 0x10, 8);
    img.u32(TABLE + 0x14, 2);
    img.u64(TABLE, CHUNK0);
    img.u64(TABLE + 8, CHUNK1);
    put_entry(&mut img, entry_at(0), 0, b"None", false);
    put_entry(&mut img, entry_at(1), 1, b"Caf\xe9", false);
    let units: [u16; 3] = [0x47, 0xD800, 0x72];
    let bytes: Vec<u8> = units.iter().flat_map(|u| u.to_le_bytes()).collect();
    put_entry(&mut img, entry_at(2), 2, &bytes, true);
    put_entry(&mut img, entry_at(3), 3, &[b'x'; 300], false);
    img.u64(CHUNK0, entry_at(0));
    img.u64(CHUNK0 + 8, entry_at(1));
    img.u64(CHUNK0 + 16, entry_at(2));
    img.u64(CHUNK0 + 24, entry_at(3));
    // Slot 4 stays null. Slot 5 points into unmapped memory.
    img.u64(CHUNK1 + 8, 0xDEAD_0000);
    put_entry(
        &mut img,
        entry_at(6),
        6,
        &vec![b'y'; MAX_NAME_LENGTH + 50],
        false,
    );
    img.u64(CHUNK1 + 16, entry_at(6));
    // The entry is closer to the end of the mapping than one prefetch.
    let tail = BASE + SIZE as u64 - 0x20;
    put_entry(&mut img, tail, 7, b"tail", false);
    img.u64(CHUNK1 + 24, tail);
    img
}

fn load_array(snapshot: &Snapshot, layout: NameArrayLayout) -> Result<NameTable, EngineError> {
    NameTable::load(
        snapshot,
        &NameTableLayout::Array(layout),
        &LoadOptions::default(),
    )
}

#[test]
fn array_entries_decode_and_bad_entries_are_counted() {
    let snapshot = array_image().snapshot("game.exe");
    let names = load_array(&snapshot, array_layout()).unwrap();
    assert_eq!(names.resolve_index(0), Some("None"));
    // Not UTF-8: Latin-1.
    assert_eq!(names.resolve_index(1), Some("Caf\u{e9}"));
    // A lone surrogate in a wide name: replaced, not an error.
    assert_eq!(names.resolve_index(2), Some("G\u{fffd}r"));
    // Longer than the prefetch: read by the terminator scan.
    assert_eq!(names.resolve_index(3), Some("x".repeat(300).as_str()));
    // A null pointer is a hole; an unmapped one and an unterminated name are
    // unreadable entries.
    assert!(names.resolve_index(4).is_none() && !names.is_entry_start(4));
    assert!(names.resolve_index(5).is_none());
    assert!(names.resolve_index(6).is_none());
    assert_eq!(names.stats().unreadable_entries, 2);
    // The prefetch fails near the end of the mapping; the entry still loads.
    assert_eq!(names.resolve_index(7), Some("tail"));
    assert_eq!(names.len(), 5);
    assert_eq!(names.stats().entries, 5);
    assert_eq!(names.stats().blocks, 2);
    assert!(names.resolve_index(8).is_none());
    assert!(names.resolve_index(u32::MAX).is_none());
}

#[test]
fn array_counts_and_pointers_are_checked() {
    let case = |edit: &dyn Fn(&mut Img)| {
        let mut img = array_image();
        edit(&mut img);
        load_array(&img.snapshot("game.exe"), array_layout())
    };
    // More names than the chunks can hold.
    let error = case(&|img| img.u32(TABLE + 0x10, 9)).unwrap_err();
    assert!(matches!(error, EngineError::Limit { .. }), "{error}");
    // A chunk count above the limit.
    let error = case(&|img| img.u32(TABLE + 0x14, 9000)).unwrap_err();
    assert!(matches!(error, EngineError::Limit { .. }), "{error}");
    // A null pointer to the array.
    let error = case(&|img| img.u64(GNAMES, 0)).unwrap_err();
    assert!(matches!(error, EngineError::Null { .. }), "{error}");
    // The first chunk missing is an error, a later one is a recorded loss.
    let error = case(&|img| img.u64(TABLE, 0)).unwrap_err();
    assert!(matches!(error, EngineError::Null { .. }), "{error}");
    let names = case(&|img| img.u64(TABLE + 8, 0xDEAD_0000)).unwrap();
    assert_eq!(names.stats().unreadable_blocks.len(), 1);
    assert_eq!(names.stats().unreadable_blocks[0].block, 1);
    assert_eq!(names.len(), 4);
    // A layout that cannot work.
    let mut layout = array_layout();
    layout.elements_per_chunk = 0;
    let snapshot = array_image().snapshot("game.exe");
    assert!(matches!(
        load_array(&snapshot, layout).unwrap_err(),
        EngineError::InvalidLayout { .. }
    ));
    let mut layout = array_layout();
    layout.entry.string_offset = 0x400;
    assert!(matches!(
        load_array(&snapshot, layout).unwrap_err(),
        EngineError::InvalidLayout { .. }
    ));
    // Not indirect: the location is the array itself.
    let mut layout = array_layout();
    layout.indirect = false;
    layout.location = Location::Absolute(TABLE);
    assert_eq!(load_array(&snapshot, layout).unwrap().len(), 5);
}

#[test]
fn names_resolve_from_the_display_index_with_a_comparison_fallback() {
    let snapshot = array_image().snapshot("game.exe");
    let names = load_array(&snapshot, array_layout()).unwrap();
    let cp = FNameLayout {
        size: 12,
        comparison_index: 0,
        display_index: Some(4),
        number: Some(8),
    };
    let bytes = |comparison: u32, display: u32, number: u32| {
        let mut out = Vec::new();
        for value in [comparison, display, number] {
            out.extend_from_slice(&value.to_le_bytes());
        }
        out
    };
    // Display wins, and the number is appended as Number - 1.
    assert_eq!(names.resolve(&cp, &bytes(0, 1, 0)).unwrap(), "Caf\u{e9}");
    assert_eq!(names.resolve(&cp, &bytes(0, 1, 1)).unwrap(), "Caf\u{e9}_0");
    assert_eq!(names.resolve(&cp, &bytes(0, 1, 3)).unwrap(), "Caf\u{e9}_2");
    // A display index that does not resolve falls back to the comparison one.
    assert_eq!(names.resolve(&cp, &bytes(1, 99, 0)).unwrap(), "Caf\u{e9}");
    // Neither resolves: an error naming the display index.
    let error = names.resolve(&cp, &bytes(98, 99, 0)).unwrap_err();
    assert!(
        matches!(error, EngineError::UnresolvedName { index: 99 }),
        "{error}"
    );
    // Bytes too short for the layout.
    let error = names.resolve(&cp, &bytes(0, 1, 0)[..8]).unwrap_err();
    assert!(matches!(error, EngineError::Malformed { .. }), "{error}");
    // A single index, with the comparison index not at the start.
    let single = FNameLayout {
        size: 8,
        comparison_index: 4,
        display_index: None,
        number: Some(0),
    };
    let mut raw = 2u32.to_le_bytes().to_vec();
    raw.extend_from_slice(&1u32.to_le_bytes());
    assert_eq!(names.resolve(&single, &raw).unwrap(), "Caf\u{e9}_1");
    // The largest number does not overflow.
    let mut raw = u32::MAX.to_le_bytes().to_vec();
    raw.extend_from_slice(&1u32.to_le_bytes());
    let single = FNameLayout {
        size: 8,
        comparison_index: 4,
        display_index: None,
        number: Some(0),
    };
    assert_eq!(
        names.resolve(&single, &raw).unwrap(),
        format!("Caf\u{e9}_{}", u32::MAX - 1)
    );
}

#[test]
fn pool_blocks_are_read_to_their_end_except_the_last() {
    // Two blocks of 4 << 4 = 64 bytes. The first ends with an unused tail.
    let layout = {
        let mut layout = pool_layout(4);
        layout.entry.len_shift = 1;
        layout
    };
    let second = BLOCK + 0x100;
    let mut img = Img::new(BASE, SIZE);
    let mut at = narrow(&mut img, BLOCK, b"None");
    at = narrow(&mut img, at, b"First");
    assert!(at - BLOCK <= 64);
    let end = narrow(&mut img, second, b"Second");
    img.u64(POOL + 0x10, BLOCK);
    img.u64(POOL + 0x18, second);
    img.u32(POOL + 8, 1);
    img.u32(POOL + 0xC, (end - second) as u32);
    let snapshot = img.snapshot("game.exe");
    let names = load(&snapshot, layout.clone(), false);
    assert_eq!(names.resolve_index(0), Some("None"));
    assert_eq!(names.resolve_index(2), Some("First"));
    // Block 1 starts at index 1 << 4.
    assert_eq!(names.resolve_index(16), Some("Second"));
    assert!(names.is_entry_start(16) && !names.is_entry_start(17));
    assert_eq!(names.stats().blocks, 2);

    // A cursor beyond the block is a damaged last block, which is an error
    // only because block 1 is not the first.
    let mut broken = Img::new(BASE, SIZE);
    broken.put(BLOCK, &[0; 8]);
    broken.u64(POOL + 0x10, BLOCK);
    broken.u64(POOL + 0x18, second);
    broken.u32(POOL + 8, 1);
    broken.u32(POOL + 0xC, 1000);
    let snapshot = broken.snapshot("game.exe");
    let names = load(&snapshot, layout, false);
    assert_eq!(names.stats().unreadable_blocks.len(), 1);
}

#[test]
fn iteration_is_in_index_order_and_skips_entries_without_text() {
    let snapshot = array_image().snapshot("game.exe");
    let names = load_array(&snapshot, array_layout()).unwrap();
    let listed: Vec<u32> = names.iter().map(|(index, _)| index).collect();
    assert_eq!(listed, [0, 1, 2, 3, 7]);
    assert!(!names.is_empty());
    assert!(NameTable::default().is_empty());
}
