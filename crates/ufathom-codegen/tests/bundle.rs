//! The default SDK builds from its own files, without an ufathom checkout.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "tests may panic"
)]

mod common;

use common::dumps::golden_dump;
use common::{read_tree, run_cargo};
use ufathom_codegen::{GenOptions, generate};

#[test]
fn bundled_sdk_compiles_without_an_ufathom_checkout() {
    let dir = tempfile::Builder::new()
        .prefix("standalone-sdk-")
        .tempdir()
        .unwrap();
    generate(&golden_dump(), &GenOptions::default(), dir.path()).unwrap();

    let manifest = std::fs::read_to_string(dir.path().join("Cargo.toml")).unwrap();
    assert!(
        manifest.contains(r#"ufathom-runtime = { path = "vendor/ufathom-runtime" }"#),
        "{manifest}"
    );
    assert!(manifest.contains(r#"crate-type = ["rlib", "cdylib"]"#));
    for file in [
        "src/main.rs",
        "src/dll.rs",
        "vendor/ufathom-runtime/Cargo.toml",
        "vendor/ufathom-runtime/src/lib.rs",
        "vendor/ufathom-mem/Cargo.toml",
        "vendor/ufathom-mem/src/lib.rs",
        "LICENSE-MIT",
        "LICENSE-APACHE",
    ] {
        assert!(dir.path().join(file).is_file(), "missing {file}");
    }

    let run = run_cargo(dir.path(), &["check", "--all-targets"]);
    assert!(
        !run.stderr.contains("warning:"),
        "warnings in the standalone SDK:\n{}",
        run.stderr
    );
    // Checking is not enough here: prove that both entry points link, including
    // the test harnesses Cargo builds for the library and the executable.
    for args in [
        &["build", "--lib"][..],
        &["build", "--bin", "ufathom-sdk-external"][..],
        &["test", "--no-run"][..],
    ] {
        let run = run_cargo(dir.path(), args);
        assert!(
            !run.stderr.contains("warning:"),
            "warnings while linking {args:?}:\n{}",
            run.stderr
        );
    }
}

#[test]
fn bundled_files_participate_in_incremental_output() {
    let dir = tempfile::tempdir().unwrap();
    let first = generate(&golden_dump(), &GenOptions::default(), dir.path()).unwrap();
    assert!(first.files_written.len() > 20);
    let before = read_tree(dir.path());

    let second = generate(&golden_dump(), &GenOptions::default(), dir.path()).unwrap();
    assert!(
        second.files_written.is_empty(),
        "{:?}",
        second.files_written
    );
    assert_eq!(second.files_unchanged.len(), first.files_written.len());
    assert_eq!(read_tree(dir.path()), before);
}

#[test]
fn an_explicit_runtime_does_not_leave_stale_vendor_files() {
    let dir = tempfile::tempdir().unwrap();
    generate(&golden_dump(), &GenOptions::default(), dir.path()).unwrap();
    assert!(
        dir.path()
            .join("vendor/ufathom-runtime/src/lib.rs")
            .is_file()
    );

    let options = GenOptions::with_runtime_path("elsewhere/ufathom-runtime");
    let report = generate(&golden_dump(), &options, dir.path()).unwrap();
    assert!(
        !dir.path()
            .join("vendor/ufathom-runtime/src/lib.rs")
            .exists()
    );
    assert!(
        report
            .files_removed
            .contains(&"vendor/ufathom-runtime/src/lib.rs".to_owned())
    );
}
