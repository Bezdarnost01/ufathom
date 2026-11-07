//! Every `Corruption` on four profiles: the complete walk finishes, never
//! panics or hangs, and the damage shows up exactly where the reader is
//! documented to report it.
//!
//! The profiles cover a UE 4.21 build (legacy properties, array names), a
//! UE 4.25 case-preserving build (modern properties, pool names), and UE 5.7
//! with and without case-preserving names (split enum arrays, tagged
//! pointers).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "test code may panic on failure; clippy.toml only exempts #[test] functions, not helpers"
)]

mod common;

use std::collections::BTreeSet;
use std::sync::Arc;

use common::{Walk, assert_matches_truth, decoded_fields, walk, with_watchdog};
use ufathom_engine::{EngineError, NameTable, for_layout, validate_members};
use ufathom_model::{EngineVersion, FieldDef, PropertyType};
use ufathom_testkit::{BuildOptions, Corruption, Fixture, FixtureBuilder, Profile};

const WATCHDOG_SECONDS: u64 = 120;

fn profiles() -> Vec<Profile> {
    let ue5 = |case_preserving| {
        Profile::with_options(
            EngineVersion {
                major: 5,
                minor: 7,
                patch: 0,
            },
            BuildOptions {
                case_preserving_names: case_preserving,
                pool_entry: None,
            },
        )
        .unwrap()
    };
    vec![
        Profile::ue4_21_measured(),
        Profile::ue4_25_case_preserving_measured(),
        ue5(false),
        ue5(true),
    ]
}

fn builder(profile: &Profile) -> FixtureBuilder {
    FixtureBuilder::new(profile.clone())
        .seed(6)
        .filler_objects(200)
        .name_blocks(2)
}

fn run(label: &str, fx: &Arc<Fixture>) -> Walk {
    let fx = Arc::clone(fx);
    with_watchdog(label, WATCHDOG_SECONDS, move || walk(&fx, None))
}

fn index_of(fx: &Fixture, path: &str) -> u32 {
    fx.object(path).unwrap().index
}

fn path_of_index(fx: &Fixture, index: u32) -> &str {
    &fx.objects.iter().find(|r| r.index == index).unwrap().path
}

/// Members must match the baseline except that a type may degrade to
/// `Unknown` when a path it refers to can no longer be built. Returns how
/// many degraded.
fn assert_degraded_only(mine: &[FieldDef], base: &[FieldDef], label: &str) -> usize {
    assert_eq!(mine.len(), base.len(), "{label}: member count");
    let mut degraded = 0;
    for (got, want) in mine.iter().zip(base) {
        assert_eq!(
            (
                &got.name,
                got.offset,
                got.element_size,
                got.array_dim,
                got.flags
            ),
            (
                &want.name,
                want.offset,
                want.element_size,
                want.array_dim,
                want.flags
            ),
            "{label}"
        );
        if got.ty != want.ty {
            assert!(
                matches!(got.ty, PropertyType::Unknown { .. }),
                "{label}: {} became {:?}, was {:?}",
                want.name,
                got.ty,
                want.ty
            );
            degraded += 1;
        }
    }
    degraded
}

fn by_addr(walk: &common::StructWalk) -> Vec<&common::FunctionWalk> {
    let mut all: Vec<_> = walk.functions.iter().collect();
    all.sort_by_key(|f| f.addr);
    all
}

/// Everything not in `skip` reads as it does in the clean image. Returns the
/// number of members whose type degraded to `Unknown`.
fn assert_same_as_base(base: &Walk, w: &Walk, skip: &BTreeSet<String>, label: &str) -> usize {
    let mut degraded = 0;
    for (path, clean) in &base.structs {
        if skip.contains(path) {
            continue;
        }
        let here = &w.structs[path];
        let label = format!("{label}: {path}");
        assert_eq!(here.view.as_ref().ok(), clean.view.as_ref().ok(), "{label}");
        assert!(
            here.fields.errors.is_empty(),
            "{label}: {:?}",
            here.fields.errors
        );
        assert!(
            here.function_errors.is_empty(),
            "{label}: {:?}",
            here.function_errors
        );
        degraded += assert_degraded_only(
            &decoded_fields(&here.fields),
            &decoded_fields(&clean.fields),
            &label,
        );
        let (mine, theirs) = (by_addr(here), by_addr(clean));
        assert_eq!(mine.len(), theirs.len(), "{label}: functions");
        for (a, b) in mine.into_iter().zip(theirs) {
            assert_eq!(a.addr, b.addr, "{label}");
            assert_eq!(a.view.as_ref().ok(), b.view.as_ref().ok(), "{label}");
            assert_eq!(a.body.as_ref().ok(), b.body.as_ref().ok(), "{label}");
            degraded += assert_degraded_only(
                &decoded_fields(&a.params),
                &decoded_fields(&b.params),
                &label,
            );
        }
    }
    degraded
}

