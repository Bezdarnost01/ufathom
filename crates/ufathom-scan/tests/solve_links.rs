//! `UStruct::Children` among the other pointers to a struct's own members.
//!
//! A linked struct holds `PropertyLink`, `RefLink`, `DestructorLink` and
//! `PostConstructLink` next to `Children`. Their heads are often properties
//! whose outer is the struct, so they pass the test that finds `Children`.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::print_stderr,
    reason = "test code may panic on failure"
)]

mod solve_common;

use solve_common::{assert_equal_to_truth, build, peek_u64, poke, rebuild, run};
use ufathom_scan::{DiscoverOptions, ScanError, discover};
use ufathom_testkit::{Fixture, Profile};

const CLASS: &str = "/Script/CoreUObject.Class";
const SCRIPT_STRUCT: &str = "/Script/CoreUObject.ScriptStruct";

fn named(name: &str) -> Profile {
    Profile::by_name(name).unwrap_or_else(|| panic!("{name}"))
}

/// The addresses of all classes and script structs of the image.
fn structs_of(fx: &Fixture) -> Vec<u64> {
    fx.objects
        .iter()
        .filter(|r| r.class_path == CLASS || r.class_path == SCRIPT_STRUCT)
        .map(|r| r.address)
        .collect()
}

#[test]
fn the_measured_421_build_holds_a_link_chain_that_looks_like_children() {
    let profile = Profile::ue4_21_measured();
    // The measured layout: Children 0x38, DestructorLink 0x68.
    assert_eq!(profile.layout.ustruct.children, 0x38);
    assert_eq!(profile.shape.struct_links.destructor_link, 0x68);

    let fx = build(profile, 1, 300);
    let actor = fx.object("/Script/Engine.Actor").unwrap().address;
    let tags = fx.object("/Script/Engine.Actor:Tags").unwrap().address;
    let children = peek_u64(&fx.parts, actor + 0x38);
    let destructor = peek_u64(&fx.parts, actor + 0x68);
    assert_ne!(children, 0);
    assert_eq!(
        destructor, tags,
        "the first member that needs destruction heads the chain"
    );
    assert_ne!(
        destructor, children,
        "the two lists must start at different members"
    );

    let d = run(&fx);
    assert_equal_to_truth(&fx, &d);
    assert_eq!(d.layout.ustruct.children, 0x38);
}

#[test]
fn children_is_found_next_to_link_chains_in_every_era() {
    for name in [
        "ue4.11",
        "ue4.18",
        "ue4.21-measured",
        "ue4.22-cp-measured",
        "ue4.24",
        "ue4.25-cp-measured",
        "ue5.7-cp",
        "shuffled-3",
        "shuffled-11",
    ] {
        let fx = build(named(name), 2, 300);
        let d = run(&fx);
        assert_equal_to_truth(&fx, &d);
    }
}

/// Copies the `Children` head of every struct into each of `offsets`.
fn copy_children_into(
    fx: &Fixture,
    offsets: &[u64],
    clear_children: bool,
) -> ufathom_mem::Snapshot {
    let children = u64::from(fx.layout.ustruct.children);
    rebuild(fx, |parts| {
        for addr in structs_of(fx) {
            let head = peek_u64(parts, addr + children);
            for &offset in offsets {
                poke(parts, addr + offset, &head.to_le_bytes());
            }
            if clear_children {
                poke(parts, addr + children, &0u64.to_le_bytes());
            }
        }
    })
}

#[test]
fn identical_lists_are_told_apart_by_the_position_after_super_struct() {
    let fx = build(Profile::ue4_21_measured(), 1, 300);
    // PropertyLink and RefLink now lead to exactly the members Children does.
    let snapshot = copy_children_into(&fx, &[0x58, 0x60], false);
    let d = discover(&snapshot, &DiscoverOptions::default()).unwrap();
    assert_eq!(d.layout.ustruct.children, 0x38);
    assert_eq!(d.layout.ufield.next, fx.layout.ufield.next);
}

#[test]
fn identical_lists_away_from_super_struct_are_reported_not_guessed() {
    let fx = build(Profile::ue4_21_measured(), 1, 300);
    // Children is gone; two equally wide pointers remain, neither of them
    // right after SuperStruct.
    let snapshot = copy_children_into(&fx, &[0x58, 0x60], true);
    match discover(&snapshot, &DiscoverOptions::default()) {
        Err(ScanError::Ambiguous { field, candidates }) => {
            assert_eq!(field, "ustruct.children");
            assert!(candidates.contains("0x58") && candidates.contains("0x60"));
            assert!(!candidates.contains("0x68"), "{candidates}");
        }
        other => panic!("expected an ambiguity, got {other:?}"),
    }
}
