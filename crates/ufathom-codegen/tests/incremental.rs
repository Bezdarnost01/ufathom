//! A second generation rewrites only what changed.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "tests may panic"
)]

mod common;

use std::fs;

use common::dumps::{ENGINE, MODS, golden_dump, path};
use common::read_tree;
use ufathom_codegen::{GenOptions, MANIFEST_NAME, generate};
use ufathom_model::{FieldDef, PropertyType};

fn options() -> GenOptions {
    GenOptions {
        runtime: ufathom_codegen::RuntimeDependency::Version("0.1.0".to_owned()),
        ..GenOptions::default()
    }
}

fn modified(dir: &std::path::Path, files: &[String]) -> Vec<std::time::SystemTime> {
    files
        .iter()
        .map(|f| fs::metadata(dir.join(f)).unwrap().modified().unwrap())
        .collect()
}

#[test]
fn unchanged_dump_writes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let first = generate(&golden_dump(), &options(), dir.path()).unwrap();
    assert_eq!(first.files_written.len(), 10);
    let before = read_tree(dir.path());
    let times = modified(dir.path(), &first.files_written);
    let second = generate(&golden_dump(), &options(), dir.path()).unwrap();
    assert!(
        second.files_written.is_empty(),
        "{:?}",
        second.files_written
    );
    assert_eq!(second.files_unchanged.len(), 10);
    assert_eq!(read_tree(dir.path()), before);
    assert_eq!(
        times,
        modified(dir.path(), &first.files_written),
        "an unchanged file was touched"
    );
}

#[test]
fn one_changed_package_rewrites_only_its_file() {
    let dir = tempfile::tempdir().unwrap();
    generate(&golden_dump(), &options(), dir.path()).unwrap();
    let mut dump = golden_dump();
    let pawn = dump
        .structs
        .iter_mut()
        .find(|s| s.path == path(ENGINE, "Pawn"))
        .unwrap();
    pawn.fields.push(FieldDef {
        name: "Extra".to_owned(),
        offset: 0x50,
        ty: PropertyType::Int32,
        element_size: 4,
        array_dim: 1,
        flags: 0,
    });
    pawn.size = 0x58;
    let second = generate(&dump, &options(), dir.path()).unwrap();
    assert_eq!(second.files_written, vec!["src/engine.rs".to_owned()]);
    assert_eq!(second.files_unchanged.len(), 9);
    let text = fs::read_to_string(dir.path().join("src/engine.rs")).unwrap();
    assert!(text.contains("pub extra: i32"));
}

#[test]
fn regeneration_preserves_the_application_entry_point() {
    let dir = tempfile::tempdir().unwrap();
    generate(&golden_dump(), &options(), dir.path()).unwrap();
    let main = dir.path().join("src/main.rs");
    fs::write(&main, "// application code\n").unwrap();

    let second = generate(&golden_dump(), &options(), dir.path()).unwrap();
    assert_eq!(fs::read_to_string(&main).unwrap(), "// application code\n");
    assert!(!second.files_written.contains(&"src/main.rs".to_owned()));
    assert!(second.files_unchanged.contains(&"src/main.rs".to_owned()));
}

#[test]
fn a_package_that_disappears_loses_its_file() {
    let dir = tempfile::tempdir().unwrap();
    generate(&golden_dump(), &options(), dir.path()).unwrap();
    let mut dump = golden_dump();
    dump.structs.retain(|s| s.package != MODS);
    dump.packages.retain(|p| p != MODS);
    let second = generate(&dump, &options(), dir.path()).unwrap();
    assert_eq!(second.files_removed, vec!["src/mods.rs".to_owned()]);
    assert!(!dir.path().join("src/mods.rs").exists());
    // The manifest tracks what is left.
    let manifest = fs::read_to_string(dir.path().join(MANIFEST_NAME)).unwrap();
    assert!(!manifest.contains("mods.rs"));
    // Lib and Cargo.toml mention one package less, so they were rewritten.
    assert!(second.files_written.contains(&"src/lib.rs".to_owned()));
}

#[test]
fn invalid_options_are_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let mut o = options();
    o.crate_name = "bad name".to_owned();
    assert!(generate(&golden_dump(), &o, dir.path()).is_err());
    let mut o = options();
    o.crate_version = String::new();
    assert!(generate(&golden_dump(), &o, dir.path()).is_err());
}