fn assert_enums_same(base: &Walk, w: &Walk, label: &str) {
    for (path, clean) in &base.enums {
        assert_eq!(
            w.enums[path].as_ref().ok(),
            clean.as_ref().ok(),
            "{label}: {path}"
        );
    }
}

fn nothing() -> BTreeSet<String> {
    BTreeSet::new()
}

fn skip(paths: &[&str]) -> BTreeSet<String> {
    paths.iter().map(|p| (*p).to_owned()).collect()
}

/// Structs whose truth super chain passes through one of `through`.
fn descendants(fx: &Fixture, through: &[&str]) -> BTreeSet<String> {
    let parent = |path: &str| {
        fx.truth
            .structs
            .iter()
            .find(|s| s.path == path)
            .and_then(|s| s.super_path.clone())
    };
    let mut out = BTreeSet::new();
    for def in &fx.truth.structs {
        let mut at = Some(def.path.clone());
        while let Some(path) = at {
            if through.contains(&path.as_str()) {
                out.insert(def.path.clone());
                break;
            }
            at = parent(&path);
        }
    }
    out
}

const ACTOR: &str = "/Script/Engine.Actor";
const PAWN: &str = "/Script/Engine.Pawn";
const LEVEL: &str = "/Script/Engine.Level";
const GUID: &str = "/Script/CoreUObject.Guid";
const COLOR: &str = "/Script/CoreUObject.Color";

