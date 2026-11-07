//! Exact recovery: `discover` with no hints must return the layout and the
//! globals every fixture was built from.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test code may panic on failure"
)]

mod solve_common;

use solve_common::{assert_equal_to_truth, build, run};
use ufathom_testkit::Profile;

fn recover(profile: Profile, seed: u64) {
    let fx = build(profile, seed, 300);
    let d = run(&fx);
    assert_equal_to_truth(&fx, &d);
}

#[test]
fn stock_profiles_of_every_supported_version() {
    let versions = (411..=427).chain(500..=508);
    for number in versions {
        let name = format!("ue{}.{}", number / 100, number % 100);
        recover(
            Profile::by_name(&name).unwrap_or_else(|| panic!("{name}")),
            1,
        );
    }
}

#[test]
fn case_preserving_profiles_of_every_supported_version() {
    let versions = (422..=427).chain(500..=508);
    for number in versions {
        let name = format!("ue{}.{}-cp", number / 100, number % 100);
        recover(
            Profile::by_name(&name).unwrap_or_else(|| panic!("{name}")),
            2,
        );
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
        recover(profile, 3);
    }
}

#[test]
fn thirty_shuffled_seeds() {
    for seed in 0..30 {
        recover(Profile::shuffled(seed), seed);
    }
}
