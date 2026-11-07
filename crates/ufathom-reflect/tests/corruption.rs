//! Every corruption of the testkit: no panic, no hang, and the specific
//! diagnostics, on a UE 4.21 (array names, legacy properties), a UE 4.25
//! case-preserving (name pool, `FField`) and a UE 5.7 profile.
//!
//! The layout comes from the fixture, not from `discover`: three of the
//! corruptions damage the image so much that `discover` refuses it, and this
//! crate's job starts where the layout is known.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "test code may panic on failure"
)]

mod common;

use std::collections::BTreeSet;
use std::time::Duration;

use common::{assert_matches_truth, build_corrupt, truth_discovery, watchdog};
use ufathom_model::{Diagnostic, Severity, StructDef};
use ufathom_reflect::{ReflectError, ReflectOptions, ReflectOutput, build_dump};
use ufathom_testkit::{Corruption, Fixture, Profile};

const WATCHDOG: Duration = Duration::from_secs(120);

fn profiles() -> Vec<Profile> {
    vec![
        Profile::ue4_21_measured(),
        Profile::ue4_25_case_preserving_measured(),
        Profile::by_name("ue5.7").unwrap(),
    ]
}

/// Builds the damaged fixture and dumps it on a watched thread.
fn run(
    profile: &Profile,
    corruption: Corruption,
    fillers: usize,
    threshold: Option<f64>,
) -> (Fixture, Result<ReflectOutput, ReflectError>) {
    let profile = profile.clone();
    watchdog(
        &format!("{} {corruption:?}", profile.name),
        WATCHDOG,
        move || {
            let fx = build_corrupt(profile, 7, fillers, &[corruption]);
            let mut options = ReflectOptions::default();
            if let Some(threshold) = threshold {
                options.degraded_threshold = threshold;
            }
            let out = build_dump(&fx.memory, &truth_discovery(&fx), &options);
            (fx, out)
        },
    )
}

fn ok(profile: &Profile, corruption: Corruption) -> (Fixture, ReflectOutput) {
    let (fx, out) = run(profile, corruption, 600, None);
    let out = out.unwrap_or_else(|e| panic!("{} {corruption:?}: {e}", fx.profile.name));
    (fx, out)
}

fn errors(out: &ReflectOutput) -> Vec<&Diagnostic> {
    out.dump
        .diagnostics
        .iter()
        .filter(|d| d.severity == Severity::Error)
        .collect()
}

/// Whether a diagnostic of `severity` on `path` mentions `needle`.
fn has(out: &ReflectOutput, severity: Severity, path: &str, needle: &str) -> bool {
    out.dump
        .diagnostics
        .iter()
        .any(|d| d.severity == severity && d.path == path && d.message.contains(needle))
}

fn engine(name: &str) -> String {
    format!("/Script/Engine.{name}")
}

fn find<'a>(out: &'a ReflectOutput, path: &str) -> Option<&'a StructDef> {
    out.dump.structs.iter().find(|s| s.path == path)
}

/// Every struct of the dump equals the truth, except those in `except`.
fn assert_rest_equals_truth(fx: &Fixture, out: &ReflectOutput, except: &[String]) {
    let truth: Vec<&StructDef> = fx
        .truth
        .structs
        .iter()
        .filter(|s| !except.contains(&s.path))
        .collect();
    let got: Vec<&StructDef> = out
        .dump
        .structs
        .iter()
        .filter(|s| !except.contains(&s.path))
        .collect();
    assert_eq!(got.len(), truth.len(), "{}: struct count", fx.profile.name);
    for (got, want) in got.iter().zip(&truth) {
        assert_eq!(got, want, "{}: {}", fx.profile.name, want.path);
    }
}

