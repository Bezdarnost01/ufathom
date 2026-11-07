//! Struct, function and enum views, super chains and member lists on
//! hand-built memory.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "test code may panic on failure; clippy.toml only exempts #[test] functions, not helpers"
)]

mod common;

use common::{Img, small};
use ufathom_engine::limits::{
    MAX_CHAIN_LENGTH, MAX_ENUM_ENTRIES, MAX_MEMBER_CHAIN_LENGTH, MAX_PROPERTIES_PER_STRUCT,
};
use ufathom_engine::{
    EngineError, ModernProperties, NameTable, PropertySystemReader, RawPayload, RawProperty,
    read_enum_entries, read_function, read_struct, short_enum_value_name, super_chain,
    validate_members,
};
use ufathom_model::PropertyType;
use ufathom_model::layout::{FNameLayout, UEnumLayout, UFunctionLayout, UStructLayout};
use ufathom_testkit::Profile;

const BASE: u64 = 0x30_0000;

fn fname() -> FNameLayout {
    FNameLayout {
        size: 8,
        comparison_index: 0,
        display_index: None,
        number: Some(4),
    }
}

fn name_bytes(index: u32) -> [u8; 8] {
    let mut bytes = [0u8; 8];
    bytes[..4].copy_from_slice(&index.to_le_bytes());
    bytes
}

fn pair_layout(stride: u32, value_offset: u32, value_width: u32) -> UEnumLayout {
    UEnumLayout::PairArray {
        offset: 0x40,
        stride,
        value_offset,
        value_width,
    }
}

const ENUM: u64 = BASE + 0x100;
const DATA: u64 = BASE + 0x1000;

/// An enum with the entries `(name index, value)` in a pair array.
fn pair_image(stride: u32, value_offset: u32, width: usize, entries: &[(u32, i64)]) -> Img {
    let mut img = Img::new(BASE, 0x4000);
    img.u64(ENUM + 0x40, DATA);
    img.u32(ENUM + 0x48, entries.len() as u32);
    img.u32(ENUM + 0x4C, entries.len() as u32);
    for (n, (name, value)) in entries.iter().enumerate() {
        let at = DATA + n as u64 * u64::from(stride);
        img.put(at, &name_bytes(*name));
        img.put(at + u64::from(value_offset), &value.to_le_bytes()[..width]);
    }
    img
}

#[test]
fn pair_array_enums_widen_values_by_their_width() {
    let entries = [(1, 0), (2, -1), (3, 255)];
    // Eight-byte values keep their sign.
    let img = pair_image(16, 8, 8, &entries);
    let got = read_enum_entries(
        &img.snapshot("g.exe"),
        &pair_layout(16, 8, 8),
        &fname(),
        ENUM,
    )
    .unwrap();
    let values: Vec<i64> = got.iter().map(|e| e.value).collect();
    assert_eq!(values, [0, -1, 255]);
    assert_eq!(got[1].name.as_slice(), name_bytes(2));
    // One-byte values are unsigned: 0xFF is 255, not -1.
    let img = pair_image(12, 8, 1, &[(1, 0), (2, 255)]);
    let got = read_enum_entries(
        &img.snapshot("g.exe"),
        &pair_layout(12, 8, 1),
        &fname(),
        ENUM,
    )
    .unwrap();
    assert_eq!(got.iter().map(|e| e.value).collect::<Vec<_>>(), [0, 255]);
    // Wider narrow values are signed.
    for width in [2usize, 4] {
        let img = pair_image(16, 8, width, &[(1, -2)]);
        let layout = pair_layout(16, 8, width as u32);
        let got = read_enum_entries(&img.snapshot("g.exe"), &layout, &fname(), ENUM).unwrap();
        assert_eq!(got[0].value, -2, "width {width}");
    }
}

