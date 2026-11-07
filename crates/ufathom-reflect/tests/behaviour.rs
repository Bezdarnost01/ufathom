//! Cancellation, determinism, the usmap cross-check and re-validation of a
//! target that changes while it is read.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "test code may panic on failure"
)]

mod common;

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use common::{
    assert_matches_truth, build, build_corrupt, discovery, dump_on, truth_discovery, watchdog,
};
use rayon::ThreadPoolBuilder;
use ufathom_engine::{LoadOptions, NameTable};
use ufathom_mem::{Addr, MemError, MemoryReader, MemoryRegion, ModuleInfo, Snapshot};
use ufathom_model::layout::ObjectArrayKind;
use ufathom_model::{Dump, EngineInfo, Severity, StructDef, TargetInfo, ToolInfo};
use ufathom_reflect::{ReflectError, ReflectOptions, ReflectStats, build_dump};
use ufathom_testkit::{Corruption, Fixture, Profile};
use ufathom_usmap::{Usmap, compare, write_usmap};

fn profiles() -> Vec<Profile> {
    vec![
        Profile::ue4_21_measured(),
        Profile::ue4_25_case_preserving_measured(),
        Profile::by_name("ue5.7").unwrap(),
    ]
}

/// A reader that counts reads and sets a flag after a given number of them.
struct CancelAfter<'a> {
    inner: &'a Snapshot,
    reads: AtomicUsize,
    after: usize,
    flag: &'a AtomicBool,
}

impl MemoryReader for CancelAfter<'_> {
    fn read_exact(&self, addr: Addr, buf: &mut [u8]) -> Result<(), MemError> {
        if self.reads.fetch_add(1, Ordering::SeqCst) + 1 >= self.after {
            self.flag.store(true, Ordering::SeqCst);
        }
        self.inner.read_exact(addr, buf)
    }

    fn regions(&self) -> Result<Vec<MemoryRegion>, MemError> {
        self.inner.regions()
    }

    fn modules(&self) -> Result<Vec<ModuleInfo>, MemError> {
        self.inner.modules()
    }
}

#[test]
fn a_flag_set_before_the_call_stops_it_at_once() {
    let fx = build(Profile::ue4_25_case_preserving_measured(), 1, 300);
    let d = discovery(&fx);
    let flag = AtomicBool::new(true);
    let options = ReflectOptions {
        cancel: Some(&flag),
        ..ReflectOptions::default()
    };
    let result = build_dump(&fx.memory, &d, &options);
    assert!(matches!(result, Err(ReflectError::Cancelled)), "{result:?}");
}

#[test]
fn a_flag_set_during_the_walk_stops_it_promptly() {
    for profile in profiles() {
        let fx = build(profile, 2, 3000);
        let d = discovery(&fx);

        // How many reads an uncancelled dump takes.
        let never = AtomicBool::new(false);
        let counter = CancelAfter {
            inner: &fx.memory,
            reads: AtomicUsize::new(0),
            after: usize::MAX,
            flag: &never,
        };
        let full = build_dump(&counter, &d, &ReflectOptions::default()).unwrap();
        let total = counter.reads.load(Ordering::SeqCst);
        assert!(!full.degraded);

        // Loading the name table is one bulk call of the engine crate that
        // cannot be interrupted, so the flag is set after it.
        let names_counter = CancelAfter {
            inner: &fx.memory,
            reads: AtomicUsize::new(0),
            after: usize::MAX,
            flag: &never,
        };
        NameTable::load(
            &names_counter,
            &d.layout.names,
            &LoadOptions::for_layout(&d.layout, None),
        )
        .unwrap();
        let names = names_counter.reads.load(Ordering::SeqCst);
        let walk = total - names;

        for threads in [0usize, 2] {
            let flag = AtomicBool::new(false);
            let reader = CancelAfter {
                inner: &fx.memory,
                reads: AtomicUsize::new(0),
                after: names + walk / 3,
                flag: &flag,
            };
            let pool = (threads > 0).then(|| {
                ThreadPoolBuilder::new()
                    .num_threads(threads)
                    .build()
                    .unwrap()
            });
            let options = ReflectOptions {
                pool: pool.as_ref(),
                cancel: Some(&flag),
                ..ReflectOptions::default()
            };
            let result = build_dump(&reader, &d, &options);
            assert!(
                matches!(result, Err(ReflectError::Cancelled)),
                "{} threads {threads}: {result:?}",
                fx.profile.name
            );
            // Between objects: far fewer reads than the whole walk.
            let used = reader.reads.load(Ordering::SeqCst);
            assert!(
                used < names + walk * 2 / 3,
                "{} threads {threads}: {used} reads of {total} after cancelling at {}",
                fx.profile.name,
                names + walk / 3
            );
        }
    }
}

