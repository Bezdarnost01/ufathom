//! The name table search outside its usual place: the fallback scan of memory
//! that is not the module's own data.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::print_stderr,
    reason = "test code may panic on failure"
)]

mod solve_common;

use solve_common::{absolute, build, poke, rebuild};
use ufathom_mem::{Perms, RegionKind};
use ufathom_model::layout::NameTableLayout;
use ufathom_scan::{DiscoverOptions, discover};
use ufathom_testkit::{Part, PartKind, Profile};

fn named(name: &str) -> Profile {
    Profile::by_name(name).unwrap_or_else(|| panic!("{name}"))
}

#[test]
fn a_pool_that_lives_on_the_heap_is_found_by_the_scan_of_other_memory() {
    for name in ["ue4.25-cp", "ue5.7"] {
        let fx = build(named(name), 3, 300);
        let NameTableLayout::Pool(truth) = &fx.layout.names else {
            panic!("{name}: not a pool");
        };
        let anchor = fx.anchors.data.name_anchor;
        let size = usize::try_from(truth.blocks).unwrap() + 8192 * 8;
        let moved = fx.addresses.heap + fx.heap_span + 0x100_0000;
        let mut copy = Vec::new();
        let snapshot = rebuild(&fx, |parts| {
            let part = parts
                .iter()
                .find(|p| anchor >= p.addr && anchor < p.addr + p.bytes.len() as u64)
                .unwrap();
            let at = (anchor - part.addr) as usize;
            copy = part.bytes[at..at + size].to_vec();
            // Nothing in the module's data points at the blocks any more.
            poke(parts, anchor, &vec![0u8; size]);
            let mut page = copy.clone();
            page.resize(page.len().next_multiple_of(0x1000), 0);
            parts.push(Part {
                kind: PartKind::Heap,
                addr: moved,
                bytes: page,
                perms: Perms::RW,
                region_kind: RegionKind::Private,
            });
        });
        let d = discover(&snapshot, &DiscoverOptions::default())
            .unwrap_or_else(|e| panic!("{name}: {e}"));
        let NameTableLayout::Pool(got) = &d.layout.names else {
            panic!("{name}: not a pool");
        };
        // The pool is where it was moved to, and everything else about it is
        // what it was.
        let base = absolute(&fx, &got.location);
        assert_eq!(
            base + u64::from(got.blocks),
            moved + u64::from(truth.blocks),
            "{name}"
        );
        assert_eq!(
            base + u64::from(got.current_block),
            moved + u64::from(truth.current_block),
            "{name}"
        );
        assert_eq!(
            base + u64::from(got.cursor),
            moved + u64::from(truth.cursor),
            "{name}"
        );
        assert_eq!(
            (got.stride, got.block_offset_bits),
            (truth.stride, truth.block_offset_bits),
            "{name}"
        );
        assert_eq!(got.entry, truth.entry, "{name}");
        assert_eq!(d.layout.uobject, fx.layout.uobject, "{name}");
        let evidence = d.evidence_for("names.pool.blocks").unwrap();
        assert!(
            evidence.method.contains("scan of other memory"),
            "{name}: {}",
            evidence.method
        );
    }
}

#[test]
fn an_array_without_a_pointer_in_module_data_is_found_by_the_scan_of_other_memory() {
    let fx = build(named("ue4.21"), 3, 300);
    let NameTableLayout::Array(truth) = &fx.layout.names else {
        panic!("not an array");
    };
    let snapshot = rebuild(&fx, |parts| {
        // The GNames slot is the only pointer to the table.
        poke(parts, fx.anchors.data.name_anchor, &[0u8; 8]);
    });
    let d = discover(&snapshot, &DiscoverOptions::default()).unwrap();
    let NameTableLayout::Array(got) = &d.layout.names else {
        panic!("not an array");
    };
    assert!(!got.indirect);
    assert_eq!(
        absolute(&fx, &got.location),
        fx.anchors.data.names.table_object
    );
    assert_eq!(got.num_elements, truth.num_elements);
    assert_eq!(got.num_chunks, truth.num_chunks);
    assert_eq!(got.elements_per_chunk, truth.elements_per_chunk);
    assert_eq!(got.entry, truth.entry);
    assert_eq!(d.layout.uobject, fx.layout.uobject);
}