#[test]
fn enum_counts_are_checked_before_anything_is_allocated() {
    let snapshot = |count: u32, data: u64| {
        let mut img = pair_image(16, 8, 8, &[(1, 1)]);
        img.u32(ENUM + 0x48, count);
        img.u64(ENUM + 0x40, data);
        img.snapshot("g.exe")
    };
    let read = |memory: &ufathom_mem::Snapshot| {
        read_enum_entries(memory, &pair_layout(16, 8, 8), &fname(), ENUM)
    };
    for count in [
        i32::MAX as u32,
        0x8000_0000,
        u32::MAX,
        MAX_ENUM_ENTRIES as u32 + 1,
    ] {
        let error = read(&snapshot(count, DATA)).unwrap_err();
        assert!(
            matches!(error, EngineError::Limit { .. }),
            "{count:#x}: {error}"
        );
    }
    // The limit itself is allowed by the check (the read then fails on size).
    let error = read(&snapshot(MAX_ENUM_ENTRIES as u32, DATA)).unwrap_err();
    assert!(matches!(error, EngineError::Read { .. }), "{error}");
    // An empty enum needs no data pointer; a non-empty one does.
    assert!(read(&snapshot(0, 0)).unwrap().is_empty());
    assert!(matches!(
        read(&snapshot(2, 0)).unwrap_err(),
        EngineError::Null { .. }
    ));
    assert!(matches!(
        read(&snapshot(2, 0xDEAD_0000)).unwrap_err(),
        EngineError::Read { .. }
    ));
    // Layouts that cannot hold the fields.
    let memory = snapshot(1, DATA);
    for layout in [
        pair_layout(4, 8, 8),
        pair_layout(16, 12, 8),
        pair_layout(16, 8, 3),
    ] {
        let error = read_enum_entries(&memory, &layout, &fname(), ENUM).unwrap_err();
        assert!(
            matches!(error, EngineError::InvalidLayout { .. }),
            "{layout:?}: {error}"
        );
    }
    let mut huge = fname();
    huge.size = 0;
    assert!(read_enum_entries(&memory, &pair_layout(16, 8, 8), &huge, ENUM).is_err());
}

#[test]
fn split_array_enums_mask_the_tag_bits_of_both_pointers() {
    let names = BASE + 0x2000;
    let values = BASE + 0x2800;
    let mut img = Img::new(BASE, 0x4000);
    img.u64(ENUM + 0x40, names | 1);
    img.u64(ENUM + 0x48, values | 2);
    img.u32(ENUM + 0x50, 3);
    for (n, (name, value)) in [(10u32, 5i64), (11, -7), (12, i64::MAX)]
        .into_iter()
        .enumerate()
    {
        img.put(names + n as u64 * 8, &name_bytes(name));
        img.put(values + n as u64 * 8, &value.to_le_bytes());
    }
    let layout = UEnumLayout::SplitArrays {
        names: 0x40,
        values: 0x48,
        count: 0x50,
    };
    let got = read_enum_entries(&img.snapshot("g.exe"), &layout, &fname(), ENUM).unwrap();
    assert_eq!(
        got.iter().map(|e| e.value).collect::<Vec<_>>(),
        [5, -7, i64::MAX]
    );
    assert_eq!(got[2].name.as_slice(), name_bytes(12));

    // A count above the limit, and a null names pointer.
    img.u32(ENUM + 0x50, i32::MAX as u32);
    let error = read_enum_entries(&img.snapshot("g.exe"), &layout, &fname(), ENUM).unwrap_err();
    assert!(matches!(error, EngineError::Limit { .. }), "{error}");
    img.u32(ENUM + 0x50, 3);
    img.u64(ENUM + 0x40, 2);
    let error = read_enum_entries(&img.snapshot("g.exe"), &layout, &fname(), ENUM).unwrap_err();
    assert!(matches!(error, EngineError::Null { .. }), "{error}");
}

#[test]
fn enum_value_names_lose_only_their_prefix() {
    assert_eq!(short_enum_value_name("ENetRole::ROLE_None"), "ROLE_None");
    assert_eq!(short_enum_value_name("Bare"), "Bare");
    assert_eq!(short_enum_value_name("A::B::C"), "C");
    assert_eq!(short_enum_value_name(""), "");
}

#[test]
fn a_struct_reads_min_alignment_with_the_stated_width() {
    let mut img = Img::new(BASE, 0x1000);
    img.u64(ENUM + 0x30, 0x1111);
    img.u64(ENUM + 0x38, 0x2222);
    img.u64(ENUM + 0x40, 0x3333);
    img.u32(ENUM + 0x48, 0x60);
    img.u32(ENUM + 0x4C, 0xAAAA_0010);
    let snapshot = img.snapshot("g.exe");
    let layout = |width: u32, child_properties: Option<u32>| UStructLayout {
        super_struct: 0x30,
        children: 0x38,
        child_properties,
        properties_size: 0x48,
        min_alignment: 0x4C,
        min_alignment_width: width,
    };
    let view = read_struct(&snapshot, &layout(2, Some(0x40)), ENUM).unwrap();
    assert_eq!(
        (view.super_struct, view.children, view.child_properties),
        (0x1111, 0x2222, Some(0x3333))
    );
    assert_eq!((view.properties_size, view.min_alignment), (0x60, 0x10));
    assert_eq!(
        read_struct(&snapshot, &layout(4, None), ENUM)
            .unwrap()
            .min_alignment,
        0xAAAA_0010
    );
    assert_eq!(
        read_struct(&snapshot, &layout(1, None), ENUM)
            .unwrap()
            .min_alignment,
        0x10
    );
    assert_eq!(
        read_struct(&snapshot, &layout(4, None), ENUM)
            .unwrap()
            .child_properties,
        None
    );
    assert!(matches!(
        read_struct(&snapshot, &layout(3, None), ENUM).unwrap_err(),
        EngineError::InvalidLayout { .. }
    ));
    assert!(matches!(
        read_struct(&snapshot, &layout(4, None), 0xDEAD_0000).unwrap_err(),
        EngineError::Read { .. }
    ));
}

