//! The critical test: the generated SDK must compile.
//!
//! Each test generates a crate into a temp directory and runs `cargo check`
//! on it, offline, with a target directory shared under `target/`. A test
//! that cannot start cargo fails with a message instead of passing.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::print_stderr,
    reason = "tests may panic and report timings"
)]

mod common;

use common::dumps::{golden_dump, testkit_dump};
use common::{generate_and_check, run_cargo};
use ufathom_codegen::generate;
use ufathom_model::EngineVersion;
use ufathom_testkit::Profile;

fn report(label: &str, report: &ufathom_codegen::GenReport, elapsed: std::time::Duration) {
    eprintln!(
        "compile test {label}: {} types, {} fields ({} degraded), {} functions, cargo check {:.1?}",
        report.types(),
        report.fields,
        report.degraded_fields(),
        report.functions,
        elapsed
    );
}

#[test]
fn golden_dump_compiles() {
    let (_dir, r, elapsed) = generate_and_check(&golden_dump(), "golden");
    report("golden", &r, elapsed);
    assert!(r.degraded_fields() > 0);
}

#[test]
fn golden_dump_compiles_one_package_at_a_time() {
    // Each feature must pull in everything its package refers to.
    let dump = golden_dump();
    let dir = tempfile::Builder::new()
        .prefix("sdk-features-")
        .tempdir()
        .unwrap();
    generate(&dump, &common::local_options(), dir.path()).unwrap();
    for feature in ["core_u_object", "engine", "mods"] {
        let run = run_cargo(
            dir.path(),
            &["check", "--no-default-features", "--features", feature],
        );
        eprintln!(
            "compile test feature {feature}: cargo check {:.1?}",
            run.elapsed
        );
    }
}

#[test]
fn ue4_21_measured_compiles() {
    let (_dir, r, elapsed) = generate_and_check(&testkit_dump(Profile::ue4_21_measured()), "ue421");
    report("ue4.21", &r, elapsed);
}

#[test]
fn ue4_25_case_preserving_compiles() {
    let dump = testkit_dump(Profile::ue4_25_case_preserving_measured());
    let (_dir, r, elapsed) = generate_and_check(&dump, "ue425cp");
    report("ue4.25-cp", &r, elapsed);
}

#[test]
fn ue5_7_compiles() {
    let profile = Profile::stock(EngineVersion {
        major: 5,
        minor: 7,
        patch: 0,
    })
    .unwrap();
    let (_dir, r, elapsed) = generate_and_check(&testkit_dump(profile), "ue57");
    report("ue5.7", &r, elapsed);
}

#[test]
fn ue5_case_preserving_compiles() {
    let (_dir, r, elapsed) = generate_and_check(
        &testkit_dump(Profile::ue5_case_preserving_assumed()),
        "ue5cp",
    );
    report("ue5-cp", &r, elapsed);
}
