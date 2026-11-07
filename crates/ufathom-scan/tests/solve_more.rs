//! Further behaviour of `discover`: mangled names, member orders, overrides,
//! the pointer transform, hints, a thread pool, single-block name tables,
//! version facts and the read budget.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::print_stderr,
    reason = "test code may panic on failure"
)]

mod solve_common;

use std::sync::atomic::{AtomicU64, Ordering};

use solve_common::{absolute, assert_equal_to_truth, build, peek_u64, poke, rebuild, run};
use ufathom_mem::{MemError, MemoryReader, MemoryRegion, ModuleInfo, Snapshot};
use ufathom_model::EngineVersion;
use ufathom_model::layout::{NameTableLayout, TransformOp};
use ufathom_scan::{DiscoverOptions, LayoutOverrides, ScanError, discover};
use ufathom_testkit::{ChainOrder, FixtureBuilder, Profile};

fn named(name: &str) -> Profile {
    Profile::by_name(name).unwrap_or_else(|| panic!("{name}"))
}

#[test]
fn mangled_display_names_and_every_member_order() {
    let profiles = [
        Profile::ue4_21_measured(),
        Profile::ue4_22_case_preserving_measured(),
        Profile::ue4_25_case_preserving_measured(),
        named("ue5.7-cp"),
        Profile::shuffled(3),
        Profile::shuffled(11),
    ];
    for profile in profiles {
        for order in [
            ChainOrder::Declaration,
            ChainOrder::Reversed,
            ChainOrder::FunctionsFirst,
        ] {
            let fx = FixtureBuilder::new(profile.clone())
                .seed(4)
                .filler_objects(300)
                .name_blocks(3)
                .chain_order(order)
                .mangled_display_names(80)
                .build();
            let d = run(&fx);
            assert_equal_to_truth(&fx, &d);
        }
    }
}

#[test]
fn a_correct_override_passes_and_a_wrong_one_is_a_named_contradiction() {
    let fx = build(Profile::ue4_25_case_preserving_measured(), 1, 300);
    let truth = &fx.layout;
    let right = LayoutOverrides {
        uobject_class: Some(truth.uobject.class),
        ustruct_super_struct: Some(truth.ustruct.super_struct),
        properties_element_size: Some(truth.properties.element_size),
        ..LayoutOverrides::default()
    };
    let options = DiscoverOptions {
        overrides: right,
        ..DiscoverOptions::default()
    };
    let d = discover(&fx.memory, &options).unwrap();
    assert_equal_to_truth(&fx, &d);

    for (field, wrong, apply) in [
        (
            "uobject.class",
            truth.uobject.class + 8,
            (|o: &mut LayoutOverrides, v| o.uobject_class = Some(v))
                as fn(&mut LayoutOverrides, u32),
        ),
        ("ustruct.children", truth.ustruct.children + 8, |o, v| {
            o.ustruct_children = Some(v)
        }),
        (
            "properties.element_size",
            truth.properties.element_size + 4,
            |o, v| o.properties_element_size = Some(v),
        ),
    ] {
        let mut overrides = LayoutOverrides::default();
        apply(&mut overrides, wrong);
        let options = DiscoverOptions {
            overrides,
            ..DiscoverOptions::default()
        };
        match discover(&fx.memory, &options) {
            Err(ScanError::OverrideContradiction {
                field: got,
                given,
                found,
            }) => {
                assert_eq!(got, field);
                assert_eq!(given, u64::from(wrong));
                assert_ne!(found, given);
            }
            other => panic!(
                "{field}: expected a contradiction, got {:?}",
                other.map(|_| ())
            ),
        }
    }
    assert!(LayoutOverrides::NAMES.contains(&"uobject.class"));
}