#[test]
fn a_function_reads_flags_and_a_native_entry_point() {
    let mut img = Img::new(BASE, 0x1000);
    img.u32(ENUM + 0x80, 0x0400_0401);
    img.u64(ENUM + 0x88, 0x7FF6_1234);
    let snapshot = img.snapshot("g.exe");
    let layout = UFunctionLayout {
        flags: 0x80,
        native_func: Some(0x88),
    };
    let view = read_function(&snapshot, &layout, ENUM).unwrap();
    assert_eq!(
        (view.flags, view.native_func),
        (0x0400_0401, Some(0x7FF6_1234))
    );
    // A null entry point is a script function; an absent field is unknown.
    let at_zero = UFunctionLayout {
        flags: 0x80,
        native_func: Some(0x90),
    };
    assert_eq!(
        read_function(&snapshot, &at_zero, ENUM)
            .unwrap()
            .native_func,
        None
    );
    let without = UFunctionLayout {
        flags: 0x80,
        native_func: None,
    };
    assert_eq!(
        read_function(&snapshot, &without, ENUM)
            .unwrap()
            .native_func,
        None
    );
}

fn chain_layout() -> UStructLayout {
    UStructLayout {
        super_struct: 0,
        children: 8,
        child_properties: None,
        properties_size: 0x10,
        min_alignment: 0x14,
        min_alignment_width: 4,
    }
}

#[test]
fn a_super_chain_is_bounded_and_reports_loops_and_unreadable_links() {
    let node = |n: u64| BASE + 0x100 + n * 0x10;
    // 300 links in a row.
    let mut img = Img::new(BASE, 0x4000);
    for n in 0..300 {
        img.u64(node(n), node(n + 1));
    }
    img.u64(node(299), 0);
    let snapshot = img.snapshot("g.exe");
    let items: Vec<_> = super_chain(&snapshot, &chain_layout(), node(0)).collect();
    assert_eq!(items.len(), MAX_CHAIN_LENGTH + 1);
    assert!(items[..MAX_CHAIN_LENGTH].iter().all(Result::is_ok));
    assert!(matches!(
        items[MAX_CHAIN_LENGTH],
        Err(EngineError::Limit { .. })
    ));

    // A struct that is its own super: once, then the loop.
    let mut img = Img::new(BASE, 0x1000);
    img.u64(node(0), node(0));
    let items: Vec<_> = super_chain(&img.snapshot("g.exe"), &chain_layout(), node(0)).collect();
    assert!(matches!(
        items.as_slice(),
        [Ok(_), Err(EngineError::Cycle { .. })]
    ));

    // A short chain ends cleanly; a null start is empty; an unreadable link
    // is reported instead of being handed out.
    let mut img = Img::new(BASE, 0x1000);
    img.u64(node(0), node(1));
    img.u64(node(1), 0xDEAD_0000);
    let snapshot = img.snapshot("g.exe");
    let items: Vec<_> = super_chain(&snapshot, &chain_layout(), node(0)).collect();
    assert!(matches!(
        items.as_slice(),
        [Ok(_), Ok(_), Err(EngineError::Read { .. })]
    ));
    assert_eq!(super_chain(&snapshot, &chain_layout(), 0).count(), 0);
}

const OWNER: u64 = BASE + 0x100;
const NODES: u64 = BASE + 0x1_0000;
const NODE_SIZE: u64 = 0x40;

