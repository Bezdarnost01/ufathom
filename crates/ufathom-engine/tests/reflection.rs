//! Every view against the testkit's truth, across profiles and orders.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "test code may panic on failure; clippy.toml only exempts #[test] functions, not helpers"
)]

mod common;

use common::{assert_matches_truth, small, walk};
use ufathom_engine::{LoadOptions, NameTable};
use ufathom_model::EngineVersion;
use ufathom_testkit::{BuildOptions, ChainOrder, Fixture, FixtureBuilder, Profile};

fn versions() -> Vec<(u16, u16)> {
    (11..=27)
        .map(|minor| (4, minor))
        .chain((0..=8).map(|minor| (5, minor)))
        .collect()
}

fn profile(major: u16, minor: u16, case_preserving: bool) -> Profile {
    Profile::with_options(
        EngineVersion {
            major,
            minor,
            patch: 0,
        },
        BuildOptions {
            case_preserving_names: case_preserving,
            pool_entry: None,
        },
    )
    .unwrap()
}

fn check(profile: Profile) {
    let label = profile.name.clone();
    let fx = small(profile);
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(2)
        .build()
        .unwrap();
    assert_matches_truth(&fx, &walk(&fx, None), &label);
    assert_matches_truth(&fx, &walk(&fx, Some(&pool)), &label);
}

#[test]
fn every_stock_profile_reads_back_as_its_truth() {
    for (major, minor) in versions() {
        check(profile(major, minor, false));
    }
}

#[test]
fn every_case_preserving_profile_reads_back_as_its_truth() {
    for (major, minor) in versions() {
        if major == 5 || minor >= 22 {
            check(profile(major, minor, true));
        }
    }
}

#[test]
fn the_measured_profiles_read_back_as_their_truth() {
    for profile in [
        Profile::ue4_21_measured(),
        Profile::ue4_22_case_preserving_measured(),
        Profile::ue4_25_case_preserving_measured(),
        Profile::ue5_case_preserving_assumed(),
    ] {
        check(profile);
    }
}

#[test]
fn shuffled_profiles_read_back_as_their_truth_first_half() {
    for seed in 0..12 {
        check(Profile::shuffled(seed));
    }
}

#[test]
fn shuffled_profiles_read_back_as_their_truth_second_half() {
    for seed in 12..24 {
        check(Profile::shuffled(seed));
    }
}

#[test]
fn every_member_chain_order_reads_back_as_the_truth() {
    for profile in [
        Profile::ue4_21_measured(),
        Profile::ue4_25_case_preserving_measured(),
        profile(5, 7, false),
    ] {
        for order in [
            ChainOrder::Declaration,
            ChainOrder::Reversed,
            ChainOrder::FunctionsFirst,
        ] {
            let label = format!("{} {order:?}", profile.name);
            let fx = FixtureBuilder::new(profile.clone())
                .seed(9)
                .filler_objects(100)
                .chain_order(order)
                .build();
            assert_matches_truth(&fx, &walk(&fx, None), &label);
        }
    }
}

#[test]
fn text_comes_from_the_display_index_when_the_comparison_entry_is_mangled() {
    for profile in [
        Profile::ue4_22_case_preserving_measured(),
        Profile::ue4_25_case_preserving_measured(),
        Profile::ue5_case_preserving_assumed(),
    ] {
        let label = profile.name.clone();
        let fx = FixtureBuilder::new(profile)
            .seed(2)
            .filler_objects(100)
            .mangled_display_names(12)
            .build();
        // Paths, names and types all come out as the truth says.
        let walked = walk(&fx, None);
        assert_matches_truth(&fx, &walked, &label);

        // And the comparison index alone would have given other text.
        let names = walked.names.as_ref().unwrap();
        let layout = &fx.layout;
        let display_at = layout.fname.display_index.unwrap() as usize;
        let mut differing = 0;
        for header in walked.headers.values() {
            let bytes = header.name.as_slice();
            let comparison = u32::from_le_bytes(bytes[0..4].try_into().unwrap());
            let display = u32::from_le_bytes(bytes[display_at..display_at + 4].try_into().unwrap());
            if comparison != display {
                differing += 1;
                let from_display = names.resolve_index(display).unwrap();
                let from_comparison = names.resolve_index(comparison).unwrap();
                assert_ne!(from_display, from_comparison, "{label}");
                assert!(
                    from_display.eq_ignore_ascii_case(from_comparison),
                    "{label}"
                );
                assert_eq!(
                    names.resolve(&layout.fname, bytes).unwrap(),
                    from_display,
                    "{label}"
                );
            }
        }
        assert_eq!(differing, 12, "{label}");
    }
}

