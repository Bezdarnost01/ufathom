//! The dump equals the fixture's truth, for every profile family.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test code may panic on failure"
)]

mod common;

use common::{assert_matches_truth, build, dump};
use ufathom_testkit::{ChainOrder, FixtureBuilder, Profile};

fn check(profile: Profile, seed: u64) {
    let fx = build(profile, seed, 300);
    let out = dump(&fx);
    assert!(
        !out.degraded && out.dump.diagnostics.is_empty(),
        "{}: degraded {}, diagnostics {:?}",
        fx.profile.name,
        out.degraded,
        out.dump.diagnostics
    );
    assert_eq!(out.stats.failures, 0, "{}", fx.profile.name);
    assert_matches_truth(&fx, &out);
}

#[test]
fn stock_profiles_of_every_supported_version() {
    for number in (411..=427).chain(500..=508) {
        let name = format!("ue{}.{}", number / 100, number % 100);
        check(Profile::by_name(&name).unwrap(), 1);
    }
}

#[test]
fn case_preserving_profiles_of_every_supported_version() {
    for number in (422..=427).chain(500..=508) {
        let name = format!("ue{}.{}-cp", number / 100, number % 100);
        check(Profile::by_name(&name).unwrap(), 2);
    }
}

#[test]
fn measured_profiles() {
    for profile in [
        Profile::ue4_21_measured(),
        Profile::ue4_22_case_preserving_measured(),
        Profile::ue4_25_case_preserving_measured(),
        Profile::ue5_case_preserving_assumed(),
    ] {
        check(profile, 3);
    }
}

#[test]
fn thirty_shuffled_seeds() {
    for seed in 0..30 {
        check(Profile::shuffled(seed), seed);
    }
}

#[test]
fn mangled_display_names_keep_the_display_text() {
    for name in ["ue4.22-cp", "ue4.25-cp", "ue5.3-cp"] {
        let fx = FixtureBuilder::new(Profile::by_name(name).unwrap())
            .seed(9)
            .filler_objects(300)
            .name_blocks(3)
            .mangled_display_names(200)
            .build();
        let out = dump(&fx);
        assert_eq!(out.stats.failures, 0, "{name}");
        assert_matches_truth(&fx, &out);
    }
}

#[test]
fn every_chain_order_gives_the_same_dump() {
    for name in ["ue4.21", "ue4.25-cp", "ue5.3-cp"] {
        for order in [
            ChainOrder::Declaration,
            ChainOrder::Reversed,
            ChainOrder::FunctionsFirst,
        ] {
            let fx = FixtureBuilder::new(Profile::by_name(name).unwrap())
                .seed(4)
                .filler_objects(200)
                .chain_order(order)
                .build();
            let out = dump(&fx);
            assert_eq!(out.stats.failures, 0, "{name} {order:?}");
            assert_matches_truth(&fx, &out);
        }
    }
}

#[test]
fn the_header_names_the_target_and_the_tool() {
    let fx = build(Profile::ue4_25_case_preserving_measured(), 5, 100);
    let out = dump(&fx);
    assert_eq!(out.dump.target.module, fx.module_name());
    assert_eq!(out.dump.target.build_id, fx.engine.build);
    assert_eq!(out.dump.layout, discovery_layout(&fx));
    assert_eq!(out.dump.tool.name, "ufathom");
    assert!(out.dump.packages.contains(&"/Script/Engine".to_owned()));
    assert!(out.dump.packages.contains(&"/Game/Maps/Main".to_owned()));
    assert_eq!(
        out.stats.classes + out.stats.structs,
        out.dump.structs.len() as u64
    );
    assert_eq!(out.stats.enums, out.dump.enums.len() as u64);
    let functions: usize = out.dump.structs.iter().map(|s| s.functions.len()).sum();
    assert_eq!(out.stats.functions, functions as u64);
}

fn discovery_layout(fx: &ufathom_testkit::Fixture) -> ufathom_model::EngineLayout {
    common::discovery(fx).layout
}