#[test]
fn a_scrambled_objects_pointer_needs_the_transform() {
    const KEY: u64 = 0xA5A5_0000_0000_0000;
    for name in ["ue4.25-cp", "ue4.17"] {
        let fx = build(named(name), 2, 300);
        let slot = fx.anchors.data.object_array + u64::from(fx.layout.objects.objects);
        let snapshot = rebuild(&fx, |parts| {
            let stored = peek_u64(parts, slot);
            poke(parts, slot, &(stored ^ KEY).to_le_bytes());
        });

        let plain = discover(&snapshot, &DiscoverOptions::default());
        match plain {
            Err(ScanError::ObjectArrayNotFound { hint, .. }) => {
                assert!(hint.contains("pointer_transform"), "{name}: {hint}");
            }
            other => panic!(
                "{name}: expected a clear error, got {:?}",
                other.map(|_| ())
            ),
        }

        let options = DiscoverOptions {
            pointer_transform: vec![TransformOp::Xor(KEY)],
            ..DiscoverOptions::default()
        };
        let d = discover(&snapshot, &options).unwrap();
        let mut expected = fx.clone();
        expected.layout.objects.pointer_transform = vec![TransformOp::Xor(KEY)];
        assert_eq!(
            d.layout.objects.pointer_transform,
            vec![TransformOp::Xor(KEY)]
        );
        // The module has no PE headers here, but every layout field matches.
        assert_equal_to_truth(&expected, &d);
    }
}

#[test]
fn a_wrong_hint_never_overrides_the_evidence() {
    let fx = build(Profile::ue4_25_case_preserving_measured(), 1, 300);
    for (major, minor) in [(4, 20), (5, 8), (4, 11)] {
        let options = DiscoverOptions {
            hint_version: Some(EngineVersion {
                major,
                minor,
                patch: 0,
            }),
            ..DiscoverOptions::default()
        };
        let d = discover(&fx.memory, &options).unwrap();
        assert_equal_to_truth(&fx, &d);
    }
}

#[test]
fn the_hint_only_orders_an_unobservable_storage_width() {
    // The storage width of MinAlignment is the one thing the images do not
    // show: the bytes after the value are zero either way.
    let old = build(named("ue5.3"), 1, 300);
    let new = build(named("ue5.7"), 1, 300);
    let with = |minor| DiscoverOptions {
        hint_version: Some(EngineVersion {
            major: 5,
            minor,
            patch: 0,
        }),
        ..DiscoverOptions::default()
    };
    let d_old = discover(&old.memory, &with(3)).unwrap();
    let d_new = discover(&new.memory, &with(7)).unwrap();
    assert_eq!(
        d_old.layout.ustruct.min_alignment_width,
        old.layout.ustruct.min_alignment_width
    );
    assert_eq!(
        d_new.layout.ustruct.min_alignment_width,
        new.layout.ustruct.min_alignment_width
    );
    assert!(
        d_new
            .warnings
            .iter()
            .any(|w| w.contains("min_alignment_width")),
        "the width must be reported as not observed"
    );
}

#[test]
fn a_thread_pool_gives_the_same_result() {
    let fx = build(Profile::shuffled(5), 5, 300);
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(2)
        .build()
        .unwrap();
    let with_pool = DiscoverOptions {
        pool: Some(&pool),
        ..DiscoverOptions::default()
    };
    let a = discover(&fx.memory, &with_pool).unwrap();
    let b = run(&fx);
    assert_eq!(a.layout, b.layout);
    assert_eq!(a.globals, b.globals);
    assert_equal_to_truth(&fx, &a);
}

#[test]
fn discovery_is_deterministic() {
    let fx = build(Profile::shuffled(8), 8, 300);
    let a = run(&fx);
    let b = run(&fx);
    assert_eq!(a.layout, b.layout);
    assert_eq!(a.evidence, b.evidence);
    assert_eq!(a.warnings, b.warnings);
}

