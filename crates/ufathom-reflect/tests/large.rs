//! Timings on a large image. Ignored by default: building the image takes a
//! while. Run with
//! `cargo test -p ufathom-reflect --release --test large -- --ignored --nocapture`,
//! and set `UFATHOM_FILLERS` to change the size.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr,
    reason = "test code may panic on failure and reports on stderr"
)]

mod common;

use std::time::Instant;

use common::{build, discovery, dump_on};
use ufathom_reflect::{ReflectOptions, build_dump};
use ufathom_testkit::Profile;

#[test]
#[ignore = "builds an image of hundreds of thousands of objects"]
fn timings_on_a_large_image() {
    let fillers: usize = std::env::var("UFATHOM_FILLERS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(300_000);
    for profile in [
        Profile::ue4_25_case_preserving_measured(),
        Profile::ue4_21_measured(),
    ] {
        let started = Instant::now();
        let fx = build(profile, 1, fillers);
        eprintln!(
            "{}: image of {} objects built in {:?}",
            fx.profile.name,
            fx.objects.len(),
            started.elapsed()
        );
        let d = discovery(&fx);
        let sequential = build_dump(&fx.memory, &d, &ReflectOptions::default()).unwrap();
        eprintln!("sequential {:?}", sequential.stats);
        for threads in [2, 4] {
            let out = dump_on(&fx, &d, threads);
            assert_eq!(out.dump, sequential.dump);
            eprintln!(
                "{threads} threads: total {:?}, parse {:?}, headers {:?}",
                out.stats.timings.total, out.stats.timings.parse, out.stats.timings.headers
            );
        }
    }
}
