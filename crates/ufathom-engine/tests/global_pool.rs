//! A load with a pool never touches rayon's global pool.
//!
//! Rayon creates its global pool on first use and its threads can never be
//! stopped, which would keep a DLL from unloading. `build_global` fails once
//! the global pool exists, so it is a probe: after a parallel load it must
//! still succeed. This file must hold only this one test, since any other
//! test in the same process could create the global pool first. The control
//! in `global_pool_control.rs` shows the probe does fail when the pool is used.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "test code may panic on failure; clippy.toml only exempts #[test] functions, not helpers"
)]

mod common;

use ufathom_engine::{LoadOptions, NameTable};
use ufathom_testkit::{FixtureBuilder, Profile};

#[test]
fn a_parallel_load_leaves_the_global_pool_uninitialised() {
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(3)
        .build()
        .unwrap();
    for profile in [
        Profile::ue4_21_measured(),
        Profile::ue4_25_case_preserving_measured(),
    ] {
        let fx = FixtureBuilder::new(profile)
            .filler_objects(100)
            .name_blocks(3)
            .build();
        let options = LoadOptions::for_layout(&fx.layout, Some(&pool));
        let names = NameTable::load(&fx.memory, &fx.layout.names, &options).unwrap();
        assert!(names.stats().blocks >= 3);
        // The whole walk, with the pool for the names.
        common::walk(&fx, Some(&pool));
    }
    rayon::ThreadPoolBuilder::new()
        .num_threads(1)
        .build_global()
        .expect("the global pool was initialised by a load");
}
