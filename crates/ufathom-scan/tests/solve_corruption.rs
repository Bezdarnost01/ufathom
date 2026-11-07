//! Damaged images: every corruption either recovers the true layout or fails
//! with a named error. Nothing panics and nothing hangs.
//!
//! The expected outcome of each corruption on three profiles (UE 4.21, UE 4.25
//! case-preserving, UE 5.7) is stated in [`expected`] and checked.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test code may panic on failure"
)]

mod solve_common;

use std::sync::mpsc;
use std::time::Duration;

use solve_common::assert_essential_equal;
use ufathom_scan::{DiscoverOptions, Discovery, ScanError, discover};
use ufathom_testkit::{Corruption, Fixture, FixtureBuilder, Profile};

/// Longest a single discovery may take before the test calls it hung.
const WATCHDOG: Duration = Duration::from_secs(120);

/// What one discovery did.
enum Outcome {
    Recovered(Box<Discovery>),
    Failed(ScanError),
    Panicked,
    Hung,
}

/// Runs `discover` on a worker thread under a watchdog.
fn guarded(fx: &Fixture) -> Outcome {
    let memory = fx.memory.clone();
    let (tx, rx) = mpsc::channel();
    let worker = std::thread::spawn(move || {
        let result = discover(&memory, &DiscoverOptions::default());
        let _ = tx.send(result);
    });
    match rx.recv_timeout(WATCHDOG) {
        Ok(Ok(found)) => Outcome::Recovered(Box::new(found)),
        Ok(Err(error)) => Outcome::Failed(error),
        Err(mpsc::RecvTimeoutError::Timeout) => Outcome::Hung,
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            let _ = worker.join();
            Outcome::Panicked
        }
    }
}

fn damaged(profile: Profile, corruption: Corruption, blocks: usize) -> Fixture {
    FixtureBuilder::new(profile)
        .seed(9)
        .filler_objects(300)
        .name_blocks(blocks)
        .corrupt(corruption)
        .build()
}

/// What a corruption must lead to.
enum Expected {
    /// The true layout. When the keyword is not empty, a warning mentions it
    /// unless the corruption leaves no trace the solver can see.
    Recovers(&'static str),
    /// An error that names the field.
    MissingEvidence(&'static str),
    /// The object array is not found.
    NoObjectArray,
}

fn expected(corruption: Corruption) -> Expected {
    match corruption {
        Corruption::NullObjectSlots
        | Corruption::ClassChainCycle
        | Corruption::NonUtf8Names
        | Corruption::ObjectFreedAndReused => Expected::Recovers(""),
        Corruption::SuperChainCycle => Expected::Recovers("super chains"),
        Corruption::PropertyListCycle | Corruption::DanglingPointers => {
            Expected::Recovers("typed payloads")
        }
        Corruption::UnmappedNamesBlock => Expected::Recovers("name table"),
        Corruption::WrongElementSize | Corruption::OverlappingField => {
            Expected::Recovers("fits inside")
        }
        Corruption::HugeArrayCounts => Expected::MissingEvidence("uenum"),
        Corruption::NameIndexOutOfRange => Expected::MissingEvidence("ufunction.flags"),
        Corruption::ZeroedObjectHeaders => Expected::MissingEvidence("uobject.name"),
        Corruption::TruncatedChunkTable => Expected::NoObjectArray,
    }
}

fn check(label: &str, profile: &Profile, corruption: Corruption) {
    let fx = damaged(profile.clone(), corruption, 3);
    let outcome = guarded(&fx);
    let what = format!("{label} {corruption:?}");
    match (expected(corruption), outcome) {
        (_, Outcome::Panicked) => panic!("{what}: discovery panicked"),
        (_, Outcome::Hung) => panic!("{what}: discovery hung"),
        (Expected::Recovers(keyword), Outcome::Recovered(d)) => {
            assert_essential_equal(&fx, &d);
            assert!(
                keyword.is_empty() || d.warnings.iter().any(|w| w.contains(keyword)),
                "{what}: no warning mentions {keyword}: {:?}",
                d.warnings
            );
        }
        (
            Expected::MissingEvidence(field),
            Outcome::Failed(ScanError::MissingEvidence { field: got, .. }),
        ) => assert_eq!(got, field, "{what}"),
        (Expected::NoObjectArray, Outcome::Failed(ScanError::ObjectArrayNotFound { .. })) => {}
        (_, Outcome::Recovered(d)) => panic!("{what}: unexpectedly recovered: {:?}", d.warnings),
        (_, Outcome::Failed(error)) => panic!("{what}: unexpected error: {error}"),
    }
}

#[test]
fn every_corruption_on_ue_4_21() {
    for corruption in Corruption::ALL {
        check("ue4.21", &Profile::ue4_21_measured(), corruption);
    }
}

#[test]
fn every_corruption_on_ue_4_25_case_preserving() {
    for corruption in Corruption::ALL {
        check(
            "ue4.25-cp",
            &Profile::ue4_25_case_preserving_measured(),
            corruption,
        );
    }
}

#[test]
fn every_corruption_on_ue_5_7() {
    let profile = Profile::by_name("ue5.7").unwrap_or_else(|| panic!("ue5.7"));
    for corruption in Corruption::ALL {
        check("ue5.7", &profile, corruption);
    }
}

#[test]
fn a_single_unmapped_name_block_is_a_named_error() {
    for (label, profile) in [
        ("ue4.21", Profile::ue4_21_measured()),
        (
            "ue5.7",
            Profile::by_name("ue5.7").unwrap_or_else(|| panic!("ue5.7")),
        ),
    ] {
        let fx = damaged(profile, Corruption::UnmappedNamesBlock, 1);
        match guarded(&fx) {
            Outcome::Failed(ScanError::NameTableNotFound { .. }) => {}
            Outcome::Recovered(_) => panic!("{label}: recovered without its only name block"),
            Outcome::Failed(error) => panic!("{label}: unexpected error {error}"),
            Outcome::Panicked | Outcome::Hung => panic!("{label}: panic or hang"),
        }
    }
}