#[test]
fn null_object_slots_are_counted_and_change_nothing() {
    for profile in profiles() {
        let (fx, out) = ok(&profile, Corruption::NullObjectSlots);
        let clean = common::dump(&common::build(profile.clone(), 7, 600));
        assert!(
            out.stats.null_slots > clean.stats.null_slots,
            "{}",
            profile.name
        );
        assert_eq!(out.stats.failures, 0);
        assert!(
            out.dump.diagnostics.is_empty(),
            "{:?}",
            out.dump.diagnostics
        );
        assert!(!out.degraded);
        assert_matches_truth(&fx, &out);
    }
}

#[test]
fn a_class_chain_cycle_names_both_classes_and_drops_only_them() {
    for profile in profiles() {
        let (fx, out) = ok(&profile, Corruption::ClassChainCycle);
        for name in ["Actor", "Pawn"] {
            let path = engine(name);
            assert!(
                has(&out, Severity::Error, &path, "does not derive from Class"),
                "{}: {name}: {:?}",
                profile.name,
                out.dump.diagnostics
            );
            assert!(
                find(&out, &path).is_none(),
                "{} {name} is not a class",
                profile.name
            );
        }
        assert_eq!(errors(&out).len(), 2, "{}", profile.name);
        assert_eq!(out.stats.failures, 2);
        assert!(!out.degraded);
        // What refers to a dropped class is reported as unresolved, not invented.
        assert!(has(
            &out,
            Severity::Warning,
            &engine("Controller"),
            "super struct"
        ));
        assert_eq!(out.dump.structs.len(), fx.truth.structs.len() - 2);
    }
}

#[test]
fn a_super_chain_cycle_is_reported_and_the_parent_dropped() {
    let cyclic = [
        engine("Actor"),
        engine("Pawn"),
        engine("Controller"),
        engine("PlayerController"),
    ];
    for profile in profiles() {
        let (fx, out) = ok(&profile, Corruption::SuperChainCycle);
        for path in &cyclic {
            assert!(
                has(&out, Severity::Error, path, "forms a cycle"),
                "{} {path}",
                profile.name
            );
            let def = find(&out, path).unwrap_or_else(|| panic!("{} {path} kept", profile.name));
            assert_eq!(def.super_path, None, "{} {path}", profile.name);
            assert!(has(&out, Severity::Error, path, "parent chain is unusable"));
        }
        assert_rest_equals_truth(&fx, &out, &cyclic);
        assert!(!out.degraded);
    }
}

#[test]
fn a_property_list_cycle_keeps_each_member_once() {
    for profile in profiles() {
        let (fx, out) = ok(&profile, Corruption::PropertyListCycle);
        let path = "/Script/CoreUObject.Guid";
        assert!(
            has(&out, Severity::Warning, path, "forms a cycle"),
            "{}",
            profile.name
        );
        assert!(errors(&out).is_empty());
        assert_eq!(out.stats.failures, 0);
        let guid = find(&out, path).unwrap();
        let want = fx.truth.structs.iter().find(|s| s.path == path).unwrap();
        assert_eq!(guid.fields, want.fields, "{}", profile.name);
        assert_matches_truth(&fx, &out);
    }
}

#[test]
fn huge_enum_counts_skip_the_enums_after_one_reread_each() {
    for profile in profiles() {
        let (fx, out) = ok(&profile, Corruption::HugeArrayCounts);
        assert!(out.dump.enums.is_empty(), "{}", profile.name);
        for def in &fx.truth.enums {
            assert!(
                has(&out, Severity::Error, &def.path, "exceeds its limit"),
                "{} {}",
                profile.name,
                def.path
            );
        }
        assert_eq!(out.stats.failures, fx.truth.enums.len() as u64);
        assert_eq!(out.stats.rereads, fx.truth.enums.len() as u64);
        assert!(!out.degraded);
        assert_eq!(out.dump.structs, fx.truth.structs);
    }
}

