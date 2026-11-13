//! Behaviour of generated code: reads a synthetic object through the generated
//! types, `ObjectRef` and `Snapshot`, by running `cargo test` inside a
//! generated SDK.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::print_stderr,
    reason = "tests may panic and report timings"
)]

mod common;

use common::dumps::behaviour_dump;
use common::{local_options, run_cargo};
use ufathom_codegen::generate;

#[test]
fn generated_sdk_reads_objects_and_calls_wrappers() {
    let dir = tempfile::Builder::new()
        .prefix("sdk-behaviour-")
        .tempdir()
        .unwrap();
    let report = generate(&behaviour_dump(), &local_options(), dir.path()).unwrap();
    assert_eq!(report.degraded_fields(), 0, "{:?}", report.degraded);
    std::fs::create_dir_all(dir.path().join("tests")).unwrap();
    std::fs::write(
        dir.path().join("tests").join("behaviour.rs"),
        include_str!("sdk_tests/behaviour.rs"),
    )
    .unwrap();
    let run = run_cargo(dir.path(), &["test"]);
    // Nine tests in the generated crate, all passing.
    assert!(
        run.stdout.contains("test result: ok. 9 passed; 0 failed"),
        "unexpected output:
{}",
        run.stdout
    );
    eprintln!("behaviour test: cargo test {:.1?}", run.elapsed);
}