#[test]
fn every_corruption_completes_and_surfaces_where_documented() {
    for profile in profiles() {
        let clean = Arc::new(builder(&profile).build());
        let base = run(&format!("{} clean", profile.name), &clean);
        assert_matches_truth(&clean, &base, &format!("{} clean", profile.name));
        for corruption in Corruption::ALL {
            let label = format!("{} {corruption:?}", profile.name);
            let fx = Arc::new(builder(&profile).corrupt(corruption).build());
            let w = run(&label, &fx);
            check(corruption, &fx, &base, &w, &label);
        }
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "one arm per corruption, kept together so none is missed"
)]
fn check(corruption: Corruption, fx: &Fixture, base: &Walk, w: &Walk, label: &str) {
    let modern = fx.profile.is_modern();
    match corruption {
        // Null slots are skipped and counted; every other object is intact.
        Corruption::NullObjectSlots => {
            assert!(w.array.is_ok() && w.chunk_errors.is_empty(), "{label}");
            assert_eq!(w.num, base.num, "{label}");
            let missing: Vec<u32> = base
                .slots
                .iter()
                .filter(|slot| !w.slots.iter().any(|s| s.index == slot.index))
                .map(|slot| slot.index)
                .collect();
            assert!(!missing.is_empty(), "{label}");
            assert_eq!(
                w.null_slots - base.null_slots,
                missing.len() as u32,
                "{label}: nulls counted"
            );
            assert!(
                missing.contains(&index_of(fx, "/Script/Engine.Default__Pawn")),
                "{label}"
            );
            for slot in &w.slots {
                assert!(base.slots.contains(slot), "{label}: slot {}", slot.index);
            }
            assert!(
                w.header_errors.is_empty() && w.rejected.is_empty(),
                "{label}"
            );
            for (index, path) in &w.paths {
                assert_eq!(path, &base.paths[index], "{label}");
            }
            assert_eq!(assert_same_as_base(base, w, &nothing(), label), 0);
            assert_enums_same(base, w, label);
        }
        // This crate never follows the class-of-class chain (only SuperStruct
        // chains are walked), so the corruption changes two header values and
        // nothing else. The point is that nothing loops.
        Corruption::ClassChainCycle => {
            let (actor, pawn) = (fx.object(ACTOR).unwrap(), fx.object(PAWN).unwrap());
            assert_eq!(w.headers[&actor.index].class, pawn.address, "{label}");
            assert_eq!(w.headers[&pawn.index].class, actor.address, "{label}");
            assert!(
                w.path_errors.is_empty() && w.header_errors.is_empty(),
                "{label}"
            );
            assert_eq!(w.paths, base.paths, "{label}");
            assert_eq!(assert_same_as_base(base, w, &nothing(), label), 0);
            assert_enums_same(base, w, label);
        }
        // The super walk reports Cycle for exactly the structs whose chain
        // passes through Actor or Pawn, after yielding each struct once.
        Corruption::SuperChainCycle => {
            let looping = descendants(fx, &[ACTOR, PAWN]);
            for (path, found) in &w.structs {
                let last = found.super_chain.last().unwrap();
                if looping.contains(path) {
                    assert!(
                        matches!(last, Err(e) if matches!(e.root(), EngineError::Cycle { .. })),
                        "{label}: {path}: {last:?}"
                    );
                    let ok: Vec<_> = found
                        .super_chain
                        .iter()
                        .filter_map(|i| i.as_ref().ok())
                        .collect();
                    let unique: BTreeSet<_> = ok.iter().collect();
                    assert_eq!(ok.len(), unique.len(), "{label}: {path} repeats a struct");
                } else {
                    assert!(
                        found.super_chain.iter().all(Result::is_ok),
                        "{label}: {path}"
                    );
                }
            }
            let actor = &w.structs[ACTOR];
            assert_eq!(
                actor.view.as_ref().unwrap().super_struct,
                fx.object(PAWN).unwrap().address
            );
            assert_eq!(assert_same_as_base(base, w, &skip(&[ACTOR]), label), 0);
        }
        // The cycle is reported once; the four members are kept exactly once.
        Corruption::PropertyListCycle => {
            let guid = &w.structs[GUID];
            let mut names: Vec<&str> = guid.fields.raw.iter().map(|r| r.name.as_str()).collect();
            names.sort_unstable();
            assert_eq!(names, ["A", "B", "C", "D"], "{label}");
            assert_eq!(guid.fields.errors.len(), 1, "{label}");
            assert!(
                matches!(guid.fields.errors[0].root(), EngineError::Cycle { .. }),
                "{label}"
            );
            // Functions share the `Children` list in the legacy system.
            let function_cycles = guid
                .function_errors
                .iter()
                .filter(|e| matches!(e.root(), EngineError::Cycle { .. }))
                .count();
            assert_eq!(function_cycles, usize::from(!modern), "{label}");
            assert_eq!(assert_same_as_base(base, w, &skip(&[GUID]), label), 0);
            assert_enums_same(base, w, label);
        }
        // Every enum count is refused before anything is allocated.
        Corruption::HugeArrayCounts => {
            assert!(!w.enums.is_empty(), "{label}");
            for (path, entries) in &w.enums {
                let error = entries.as_ref().unwrap_err();
                assert!(
                    matches!(error.root(), EngineError::Limit { .. }),
                    "{label}: {path}: {error}"
                );
            }
            assert_eq!(assert_same_as_base(base, w, &nothing(), label), 0);
        }
        // Actor's member list, Pawn's super and Level's outer are reported;
        // nothing else changes.
        Corruption::DanglingPointers => {
            let actor = &w.structs[ACTOR];
            assert!(actor.fields.raw.is_empty(), "{label}");
            assert!(
                matches!(actor.fields.errors[0].root(), EngineError::Read { .. }),
                "{label}"
            );
            // The legacy system keeps functions in the same list.
            assert_eq!(!actor.function_errors.is_empty(), !modern, "{label}");
            let through_pawn = descendants(fx, &[PAWN]);
            for (path, found) in &w.structs {
                let last = found.super_chain.last().unwrap();
                if through_pawn.contains(path) {
                    assert!(
                        matches!(last, Err(e) if matches!(e.root(), EngineError::Read { .. })),
                        "{label}: {path}: {last:?}"
                    );
                } else {
                    assert!(
                        found.super_chain.iter().all(Result::is_ok),
                        "{label}: {path}"
                    );
                }
            }
            assert!(!w.path_errors.is_empty(), "{label}");
            for (index, error) in &w.path_errors {
                let path = path_of_index(fx, *index);
                assert!(path.starts_with(LEVEL), "{label}: {path} should not fail");
                assert!(
                    matches!(error.root(), EngineError::Read { .. }),
                    "{label}: {error}"
                );
            }
            assert!(
                w.path_errors.iter().any(|(i, _)| *i == index_of(fx, LEVEL)),
                "{label}"
            );
            if !modern {
                // The legacy property whose class pointer was damaged.
                let bad = fx.object("/Script/Engine.Actor:bHidden").unwrap().address;
                let names = w.names.as_ref().unwrap();
                let system = for_layout(&fx.memory, &fx.layout, names);
                let error = system.decode(bad).unwrap_err();
                assert!(
                    matches!(error.root(), EngineError::Read { .. }),
                    "{label}: {error}"
                );
            }
            let skipped = skip(&[ACTOR, PAWN]);
            assert_same_as_base(base, w, &skipped, label);
            assert_enums_same(base, w, label);
        }
        // Names in the unreadable block resolve to `None` and are reported;
        // all other names survive. A single block would be an error instead.
        Corruption::UnmappedNamesBlock => {
            let names = w.names.as_ref().unwrap();
            let clean = base.names.as_ref().unwrap();
            let stats = names.stats();
            assert_eq!(stats.unreadable_blocks.len(), 1, "{label}: {stats:?}");
            assert_eq!(
                stats.unreadable_blocks[0].block + 1,
                stats.blocks,
                "{label}"
            );
            let lost: Vec<(u32, &str)> = clean
                .iter()
                .filter(|(index, _)| names.resolve_index(*index).is_none())
                .collect();
            assert!(!lost.is_empty(), "{label}");
            assert!(
                lost.iter().all(|(_, text)| text.starts_with("PadName")),
                "{label}"
            );
            assert_eq!(names.len() + lost.len(), clean.len(), "{label}");
            assert!(
                names.iter().all(|(i, t)| clean.resolve_index(i) == Some(t)),
                "{label}"
            );
            assert_eq!(w.paths, base.paths, "{label}");
            assert_eq!(assert_same_as_base(base, w, &nothing(), label), 0);
            assert_enums_same(base, w, label);
        }
        // Four objects get unresolvable names. Header reads still work, the
        // name does not resolve, and exactly the paths through those objects
        // fail.
        Corruption::NameIndexOutOfRange => {
            let names = w.names.as_ref().unwrap();
            let filler = fx
                .objects
                .iter()
                .find(|r| r.path.starts_with("/Game/Maps/Main.Main:PersistentLevel."))
                .unwrap();
            let victims = [
                ACTOR.to_owned(),
                PAWN.to_owned(),
                "/Script/Engine.Actor:K2_DestroyActor".to_owned(),
                filler.path.clone(),
            ];
            for victim in &victims {
                let index = index_of(fx, victim);
                let header = &w.headers[&index];
                let error = names
                    .resolve(&fx.layout.fname, header.name.as_slice())
                    .unwrap_err();
                assert!(
                    matches!(error, EngineError::UnresolvedName { .. }),
                    "{label}: {victim}"
                );
                let (_, error) = w.path_errors.iter().find(|(i, _)| *i == index).unwrap();
                assert!(
                    matches!(error.root(), EngineError::UnresolvedName { .. }),
                    "{label}"
                );
            }
            for (index, _) in &w.path_errors {
                let path = path_of_index(fx, *index);
                assert!(
                    victims.iter().any(|v| path == v
                        || path.starts_with(&format!("{v}:"))
                        || path.starts_with(&format!("{v}."))),
                    "{label}: {path} should resolve"
                );
            }
            assert!(w.header_errors.is_empty(), "{label}");
            // Types that refer to a victim degrade to Unknown, nothing else.
            let degraded = assert_same_as_base(base, w, &nothing(), label);
            assert!(
                degraded > 0,
                "{label}: references to Actor or Pawn should degrade"
            );
            assert_enums_same(base, w, label);
        }
        // One narrow name decodes as Latin-1, one wide name lossily.
        Corruption::NonUtf8Names => {
            let names = w.names.as_ref().unwrap();
            let latin = "Caf\u{e9}Spawn";
            let lossy = "Gr\u{fffd}nerEingang";
            assert!(names.iter().any(|(_, t)| t == latin), "{label}");
            assert!(names.iter().any(|(_, t)| t == lossy), "{label}");
            assert!(
                !names
                    .iter()
                    .any(|(_, t)| t == "CafeSpawn" || t == "Gr\u{fc}nerEingang"),
                "{label}"
            );
            for (index, expected) in [("CafeSpawn", latin), ("Gr\u{fc}nerEingang", lossy)] {
                let record = fx
                    .objects
                    .iter()
                    .find(|r| r.path.ends_with(&format!(".{index}")))
                    .unwrap();
                assert_eq!(
                    w.paths[&record.index],
                    record.path.replace(index, expected),
                    "{label}"
                );
            }
            assert!(w.path_errors.is_empty(), "{label}");
            assert_eq!(assert_same_as_base(base, w, &nothing(), label), 0);
        }
        // A chunk count above the chunk capacity (or an element count above
        // the allocation) is refused at construction: no slot is read.
        Corruption::TruncatedChunkTable => {
            let error = w.array.as_ref().unwrap_err();
            assert!(
                matches!(error.root(), EngineError::Limit { .. }),
                "{label}: {error}"
            );
            assert!(w.slots.is_empty() && w.chunk_errors.is_empty(), "{label}");
            assert!(w.names.is_ok(), "{label}");
            assert_eq!(assert_same_as_base(base, w, &nothing(), label), 0);
        }
        // Zeroed headers read as zeros: the slot check rejects them, and a
        // path through one fails on its null class instead of becoming the
        // name `None`.
        Corruption::ZeroedObjectHeaders => {
            assert!(
                w.rejected.len() >= 20,
                "{label}: {} rejected",
                w.rejected.len()
            );
            let pawn = index_of(fx, PAWN);
            assert!(w.rejected.contains(&pawn), "{label}");
            let names = w.names.as_ref().unwrap();
            for index in &w.rejected {
                let header = &w.headers[index];
                assert_eq!(
                    (header.class, header.outer, header.flags),
                    (0, 0, 0),
                    "{label}"
                );
                assert_eq!(
                    names
                        .resolve(&fx.layout.fname, header.name.as_slice())
                        .unwrap(),
                    "None",
                    "{label}: a zeroed name is the name None, only the slot check shows it"
                );
                let (_, error) = w.path_errors.iter().find(|(i, _)| i == index).unwrap();
                assert!(
                    matches!(error.root(), EngineError::Null { .. }),
                    "{label}: {error}"
                );
            }
            for (index, path) in &w.paths {
                assert_eq!(
                    path, &base.paths[index],
                    "{label}: no wrong path is produced"
                );
            }
            assert!(w.path_errors.len() * 2 < w.slots.len(), "{label}");
            assert!(w.header_errors.is_empty(), "{label}");
            // The struct bodies are not part of the header, so they still decode.
            assert_same_as_base(base, w, &nothing(), label);
            assert_enums_same(base, w, label);
        }
        // The member keeps its raw size, which validate_members reports; no
        // layout is derived from it.
        Corruption::WrongElementSize => {
            for (owner, member, size) in [(GUID, "B", 0x100), (ACTOR, "NetTag", 0)] {
                let found = &w.structs[owner];
                let raw = found.fields.raw.iter().find(|r| r.name == member).unwrap();
                assert_eq!(raw.element_size, size, "{label}");
                let problems = validate_members(
                    found.view.as_ref().unwrap().properties_size,
                    &found.fields.raw,
                );
                assert!(
                    problems.iter().any(|p| p.addr() == Some(raw.address)),
                    "{label}: {owner}.{member} not reported: {problems:?}"
                );
            }
            assert_no_other_problems(w, &[(GUID, 1), (ACTOR, 1)], label);
            assert_eq!(
                assert_same_as_base(base, w, &skip(&[GUID, ACTOR]), label),
                0
            );
        }
        // Overlapping members are reported against the member that overlaps.
        Corruption::OverlappingField => {
            for (owner, member) in [(GUID, "D"), (COLOR, "G")] {
                let found = &w.structs[owner];
                let raw = found.fields.raw.iter().find(|r| r.name == member).unwrap();
                let problems = validate_members(
                    found.view.as_ref().unwrap().properties_size,
                    &found.fields.raw,
                );
                assert!(
                    problems
                        .iter()
                        .any(|p| p.addr().is_some() && p.to_string().contains("overlaps")),
                    "{label}: {owner}.{member}: {problems:?}"
                );
                let _ = raw;
            }
            assert_no_other_problems(w, &[(GUID, 1), (COLOR, 1)], label);
            assert_eq!(
                assert_same_as_base(base, w, &skip(&[GUID, COLOR]), label),
                0
            );
        }
        // The victim slot stores another object's index: the slot check finds
        // exactly it, and its serial changed. Without the check its path
        // would be the donor's.
        Corruption::ObjectFreedAndReused => {
            assert_eq!(w.rejected.len(), 1, "{label}");
            let victim = w.rejected[0];
            let donor = w.headers[&victim].index;
            assert_ne!(donor, victim, "{label}");
            let changed: Vec<u32> = w
                .slots
                .iter()
                .zip(&base.slots)
                .filter(|(a, b)| a.serial != b.serial)
                .map(|(a, _)| a.index)
                .collect();
            assert_eq!(changed, [victim], "{label}: serial");
            assert_eq!(w.paths[&victim], base.paths[&donor], "{label}");
            assert!(w.path_errors.is_empty(), "{label}");
            assert_eq!(assert_same_as_base(base, w, &nothing(), label), 0);
        }
    }
}