#[test]
fn a_single_block_name_table_is_solved_or_reported_as_ordered() {
    // One pool block: the names reach only block 0, so the split of a name
    // index is settled by the region the block fills, which is exact in these
    // images. One array chunk: the chunk size is a choice and is reported.
    let fx = FixtureBuilder::new(named("ue4.25-cp"))
        .seed(1)
        .filler_objects(300)
        .build();
    let d = run(&fx);
    let evidence = d.evidence_for("names.block_offset_bits").unwrap();
    assert!(evidence.method.contains("exactly"), "{}", evidence.method);
    assert_equal_to_truth(&fx, &d);

    let fx = FixtureBuilder::new(named("ue4.21"))
        .seed(1)
        .filler_objects(300)
        .build();
    let d = run(&fx);
    assert!(
        d.warnings.iter().any(|w| w.contains("elements_per_chunk")),
        "{:?}",
        d.warnings
    );
    // Everything else is still exact.
    assert_eq!(d.layout.objects.kind, fx.layout.objects.kind);
    assert_eq!(d.layout.uobject, fx.layout.uobject);
    let (g, w) = (&d.layout.properties, &fx.layout.properties);
    assert_eq!(
        (
            g.array_dim,
            g.element_size,
            g.flags,
            g.offset_internal,
            g.size
        ),
        (
            w.array_dim,
            w.element_size,
            w.flags,
            w.offset_internal,
            w.size
        )
    );
    match (&d.layout.names, &fx.layout.names) {
        (NameTableLayout::Array(g), NameTableLayout::Array(w)) => assert_eq!(g.entry, w.entry),
        _ => panic!("table kind"),
    }
}

#[test]
fn the_engine_facts_combine_structure_and_version_strings() {
    let fx = build(Profile::ue4_25_case_preserving_measured(), 1, 300);
    let d = run(&fx);
    let v = |major, minor| EngineVersion {
        major,
        minor,
        patch: 0,
    };
    assert!(d.engine.min_version >= Some(v(4, 25)));
    assert_eq!(d.engine.max_version, None);
    assert_eq!(d.engine.claimed.as_deref(), fx.engine.claimed.as_deref());
    assert_eq!(d.engine.build.as_deref(), Some(fx.engine.build.as_str()));
    assert!(d.engine.evidence.iter().any(|e| e.contains("FField")));

    let fx = build(Profile::ue4_21_measured(), 1, 300);
    let d = run(&fx);
    assert_eq!(d.engine.max_version, Some(v(4, 22)));
    assert!(d.engine.min_version >= Some(v(4, 21)));
}

#[test]
fn missing_version_facts_never_fail_discovery() {
    let fx = build(Profile::ue4_25_case_preserving_measured(), 1, 300);
    // The rebuilt image has no PE headers at the module base.
    let snapshot = rebuild(&fx, |_| {});
    let d = discover(&snapshot, &DiscoverOptions::default()).unwrap();
    assert!(d.engine.claimed.is_none());
    assert!(
        d.engine.min_version.is_some(),
        "the structural facts remain"
    );
    assert!(
        d.engine
            .evidence
            .iter()
            .any(|e| e.contains("version facts not read"))
    );
    assert_equal_to_truth(&fx, &d);
}

/// Counts the reads that reach the reader.
struct Counting {
    inner: Snapshot,
    reads: AtomicU64,
}

impl MemoryReader for Counting {
    fn read_exact(&self, addr: u64, buf: &mut [u8]) -> Result<(), MemError> {
        self.reads.fetch_add(1, Ordering::Relaxed);
        self.inner.read_exact(addr, buf)
    }

    fn regions(&self) -> Result<Vec<MemoryRegion>, MemError> {
        self.inner.regions()
    }

    fn modules(&self) -> Result<Vec<ModuleInfo>, MemError> {
        self.inner.modules()
    }
}

/// Reads allowed for discovery on the default fixture. The module data is
/// copied with a few large reads and everything else goes through a 4 KiB
/// page cache, so the count is bounded by the pages the solver touches, not by
/// the slots it examines: scanning the 256 KiB `.data` slot by slot would take
/// tens of thousands of reads.
const READ_BUDGET: u64 = 3000;

#[test]
fn discovery_reads_pages_and_not_slots() {
    for name in ["ue4.25-cp", "ue4.21", "ue5.7"] {
        let fx = FixtureBuilder::new(named(name))
            .seed(1)
            .name_blocks(3)
            .build();
        let counting = Counting {
            inner: fx.memory.clone(),
            reads: AtomicU64::new(0),
        };
        let d = discover(&counting, &DiscoverOptions::default()).unwrap();
        assert_equal_to_truth(&fx, &d);
        let reads = counting.reads.load(Ordering::Relaxed);
        eprintln!("{name}: {reads} reads for {} objects", fx.objects.len());
        assert!(reads < READ_BUDGET, "{name}: {reads} reads");
    }
    let _ = absolute;
}