fn spanning(profile: Profile, blocks: usize) -> Fixture {
    FixtureBuilder::new(profile)
        .seed(4)
        .filler_objects(300)
        .name_blocks(blocks)
        .build()
}

#[test]
fn names_spanning_several_blocks_and_chunks_load_completely() {
    for profile in [
        Profile::ue4_21_measured(),
        Profile::ue4_25_case_preserving_measured(),
        profile(5, 7, false),
        Profile::shuffled(3),
    ] {
        let label = profile.name.clone();
        let fx = spanning(profile, 3);
        assert!(fx.anchors.data.names.blocks.len() >= 3, "{label}");
        let walked = walk(&fx, None);
        assert_matches_truth(&fx, &walked, &label);
        let names = walked.names.as_ref().unwrap();
        assert_eq!(
            names.stats().blocks as usize,
            fx.anchors.data.names.blocks.len()
        );
        // Every name the image wrote is in the table: the last block too.
        assert_eq!(
            names.stats().entries,
            fx.anchors.data.names.entries.len(),
            "{label}"
        );
        let max_index = names.iter().map(|(index, _)| index).max().unwrap();
        assert!(names.is_entry_start(max_index));
        assert!(
            names
                .resolve_index(max_index)
                .unwrap()
                .starts_with("PadName"),
            "{label}"
        );
    }
}

#[test]
fn parallel_and_sequential_loads_give_identical_tables() {
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(3)
        .build()
        .unwrap();
    for profile in [
        Profile::ue4_21_measured(),
        Profile::ue4_25_case_preserving_measured(),
        profile(5, 2, false),
        Profile::shuffled(5),
    ] {
        let label = profile.name.clone();
        let fx = spanning(profile, 4);
        let sequential = NameTable::load(
            &fx.memory,
            &fx.layout.names,
            &LoadOptions::for_layout(&fx.layout, None),
        )
        .unwrap();
        let parallel = NameTable::load(
            &fx.memory,
            &fx.layout.names,
            &LoadOptions::for_layout(&fx.layout, Some(&pool)),
        )
        .unwrap();
        assert_eq!(sequential.stats(), parallel.stats(), "{label}");
        let a: Vec<(u32, &str)> = sequential.iter().collect();
        let b: Vec<(u32, &str)> = parallel.iter().collect();
        assert_eq!(a, b, "{label}");
        assert!(a.len() > 1000, "{label}: {}", a.len());
    }
}

#[test]
fn decoded_types_cover_the_kinds_the_schema_uses() {
    for profile in [
        Profile::ue4_21_measured(),
        Profile::ue4_25_case_preserving_measured(),
        profile(5, 7, true),
    ] {
        let label = profile.name.clone();
        let fx = small(profile);
        let (kinds, total, unknown) = common::type_tally(&walk(&fx, None));
        println!("{label}: {total} members, {unknown} unknown, kinds {kinds:?}");
        assert!(total > 80, "{label}: {total}");
        // Only members with a plain byte may fall back (see assert_fields).
        assert!(unknown * 4 < total, "{label}: {unknown} of {total}");
        for expected in [
            "Bool",
            "Struct",
            "Array",
            "Map",
            "Set",
            "Object",
            "WeakObject",
            "LazyObject",
            "SoftObject",
            "SoftClass",
            "Class",
            "Float",
            "Int32",
            "Name",
            "Str",
            "Text",
            "Delegate",
            "MulticastInlineDelegate",
            "Byte",
        ] {
            assert!(
                kinds.contains(expected),
                "{label}: no {expected} in {kinds:?}"
            );
        }
    }
}
