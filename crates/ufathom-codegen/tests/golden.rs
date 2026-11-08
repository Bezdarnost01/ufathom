//! Golden files: the text of the generated crate for the hand-written dump.
//!
//! Refresh with `UPDATE_GOLDEN=1 cargo test -p ufathom-codegen --test golden`.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "tests may panic"
)]

mod common;

use std::path::Path;

use common::dumps::golden_dump;
use common::read_tree;
use ufathom_codegen::{DegradeReason, GenOptions, generate};

fn golden_dir() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("golden")
}

/// Keep the golden files focused on generated types, not vendored sources.
fn golden_options() -> GenOptions {
    GenOptions {
        runtime: ufathom_codegen::RuntimeDependency::Version("0.1.0".to_owned()),
        ..GenOptions::default()
    }
}

#[test]
fn generated_text_matches_the_golden_files() {
    let dir = tempfile::tempdir().unwrap();
    generate(&golden_dump(), &golden_options(), dir.path()).unwrap();
    let actual = read_tree(dir.path());
    let golden = golden_dir();
    if std::env::var_os("UPDATE_GOLDEN").is_some() {
        if golden.exists() {
            std::fs::remove_dir_all(&golden).unwrap();
        }
        for (rel, text) in &actual {
            let target = golden.join(rel);
            std::fs::create_dir_all(target.parent().unwrap()).unwrap();
            std::fs::write(target, text).unwrap();
        }
        return;
    }
    let expected = read_tree(&golden);
    let names = |t: &[(String, String)]| t.iter().map(|(n, _)| n.clone()).collect::<Vec<_>>();
    assert_eq!(
        names(&actual),
        names(&expected),
        "set of files differs; run with UPDATE_GOLDEN=1"
    );
    for ((name, got), (_, want)) in actual.iter().zip(&expected) {
        assert!(
            got == want,
            "{name} differs from tests/golden/{name}; run with UPDATE_GOLDEN=1 and review the diff"
        );
    }
}

#[test]
fn same_dump_gives_the_same_bytes() {
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    generate(&golden_dump(), &golden_options(), a.path()).unwrap();
    // A shuffled copy of the same dump: the output must not depend on input order.
    let mut dump = golden_dump();
    dump.structs.reverse();
    dump.enums.reverse();
    dump.packages.reverse();
    for s in &mut dump.structs {
        s.fields.reverse();
        s.functions.reverse();
    }
    generate(&dump, &golden_options(), b.path()).unwrap();
    assert_eq!(read_tree(a.path()), read_tree(b.path()));
}

#[test]
fn report_lists_every_degradation_case() {
    let dir = tempfile::tempdir().unwrap();
    let report = generate(&golden_dump(), &golden_options(), dir.path()).unwrap();
    let reasons = report.by_reason();
    for reason in [
        DegradeReason::Overlap,
        DegradeReason::OutOfBounds,
        DegradeReason::Misaligned,
        DegradeReason::AlignmentTooLow,
        DegradeReason::UnknownType,
        DegradeReason::NoRuntimeView,
        DegradeReason::SizeMismatch,
        DegradeReason::InvalidBitfield,
        DegradeReason::BaseNotEmbedded,
        DegradeReason::EnumeratorOutOfRange,
        DegradeReason::FunctionSkipped,
    ] {
        assert!(reasons.contains_key(&reason), "no {reason} in {reasons:?}");
    }
    assert_eq!(report.packages, 6);
    assert!(report.functions >= 7);
    assert_eq!(report.functions_skipped, 1);
    assert!(report.degraded_fields() > 10);
    assert!(report.types() > 20);
}