/// Every struct other than the named ones passes `validate_members`, and
/// each named one has at least the stated number of problems.
fn assert_no_other_problems(w: &Walk, expected: &[(&str, usize)], label: &str) {
    for (path, found) in &w.structs {
        let problems = validate_members(
            found.view.as_ref().unwrap().properties_size,
            &found.fields.raw,
        );
        match expected.iter().find(|(owner, _)| owner == path) {
            Some((_, at_least)) => assert!(problems.len() >= *at_least, "{label}: {path}"),
            None => assert!(problems.is_empty(), "{label}: {path}: {problems:?}"),
        }
    }
}

#[test]
fn a_single_unreadable_name_block_is_an_error_not_an_empty_table() {
    for profile in profiles() {
        let label = format!("{} single block", profile.name);
        let fx = Arc::new(
            FixtureBuilder::new(profile.clone())
                .seed(6)
                .filler_objects(100)
                .corrupt(Corruption::UnmappedNamesBlock)
                .build(),
        );
        let w = run(&label, &fx);
        let error = w.names.as_ref().map(NameTable::len).unwrap_err();
        assert!(
            matches!(error.root(), EngineError::Read { .. }),
            "{label}: {error}"
        );
        // The rest of the walk still finishes: every name is just unresolved.
        assert!(w.array.is_ok(), "{label}");
        assert!(w.path_errors.len() == w.slots.len(), "{label}");
        assert!(
            w.path_errors
                .iter()
                .all(|(_, e)| matches!(e.root(), EngineError::UnresolvedName { .. }))
        );
    }
}

#[test]
fn several_corruptions_at_once_still_finish() {
    for profile in profiles() {
        let label = format!("{} all at once", profile.name);
        let mut builder = builder(&profile);
        for corruption in Corruption::ALL {
            builder = builder.corrupt(corruption);
        }
        let fx = Arc::new(builder.build());
        let w = run(&label, &fx);
        // Nothing to assert about content beyond finishing: the object array
        // is refused, names may be partial, and every error is an error item.
        assert!(w.array.is_err(), "{label}");
    }
}
