//! Shared helpers: build a fixture, discover, dump, and compare with truth.

#![allow(
    dead_code,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "test helpers may panic on failure; each test binary uses a different part"
)]

use std::collections::BTreeMap;
use std::sync::mpsc;
use std::time::Duration;

use rayon::ThreadPoolBuilder;
use ufathom_reflect::{ReflectOptions, ReflectOutput, build_dump};
use ufathom_scan::{DiscoverOptions, Discovery, discover};
use ufathom_testkit::{Corruption, Fixture, FixtureBuilder, Profile};

/// A fixture whose name table spans three blocks or chunks.
pub fn build(profile: Profile, seed: u64, fillers: usize) -> Fixture {
    FixtureBuilder::new(profile)
        .seed(seed)
        .filler_objects(fillers)
        .name_blocks(3)
        .build()
}

/// A fixture with corruptions applied.
pub fn build_corrupt(profile: Profile, seed: u64, fillers: usize, c: &[Corruption]) -> Fixture {
    let mut builder = FixtureBuilder::new(profile)
        .seed(seed)
        .filler_objects(fillers)
        .name_blocks(3);
    for &corruption in c {
        builder = builder.corrupt(corruption);
    }
    builder.build()
}

pub fn discovery(fx: &Fixture) -> Discovery {
    discover(&fx.memory, &DiscoverOptions::default())
        .unwrap_or_else(|e| panic!("{}: discover: {e}", fx.profile.name))
}

/// Runs `build_dump` sequentially.
pub fn dump(fx: &Fixture) -> ReflectOutput {
    let d = discovery(fx);
    build_dump(&fx.memory, &d, &ReflectOptions::default())
        .unwrap_or_else(|e| panic!("{}: build_dump: {e}", fx.profile.name))
}

/// Runs `build_dump` on a pool of `threads`.
pub fn dump_on(fx: &Fixture, d: &Discovery, threads: usize) -> ReflectOutput {
    let pool = ThreadPoolBuilder::new()
        .num_threads(threads)
        .build()
        .unwrap();
    let options = ReflectOptions {
        pool: Some(&pool),
        ..ReflectOptions::default()
    };
    build_dump(&fx.memory, d, &options)
        .unwrap_or_else(|e| panic!("{}: build_dump: {e}", fx.profile.name))
}

/// Runs `f` on its own thread and fails if it does not finish in time.
pub fn watchdog<T: Send + 'static>(
    what: &str,
    limit: Duration,
    f: impl FnOnce() -> T + Send + 'static,
) -> T {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(f());
    });
    rx.recv_timeout(limit)
        .unwrap_or_else(|_| panic!("{what}: no result within {limit:?} (hang or panic)"))
}

/// Compares every struct and enum of `out` with the truth of `fx`.
///
/// `StructDef::flags` is 0 on both sides by construction: the layout has no
/// offset for `ClassFlags` or `StructFlags`. Everything else is exact.
pub fn assert_matches_truth(fx: &Fixture, out: &ReflectOutput) {
    let name = &fx.profile.name;
    let got: BTreeMap<_, _> = out.dump.structs.iter().map(|s| (&s.path, s)).collect();
    let want: BTreeMap<_, _> = fx.truth.structs.iter().map(|s| (&s.path, s)).collect();
    let missing: Vec<_> = want.keys().filter(|p| !got.contains_key(*p)).collect();
    let extra: Vec<_> = got.keys().filter(|p| !want.contains_key(*p)).collect();
    assert!(missing.is_empty(), "{name}: structs missing: {missing:?}");
    assert!(extra.is_empty(), "{name}: structs not in truth: {extra:?}");
    for (path, want) in &want {
        let got = got[path];
        assert_eq!(got.kind, want.kind, "{name}: {path} kind");
        assert_eq!(got.name, want.name, "{name}: {path} name");
        assert_eq!(got.package, want.package, "{name}: {path} package");
        assert_eq!(got.super_path, want.super_path, "{name}: {path} super");
        assert_eq!(got.size, want.size, "{name}: {path} size");
        assert_eq!(got.alignment, want.alignment, "{name}: {path} alignment");
        assert_eq!(got.fields, want.fields, "{name}: {path} fields");
        assert_eq!(got.functions, want.functions, "{name}: {path} functions");
        assert_eq!(got.flags, want.flags, "{name}: {path} flags");
    }
    let got: BTreeMap<_, _> = out.dump.enums.iter().map(|e| (&e.path, e)).collect();
    let want: BTreeMap<_, _> = fx.truth.enums.iter().map(|e| (&e.path, e)).collect();
    assert_eq!(
        got.keys().collect::<Vec<_>>(),
        want.keys().collect::<Vec<_>>(),
        "{name}: enum paths"
    );
    for (path, want) in &want {
        assert_eq!(got[path], *want, "{name}: enum {path}");
    }
    assert_eq!(
        out.dump.structs, fx.truth.structs,
        "{name}: structs as a whole"
    );
    assert_eq!(out.dump.enums, fx.truth.enums, "{name}: enums as a whole");
}