/// A modern property list of `count` nodes, each pointing to the next, read
/// with the layout of a UE 4.25 build.
fn member_chain(count: u64) -> (ufathom_testkit::Fixture, Img) {
    let fx = small(Profile::ue4_25_case_preserving_measured());
    let child_properties = u64::from(fx.layout.ustruct.child_properties.unwrap());
    let next = member_next(&fx);
    let mut img = Img::new(BASE, 0x1_0000 + (count as usize + 2) * NODE_SIZE as usize);
    img.u64(OWNER + child_properties, NODES);
    for n in 0..count {
        let at = NODES + n * NODE_SIZE;
        img.u64(at + next, if n + 1 < count { at + NODE_SIZE } else { 0 });
    }
    (fx, img)
}

fn member_next(fx: &ufathom_testkit::Fixture) -> u64 {
    match &fx.layout.properties.system {
        ufathom_model::layout::PropertySystem::Modern { ffield, .. } => u64::from(ffield.next),
        ufathom_model::layout::PropertySystem::Legacy => panic!("a modern profile"),
    }
}

#[test]
fn a_member_list_is_bounded_by_the_property_limit() {
    let (fx, img) = member_chain(MAX_PROPERTIES_PER_STRUCT as u64 + 10);
    let snapshot = img.snapshot("g.exe");
    let names = NameTable::default();
    let system = ModernProperties::new(&snapshot, &fx.layout, &names);
    let items: Vec<_> = system.fields(OWNER).collect();
    assert_eq!(items.len(), MAX_PROPERTIES_PER_STRUCT + 1);
    assert!(items[..MAX_PROPERTIES_PER_STRUCT].iter().all(Result::is_ok));
    assert!(matches!(
        items[MAX_PROPERTIES_PER_STRUCT],
        Err(EngineError::Limit { .. })
    ));
    // Exactly at the limit is fine.
    let (fx, img) = member_chain(MAX_PROPERTIES_PER_STRUCT as u64);
    let snapshot = img.snapshot("g.exe");
    let system = ModernProperties::new(&snapshot, &fx.layout, &names);
    assert!(system.fields(OWNER).all(|item| item.is_ok()));
}

#[test]
fn a_member_list_reports_loops_unreadable_links_and_empty_lists() {
    let (fx, mut img) = member_chain(3);
    let next = member_next(&fx);
    let names = NameTable::default();
    let node = |n: u64| NODES + n * NODE_SIZE;
    let run = |img: &Img| {
        let snapshot = img.snapshot("g.exe");
        let system = ModernProperties::new(&snapshot, &fx.layout, &names);
        system
            .fields(OWNER)
            .map(|item| item.map_err(|e| e.root().to_string()))
            .collect::<Vec<_>>()
    };
    let clean = run(&img);
    assert_eq!(clean, [Ok(node(0)), Ok(node(1)), Ok(node(2))]);

    // The last node points back at the first: three nodes once, then a cycle.
    img.u64(node(2) + next, node(0));
    let looped = run(&img);
    assert_eq!(&looped[..3], &clean[..]);
    assert!(
        looped[3].as_ref().unwrap_err().contains("cycle"),
        "{looped:?}"
    );
    assert_eq!(looped.len(), 4);

    // A node that points at itself.
    img.u64(node(2) + next, node(2));
    let own = run(&img);
    assert!(own[3].as_ref().unwrap_err().contains("cycle"), "{own:?}");

    // A link into unmapped memory: the nodes before it, then one error.
    img.u64(node(2) + next, 0xDEAD_0000);
    let dangling = run(&img);
    assert_eq!(dangling.len(), 4);
    assert!(
        dangling[3].as_ref().unwrap_err().contains("cannot read"),
        "{dangling:?}"
    );

    // An empty list, and an unreadable owner.
    img.u64(
        OWNER + u64::from(fx.layout.ustruct.child_properties.unwrap()),
        0,
    );
    assert!(run(&img).is_empty());
    let snapshot = img.snapshot("g.exe");
    let system = ModernProperties::new(&snapshot, &fx.layout, &names);
    let items: Vec<_> = system.fields(0xDEAD_0000).collect();
    assert!(matches!(items.as_slice(), [Err(EngineError::Read { .. })]));
}