#[test]
fn dangling_pointers_are_per_object_errors() {
    for profile in profiles() {
        let (fx, out) = ok(&profile, Corruption::DanglingPointers);
        let actor = find(&out, &engine("Actor")).expect("Actor is kept with what could be read");
        assert!(actor.fields.is_empty(), "{}", profile.name);
        assert!(
            has(&out, Severity::Error, &engine("Actor"), "member list"),
            "{}: {:?}",
            profile.name,
            out.dump.diagnostics
        );
        // Pawn's parent is unmapped: it is kept, without a parent.
        assert_eq!(find(&out, &engine("Pawn")).unwrap().super_path, None);
        assert!(has(
            &out,
            Severity::Warning,
            &engine("Pawn"),
            "super struct"
        ));
        assert!(has(
            &out,
            Severity::Error,
            &engine("Pawn"),
            "cannot read SuperStruct"
        ));
        // Level's outer is unmapped, so its path cannot be built.
        assert!(find(&out, &engine("Level")).is_none());
        assert!(
            errors(&out)
                .iter()
                .any(|d| d.path.starts_with("<slot") && d.message.contains("path of the object")),
            "{}",
            profile.name
        );
        assert!(out.stats.failures >= 3 && out.stats.failures <= 5);
        assert!(!out.degraded);
        assert!(out.dump.structs.len() < fx.truth.structs.len());
    }
}

#[test]
fn an_unmapped_name_block_is_reported_and_the_rest_is_intact() {
    for profile in profiles() {
        let (fx, out) = ok(&profile, Corruption::UnmappedNamesBlock);
        assert!(
            has(&out, Severity::Warning, "", "unreadable"),
            "{}: {:?}",
            profile.name,
            out.dump.diagnostics
        );
        assert_eq!(out.stats.failures, 0);
        assert_matches_truth(&fx, &out);
    }
}

#[test]
fn unresolvable_names_fail_the_objects_that_need_them() {
    for profile in profiles() {
        let (_, out) = ok(&profile, Corruption::NameIndexOutOfRange);
        for name in ["Actor", "Pawn"] {
            assert!(
                find(&out, &engine(name)).is_none(),
                "{} {name}",
                profile.name
            );
        }
        assert!(
            out.dump
                .structs
                .iter()
                .all(|s| s.functions.iter().all(|f| f.name != "K2_DestroyActor")),
            "{}",
            profile.name
        );
        assert!(!errors(&out).is_empty());
        assert!(
            errors(&out)
                .iter()
                .all(|d| d.message.contains("does not resolve")),
            "{}: {:?}",
            profile.name,
            errors(&out)
        );
        // Actor, Pawn, K2_DestroyActor and everything they own.
        assert!(out.stats.failures >= 3, "{}", profile.name);
        assert!(!out.degraded);
    }
}

#[test]
fn undecodable_names_are_not_failures() {
    for profile in profiles() {
        let (fx, out) = ok(&profile, Corruption::NonUtf8Names);
        assert_eq!(out.stats.failures, 0, "{}", profile.name);
        assert!(errors(&out).is_empty());
        assert_matches_truth(&fx, &out);
    }
}

#[test]
fn a_truncated_chunk_table_is_refused_by_name() {
    for profile in profiles() {
        let (fx, out) = run(&profile, Corruption::TruncatedChunkTable, 600, None);
        match out {
            Err(ReflectError::ObjectArray { source }) => {
                let text = source.to_string();
                assert!(
                    text.contains("chunk count") || text.contains("object count"),
                    "{}: {text}",
                    fx.profile.name
                );
            }
            other => panic!(
                "{}: expected an object array error, got {other:?}",
                fx.profile.name
            ),
        }
    }
}

#[test]
fn zeroed_headers_are_reread_once_and_then_failed() {
    for profile in profiles() {
        let (fx, out) = ok(&profile, Corruption::ZeroedObjectHeaders);
        // Pawn and 25 fillers.
        assert!(
            out.stats.failures >= 26,
            "{}: {}",
            profile.name,
            out.stats.failures
        );
        assert!(out.stats.rereads >= 26, "{}", profile.name);
        assert!(
            errors(&out)
                .iter()
                .any(|d| d.message.contains("stores index 0")),
            "{}",
            profile.name
        );
        assert!(find(&out, &engine("Pawn")).is_none(), "{}", profile.name);
        // 27 of 800: under the 5% default.
        assert!(!out.degraded, "{}", profile.name);
        assert!(out.dump.structs.len() < fx.truth.structs.len());
    }
}