/// What must not depend on the pool: everything but timings and cache counters.
fn stable(stats: &ReflectStats) -> [u64; 8] {
    [
        stats.objects,
        stats.null_slots,
        stats.structs,
        stats.classes,
        stats.enums,
        stats.functions,
        stats.failures,
        stats.rereads,
    ]
}

#[test]
fn the_dump_is_byte_identical_whatever_the_pool() {
    let cases: Vec<Fixture> = profiles()
        .into_iter()
        .map(|profile| build(profile, 11, 2000))
        .chain([
            build_corrupt(
                Profile::ue4_25_case_preserving_measured(),
                12,
                1500,
                &[Corruption::ZeroedObjectHeaders],
            ),
            build_corrupt(
                Profile::ue4_21_measured(),
                13,
                1500,
                &[
                    Corruption::DanglingPointers,
                    Corruption::ObjectFreedAndReused,
                ],
            ),
        ])
        .collect();
    for fx in &cases {
        let d = truth_discovery(fx);
        let reference = build_dump(&fx.memory, &d, &ReflectOptions::default()).unwrap();
        let json = reference.dump.to_json().unwrap();
        // A second sequential run, then the pools.
        let again = build_dump(&fx.memory, &d, &ReflectOptions::default()).unwrap();
        assert_eq!(
            again.dump.to_json().unwrap(),
            json,
            "{} repeat",
            fx.profile.name
        );
        for threads in [1, 2, 4] {
            for _ in 0..2 {
                let out = dump_on(fx, &d, threads);
                assert_eq!(
                    out.dump.to_json().unwrap(),
                    json,
                    "{} on {threads} threads",
                    fx.profile.name
                );
                assert_eq!(out.dump, reference.dump);
                assert_eq!(stable(&out.stats), stable(&reference.stats));
                assert_eq!(out.degraded, reference.degraded);
            }
        }
    }
}

#[test]
fn the_dump_does_not_depend_on_who_solved_the_layout() {
    let fx = build(Profile::ue4_25_case_preserving_measured(), 14, 500);
    let solved = common::dump(&fx);
    let given = build_dump(
        &fx.memory,
        &truth_discovery(&fx),
        &ReflectOptions::default(),
    )
    .unwrap();
    assert_eq!(solved.dump.structs, given.dump.structs);
    assert_eq!(solved.dump.enums, given.dump.enums);
}

fn truth_dump(fx: &Fixture) -> Dump {
    let mut dump = Dump::new(
        ToolInfo {
            name: "truth".to_owned(),
            version: "0".to_owned(),
        },
        TargetInfo {
            module: fx.module_name().to_owned(),
            build_id: String::new(),
        },
        EngineInfo::default(),
        fx.layout.clone(),
    );
    dump.structs = fx.truth.structs.clone();
    dump.enums = fx.truth.enums.clone();
    dump
}

#[test]
fn the_usmap_of_the_truth_agrees_with_the_dump() {
    let mut fixtures = vec![
        build(Profile::ue4_21_measured(), 5, 300),
        build(Profile::ue4_22_case_preserving_measured(), 5, 300),
        build(Profile::ue4_25_case_preserving_measured(), 5, 300),
        build(Profile::ue5_case_preserving_assumed(), 5, 300),
    ];
    fixtures.extend((0..5).map(|seed| build(Profile::shuffled(seed), seed, 200)));
    for fx in &fixtures {
        let out = common::dump(fx);
        let usmap = Usmap::parse(&write_usmap(&truth_dump(fx))).unwrap();
        let report = compare(&out.dump, &usmap);
        assert!(report.is_clean(), "{}: {report}", fx.profile.name);
    }
}

/// Serves one 8-byte read at `at` as `replacement` for the first `remaining`
/// such reads, as a slot whose object was freed and reused would.
struct SlotChanges<'a> {
    inner: &'a Snapshot,
    at: Addr,
    replacement: u64,
    remaining: AtomicUsize,
}

/// Decrements `counter` if it is above zero. A plain compare-exchange loop,
/// because the closure-based atomic update helper is renamed in newer Rust.
fn take_one(counter: &AtomicUsize) -> bool {
    let mut current = counter.load(Ordering::SeqCst);
    while let Some(next) = current.checked_sub(1) {
        match counter.compare_exchange(current, next, Ordering::SeqCst, Ordering::SeqCst) {
            Ok(_) => return true,
            Err(actual) => current = actual,
        }
    }
    false
}