#[test]
fn a_function_list_with_no_functions_is_bounded_by_the_chain_limit() {
    // Every link has a null class: each is an error item, and the walk still
    // stops at the chain limit.
    let (fx, mut img) = member_chain(0);
    let children = u64::from(fx.layout.ustruct.children);
    let next = u64::from(fx.layout.ufield.next);
    let count = MAX_MEMBER_CHAIN_LENGTH as u64 + 5;
    let mut big = Img::new(BASE, 0x1_0000 + (count as usize + 2) * NODE_SIZE as usize);
    big.u64(OWNER + children, NODES);
    for n in 0..count {
        let at = NODES + n * NODE_SIZE;
        big.u64(at + next, at + NODE_SIZE);
    }
    img.u64(OWNER, 0);
    let snapshot = big.snapshot("g.exe");
    let names = NameTable::default();
    let system = ModernProperties::new(&snapshot, &fx.layout, &names);
    let items: Vec<_> = system.functions(OWNER).collect();
    assert_eq!(items.len(), MAX_MEMBER_CHAIN_LENGTH + 1);
    assert!(
        items[..MAX_MEMBER_CHAIN_LENGTH]
            .iter()
            .all(|item| matches!(item, Err(EngineError::Null { .. })))
    );
    assert!(matches!(
        items[MAX_MEMBER_CHAIN_LENGTH],
        Err(EngineError::Limit { .. })
    ));
}

#[test]
fn a_layout_without_the_modern_fields_gives_error_items_not_panics() {
    let fx = small(Profile::ue4_25_case_preserving_measured());
    let snapshot = Img::new(BASE, 0x1000).snapshot("g.exe");
    let names = NameTable::default();
    // No ChildProperties offset.
    let mut layout = fx.layout.clone();
    layout.ustruct.child_properties = None;
    let system = ModernProperties::new(&snapshot, &layout, &names);
    let items: Vec<_> = system.fields(OWNER).collect();
    assert!(matches!(
        items.as_slice(),
        [Err(EngineError::InvalidLayout { .. })]
    ));
    // A legacy layout given to the modern reader.
    let legacy = small(Profile::ue4_21_measured()).layout;
    let system = ModernProperties::new(&snapshot, &legacy, &names);
    assert!(matches!(
        system.fields(OWNER).next(),
        Some(Err(EngineError::InvalidLayout { .. }))
    ));
    assert!(matches!(
        system.decode(OWNER),
        Err(EngineError::InvalidLayout { .. })
    ));
}

fn member(name: &str, class: &str, offset: u32, size: u32, dim: u32) -> RawProperty {
    RawProperty {
        address: 0x1000 + u64::from(offset),
        name: name.to_owned(),
        class_name: class.to_owned(),
        offset,
        element_size: size,
        array_dim: dim,
        flags: 0,
        payload: RawPayload::Scalar,
    }
}

#[test]
fn members_are_checked_against_their_owner_without_overflow() {
    let ok = [
        member("A", "IntProperty", 0, 4, 1),
        member("B", "IntProperty", 4, 4, 2),
        member("C", "FloatProperty", 12, 4, 1),
    ];
    assert!(validate_members(16, &ok).is_empty());
    // A member past the end, a zero size, a zero dimension.
    let problems = validate_members(
        16,
        &[
            member("Past", "IntProperty", 14, 4, 1),
            member("NoSize", "IntProperty", 0, 0, 1),
            member("NoDim", "IntProperty", 4, 4, 0),
        ],
    );
    assert!(problems.len() >= 3, "{problems:?}");
    // Overlap.
    let problems = validate_members(
        16,
        &[
            member("A", "IntProperty", 0, 8, 1),
            member("B", "IntProperty", 4, 4, 1),
        ],
    );
    assert_eq!(problems.len(), 1, "{problems:?}");
    assert_eq!(problems[0].addr(), Some(0x1004));
    // A long member hides an overlap behind a short one in between.
    let problems = validate_members(
        32,
        &[
            member("Long", "ArrayProperty", 0, 16, 1),
            member("Short", "IntProperty", 2, 2, 1),
            member("Inside", "IntProperty", 12, 4, 1),
        ],
    );
    assert_eq!(problems.len(), 2, "{problems:?}");
    // Bit-field booleans share storage and do not overlap.
    let bits = [
        member("bA", "BoolProperty", 8, 1, 1),
        member("bB", "BoolProperty", 8, 1, 1),
        member("bC", "BoolProperty", 8, 1, 1),
    ];
    assert!(validate_members(16, &bits).is_empty());
    let mixed = [
        member("bA", "BoolProperty", 8, 1, 1),
        member("N", "IntProperty", 8, 4, 1),
    ];
    assert_eq!(validate_members(16, &mixed).len(), 1);
    // Hostile numbers: no overflow, no panic.
    let hostile = [
        member("Max", "IntProperty", u32::MAX, u32::MAX, u32::MAX),
        member("Big", "IntProperty", 0, u32::MAX, u32::MAX),
    ];
    assert!(!validate_members(u32::MAX, &hostile).is_empty());
    assert!(validate_members(0, &[]).is_empty());
    let _ = PropertyType::Int32;
}