#[test]
fn zeroed_headers_degrade_a_small_image() {
    // The same damage on an image of a few dozen objects is above 5%.
    for profile in profiles() {
        let (_, out) = run(&profile, Corruption::ZeroedObjectHeaders, 40, None);
        let out = out.unwrap();
        assert!(out.degraded, "{}: {:?}", profile.name, out.stats);
        assert!(!out.dump.structs.is_empty());
    }
}

#[test]
fn wrong_element_sizes_are_warnings_and_the_member_is_kept_as_read() {
    for profile in profiles() {
        let (_, out) = ok(&profile, Corruption::WrongElementSize);
        let guid = "/Script/CoreUObject.Guid";
        assert!(has(
            &out,
            Severity::Warning,
            guid,
            "do not fit the owner size"
        ));
        assert!(has(
            &out,
            Severity::Warning,
            &engine("Actor"),
            "NetTag: element size 0"
        ));
        assert!(errors(&out).is_empty());
        let b = find(&out, guid)
            .unwrap()
            .fields
            .iter()
            .find(|f| f.name == "B")
            .unwrap();
        assert_eq!(b.element_size, 0x100, "{}", profile.name);
        assert!(!out.degraded);
    }
}

#[test]
fn overlapping_members_are_warnings() {
    for profile in profiles() {
        let (_, out) = ok(&profile, Corruption::OverlappingField);
        assert!(has(
            &out,
            Severity::Warning,
            "/Script/CoreUObject.Guid",
            "D at 0x4 overlaps"
        ));
        assert!(has(
            &out,
            Severity::Warning,
            "/Script/CoreUObject.Color",
            "overlaps"
        ));
        assert!(errors(&out).is_empty());
        assert!(!out.degraded);
    }
}

#[test]
fn a_freed_and_reused_object_is_reread_and_reported() {
    for profile in profiles() {
        let (fx, out) = ok(&profile, Corruption::ObjectFreedAndReused);
        assert_eq!(out.stats.failures, 1, "{}", profile.name);
        assert_eq!(out.stats.rereads, 1, "{}", profile.name);
        let problems = errors(&out);
        assert_eq!(problems.len(), 1);
        assert!(
            problems[0].path.starts_with("<slot"),
            "{}",
            problems[0].path
        );
        assert!(
            problems[0].message.contains("stores index"),
            "{}",
            problems[0].message
        );
        assert!(!out.degraded);
        // The reused object is an instance: every type is intact.
        assert_matches_truth(&fx, &out);
    }
}

#[test]
fn the_degraded_threshold_is_the_callers() {
    for profile in profiles() {
        let (_, strict) = run(&profile, Corruption::ObjectFreedAndReused, 600, Some(0.0));
        assert!(strict.unwrap().degraded, "{}", profile.name);
        let (_, lax) = run(&profile, Corruption::ZeroedObjectHeaders, 40, Some(1.0));
        assert!(!lax.unwrap().degraded, "{}", profile.name);
    }
}

#[test]
fn every_corruption_is_covered() {
    // A corruption added to the testkit must get a test here.
    let covered: BTreeSet<_> = [
        Corruption::NullObjectSlots,
        Corruption::ClassChainCycle,
        Corruption::SuperChainCycle,
        Corruption::PropertyListCycle,
        Corruption::HugeArrayCounts,
        Corruption::DanglingPointers,
        Corruption::UnmappedNamesBlock,
        Corruption::NameIndexOutOfRange,
        Corruption::NonUtf8Names,
        Corruption::TruncatedChunkTable,
        Corruption::ZeroedObjectHeaders,
        Corruption::WrongElementSize,
        Corruption::OverlappingField,
        Corruption::ObjectFreedAndReused,
    ]
    .into_iter()
    .collect();
    assert_eq!(covered, Corruption::ALL.into_iter().collect());
}