impl MemoryReader for SlotChanges<'_> {
    fn read_exact(&self, addr: Addr, buf: &mut [u8]) -> Result<(), MemError> {
        if addr == self.at && buf.len() == 8 && take_one(&self.remaining) {
            buf.copy_from_slice(&self.replacement.to_le_bytes());
            return Ok(());
        }
        self.inner.read_exact(addr, buf)
    }

    fn regions(&self) -> Result<Vec<MemoryRegion>, MemError> {
        self.inner.regions()
    }

    fn modules(&self) -> Result<Vec<ModuleInfo>, MemError> {
        self.inner.modules()
    }
}

/// Address of the object pointer of the array item of `path`.
fn item_pointer(fx: &Fixture, path: &str) -> Addr {
    let index = u64::from(fx.object(path).unwrap().index);
    let objects = &fx.layout.objects;
    let items = &fx.anchors.data.items;
    let size = u64::from(objects.item_size);
    let item = match objects.kind {
        ObjectArrayKind::Fixed => items.table + index * size,
        ObjectArrayKind::Chunked { .. } => {
            let per = u64::from(items.per_chunk);
            items.chunks[usize::try_from(index / per).unwrap()] + (index % per) * size
        }
    };
    item + u64::from(objects.item_object)
}

#[test]
fn a_type_that_changes_once_is_reread_and_kept() {
    let path = "/Script/Engine.Pawn";
    for profile in profiles() {
        let fx = build(profile, 21, 400);
        let reader = SlotChanges {
            inner: &fx.memory,
            at: item_pointer(&fx, path),
            replacement: 0x1234_5000,
            remaining: AtomicUsize::new(1),
        };
        let out = build_dump(&reader, &truth_discovery(&fx), &ReflectOptions::default()).unwrap();
        assert_eq!(out.stats.rereads, 1, "{}", fx.profile.name);
        assert_eq!(out.stats.failures, 0, "{}", fx.profile.name);
        assert!(
            out.dump.diagnostics.is_empty(),
            "{:?}",
            out.dump.diagnostics
        );
        assert_matches_truth(&fx, &out);
    }
}

#[test]
fn a_type_whose_slot_keeps_changing_is_skipped_with_a_diagnostic() {
    let path = "/Script/Engine.Pawn";
    for profile in profiles() {
        let fx = build(profile, 22, 400);
        let reader = SlotChanges {
            inner: &fx.memory,
            at: item_pointer(&fx, path),
            replacement: 0x1234_5000,
            remaining: AtomicUsize::new(usize::MAX),
        };
        let out = build_dump(&reader, &truth_discovery(&fx), &ReflectOptions::default()).unwrap();
        assert_eq!(out.stats.rereads, 1, "{}", fx.profile.name);
        assert_eq!(out.stats.failures, 1, "{}", fx.profile.name);
        let problems: Vec<_> = out
            .dump
            .diagnostics
            .iter()
            .filter(|d| d.severity == Severity::Error)
            .collect();
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert_eq!(problems[0].path, path);
        assert!(problems[0].message.contains("changed while it was read"));
        let rest: Vec<&StructDef> = fx.truth.structs.iter().filter(|s| s.path != path).collect();
        assert_eq!(out.dump.structs.iter().collect::<Vec<_>>(), rest);
        assert!(!out.degraded);
    }
}

#[test]
fn a_walk_over_a_hostile_reader_ends() {
    // Every read fails: the name table cannot even be loaded.
    struct Dead;
    impl MemoryReader for Dead {
        fn read_exact(&self, addr: Addr, _buf: &mut [u8]) -> Result<(), MemError> {
            Err(MemError::Unreadable { addr, len: 1 })
        }
        fn regions(&self) -> Result<Vec<MemoryRegion>, MemError> {
            Ok(Vec::new())
        }
        fn modules(&self) -> Result<Vec<ModuleInfo>, MemError> {
            Ok(Vec::new())
        }
    }
    let fx = build(Profile::ue4_25_case_preserving_measured(), 3, 100);
    let d = discovery(&fx);
    let result = watchdog("dead reader", Duration::from_secs(60), move || {
        build_dump(&Dead, &d, &ReflectOptions::default()).map(|_| ())
    });
    assert!(
        matches!(
            result,
            Err(ReflectError::Names { .. } | ReflectError::ObjectArray { .. })
        ),
        "{result:?}"
    );
}
