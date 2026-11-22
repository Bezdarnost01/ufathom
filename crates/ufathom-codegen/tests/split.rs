//! The split layout: member crates under `crates/`, a root crate that
//! re-exports them under the same module paths and holds the user's code.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::print_stderr,
    reason = "tests may panic and report timings"
)]

mod common;

use std::fs;
use std::path::Path;

use common::dumps::{
    ALPHA, BETA, DELTA, ENGINE, GAMMA, MODS, behaviour_dump, cyclic_packages_dump, golden_dump,
    path,
};
use common::{local_options, read_tree, run_cargo};
use ufathom_codegen::{CrateInfo, GenOptions, GenReport, MANIFEST_NAME, SdkLayout, generate};
use ufathom_model::{FieldDef, PropertyType};

fn split(bytes: usize) -> GenOptions {
    GenOptions {
        layout: SdkLayout::Split,
        split_crate_bytes: bytes,
        ..local_options()
    }
}

fn single() -> GenOptions {
    GenOptions {
        layout: SdkLayout::Single,
        ..local_options()
    }
}

/// The member crate that holds a package, by its dump path.
fn crate_of<'a>(report: &'a GenReport, package: &str) -> &'a CrateInfo {
    report
        .crates
        .iter()
        .find(|c| c.packages.iter().any(|p| p == package))
        .unwrap_or_else(|| panic!("no crate holds {package}: {:?}", report.crates))
}

#[test]
fn packages_that_reference_each_other_share_a_crate() {
    let dir = tempfile::tempdir().unwrap();
    let report = generate(&cyclic_packages_dump(), &split(0), dir.path()).unwrap();

    let alpha = crate_of(&report, ALPHA);
    let beta = crate_of(&report, BETA);
    assert_eq!(alpha.name, beta.name, "a cycle cannot span two crates");
    assert_eq!(alpha.packages, vec![ALPHA.to_owned(), BETA.to_owned()]);

    // Everything else has a crate of its own, and crates only depend on
    // crates that come before them.
    let gamma = crate_of(&report, GAMMA);
    let delta = crate_of(&report, DELTA);
    let core = crate_of(&report, "/Script/CoreUObject");
    assert_eq!(report.crates.len(), 4);
    assert_ne!(gamma.name, alpha.name);
    assert!(gamma.dependencies.contains(&alpha.name));
    assert_eq!(delta.dependencies, vec![core.name.clone()]);
    assert_eq!(alpha.dependencies, vec![core.name.clone()]);
    let order: Vec<&str> = report.crates.iter().map(|c| c.name.as_str()).collect();
    for member in &report.crates {
        let own = order.iter().position(|n| *n == member.name).unwrap();
        for dependency in &member.dependencies {
            let at = order.iter().position(|n| n == dependency).unwrap();
            assert!(at < own, "{dependency} comes before {}", member.name);
        }
    }
}

#[test]
fn a_package_cycle_compiles_as_one_member_crate() {
    let dir = tempfile::Builder::new()
        .prefix("sdk-cycle-")
        .tempdir()
        .unwrap();
    generate(&cyclic_packages_dump(), &split(0), dir.path()).unwrap();
    let run = run_cargo(dir.path(), &["check", "--all-targets"]);
    assert!(!run.stderr.contains("warning:"), "{}", run.stderr);
    // A package that derives from the cycle builds with it.
    let run = run_cargo(
        dir.path(),
        &["check", "--no-default-features", "--features", "gamma"],
    );
    assert!(!run.stderr.contains("warning:"), "{}", run.stderr);
}

#[test]
fn the_same_dump_gives_the_same_split_bytes() {
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    generate(&golden_dump(), &split(0), a.path()).unwrap();
    let mut dump = golden_dump();
    dump.structs.reverse();
    dump.enums.reverse();
    dump.packages.reverse();
    generate(&dump, &split(0), b.path()).unwrap();
    assert_eq!(read_tree(a.path()), read_tree(b.path()));
}

#[test]
fn module_files_are_the_same_in_both_layouts() {
    let one = tempfile::tempdir().unwrap();
    let many = tempfile::tempdir().unwrap();
    let report = generate(&golden_dump(), &single(), one.path()).unwrap();
    assert!(report.crates.is_empty());
    let report = generate(&golden_dump(), &split(0), many.path()).unwrap();
    assert!(!report.crates.is_empty());
    let mut compared = 0;
    for member in &report.crates {
        let src = many.path().join("crates").join(&member.name).join("src");
        for entry in fs::read_dir(&src).unwrap() {
            let name = entry.unwrap().file_name().to_string_lossy().into_owned();
            if name == "lib.rs" {
                continue;
            }
            let expected = fs::read_to_string(one.path().join("src").join(&name)).unwrap();
            let actual = fs::read_to_string(src.join(&name)).unwrap();
            assert_eq!(expected, actual, "{name}");
            compared += 1;
        }
    }
    assert_eq!(compared, report.packages);
}

#[test]
fn packing_small_packages_gives_fewer_crates() {
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    let apart = generate(&golden_dump(), &split(0), a.path()).unwrap();
    let packed = generate(&golden_dump(), &split(usize::MAX), b.path()).unwrap();
    assert_eq!(apart.crates.len(), apart.packages);
    assert!(
        packed.crates.len() < apart.crates.len(),
        "{:?}",
        packed.crates
    );
    // Packing keeps every package.
    let held: usize = packed.crates.iter().map(|c| c.packages.len()).sum();
    assert_eq!(held, packed.packages);
}

#[test]
fn the_root_crate_keeps_the_entry_points_and_re_exports_the_packages() {
    let dir = tempfile::tempdir().unwrap();
    let report = generate(&golden_dump(), &split(0), dir.path()).unwrap();
    for file in [
        "Cargo.toml",
        "src/lib.rs",
        "src/main.rs",
        "src/dll.rs",
        "build.rs",
    ] {
        assert!(dir.path().join(file).is_file(), "missing {file}");
    }
    // No package module in the root crate.
    assert!(!dir.path().join("src/engine.rs").exists());
    let engine = crate_of(&report, ENGINE);
    assert!(
        dir.path()
            .join("crates")
            .join(&engine.name)
            .join("src/engine.rs")
            .is_file()
    );
    let lib = fs::read_to_string(dir.path().join("src/lib.rs")).unwrap();
    assert!(
        lib.contains("pub use ::ufathom_sdk_engine::engine;"),
        "{lib}"
    );
    assert!(!lib.contains("pub mod engine;"));
    let manifest = fs::read_to_string(dir.path().join("Cargo.toml")).unwrap();
    assert!(
        manifest.contains("\"dep:ufathom-sdk-engine\""),
        "{manifest}"
    );
    assert!(manifest.contains("\"ufathom-sdk-engine?/accessors\""));
    assert!(manifest.contains("\"ufathom-sdk-engine?/inproc\""));
}

#[test]
fn a_changed_package_rewrites_only_its_own_file() {
    let dir = tempfile::tempdir().unwrap();
    generate(&golden_dump(), &split(0), dir.path()).unwrap();
    let before = read_tree(dir.path());
    let again = generate(&golden_dump(), &split(0), dir.path()).unwrap();
    assert!(again.files_written.is_empty(), "{:?}", again.files_written);
    assert_eq!(read_tree(dir.path()), before);

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
    let third = generate(&dump, &split(0), dir.path()).unwrap();
    assert_eq!(
        third.files_written,
        vec!["crates/ufathom-sdk-engine/src/engine.rs".to_owned()]
    );
}

fn write_user_code(dir: &Path) {
    fs::write(dir.join("src/main.rs"), "// application code\n").unwrap();
}

#[test]
fn regenerating_an_old_sdk_with_the_split_layout_keeps_the_user_code() {
    let dir = tempfile::tempdir().unwrap();
    generate(&golden_dump(), &single(), dir.path()).unwrap();
    assert!(dir.path().join("src/engine.rs").is_file());
    write_user_code(dir.path());

    let report = generate(&golden_dump(), &split(0), dir.path()).unwrap();
    assert_eq!(
        fs::read_to_string(dir.path().join("src/main.rs")).unwrap(),
        "// application code\n"
    );
    assert!(report.files_unchanged.contains(&"src/main.rs".to_owned()));
    // The old package files are gone.
    for module in ["engine", "core_uobject", "mods"] {
        assert!(!dir.path().join(format!("src/{module}.rs")).exists());
        assert!(
            report.files_removed.contains(&format!("src/{module}.rs")),
            "{:?}",
            report.files_removed
        );
    }
    let manifest = fs::read_to_string(dir.path().join(MANIFEST_NAME)).unwrap();
    assert!(manifest.contains("crates/ufathom-sdk-engine/src/engine.rs"));
}

#[test]
fn going_back_to_the_single_layout_keeps_the_user_code_and_removes_the_members() {
    let dir = tempfile::tempdir().unwrap();
    generate(&golden_dump(), &split(0), dir.path()).unwrap();
    write_user_code(dir.path());

    generate(&golden_dump(), &single(), dir.path()).unwrap();
    assert_eq!(
        fs::read_to_string(dir.path().join("src/main.rs")).unwrap(),
        "// application code\n"
    );
    assert!(dir.path().join("src/engine.rs").is_file());
    // The member crates, and their now empty folders, are gone.
    assert!(!dir.path().join("crates").exists());
    // The tree has the files of one generated from scratch and nothing else.
    let fresh = tempfile::tempdir().unwrap();
    generate(&golden_dump(), &single(), fresh.path()).unwrap();
    let names = |t: Vec<(String, String)>| t.into_iter().map(|(n, _)| n).collect::<Vec<_>>();
    assert_eq!(names(read_tree(dir.path())), names(read_tree(fresh.path())));
}

#[test]
fn a_file_the_user_added_to_a_member_folder_is_not_removed() {
    let dir = tempfile::tempdir().unwrap();
    generate(&golden_dump(), &split(0), dir.path()).unwrap();
    let note = dir.path().join("crates/ufathom-sdk-mods/notes.txt");
    fs::write(&note, "mine").unwrap();
    generate(&golden_dump(), &single(), dir.path()).unwrap();
    assert!(note.is_file());
}

#[test]
fn a_vanished_package_removes_its_member_crate() {
    let dir = tempfile::tempdir().unwrap();
    generate(&golden_dump(), &split(0), dir.path()).unwrap();
    assert!(dir.path().join("crates/ufathom-sdk-mods").is_dir());
    let mut dump = golden_dump();
    dump.structs.retain(|s| s.package != MODS);
    dump.packages.retain(|p| p != MODS);
    let report = generate(&dump, &split(0), dir.path()).unwrap();
    assert!(
        report
            .files_removed
            .contains(&"crates/ufathom-sdk-mods/src/mods.rs".to_owned()),
        "{:?}",
        report.files_removed
    );
    assert!(!dir.path().join("crates/ufathom-sdk-mods").exists());
}

#[test]
fn the_split_sdk_builds_with_all_packages_with_one_package_and_with_inproc() {
    let dir = tempfile::Builder::new()
        .prefix("sdk-split-")
        .tempdir()
        .unwrap();
    generate(&golden_dump(), &split(0), dir.path()).unwrap();
    let all = run_cargo(dir.path(), &["check", "--all-targets"]);
    assert!(!all.stderr.contains("warning:"), "{}", all.stderr);
    for features in ["inproc", "detour", "inproc,accessors"] {
        let run = run_cargo(dir.path(), &["check", "--features", features]);
        assert!(!run.stderr.contains("warning:"), "{}", run.stderr);
    }
    // One package, with what it needs and nothing else.
    for feature in [
        "core_uobject",
        "engine",
        "mods",
        "main_pkg",
        "std_pkg",
        "core_pkg",
    ] {
        for features in [
            feature.to_owned(),
            format!("{feature},accessors"),
            format!("{feature},accessors,inproc"),
        ] {
            let run = run_cargo(
                dir.path(),
                &["check", "--no-default-features", "--features", &features],
            );
            assert!(
                !run.stderr.contains("warning:"),
                "{features}: {}",
                run.stderr
            );
        }
    }
}

#[test]
fn a_package_that_was_not_asked_for_is_not_built() {
    let dir = tempfile::Builder::new()
        .prefix("sdk-split-one-")
        .tempdir()
        .unwrap();
    generate(&golden_dump(), &split(0), dir.path()).unwrap();
    let run = run_cargo(
        dir.path(),
        &["check", "--no-default-features", "--features", "core_pkg"],
    );
    let log = run.stderr;
    assert!(log.contains("ufathom-sdk-core_pkg") || log.contains("ufathom-sdk-core-pkg"));
    for other in [
        "ufathom-sdk-engine",
        "ufathom-sdk-mods",
        "ufathom-sdk-main-pkg",
    ] {
        assert!(!log.contains(other), "{other} was built: {log}");
    }
}

#[test]
fn packed_crates_build_too() {
    let dir = tempfile::Builder::new()
        .prefix("sdk-packed-")
        .tempdir()
        .unwrap();
    generate(&golden_dump(), &split(usize::MAX), dir.path()).unwrap();
    let run = run_cargo(
        dir.path(),
        &["check", "--all-targets", "--features", "inproc"],
    );
    assert!(!run.stderr.contains("warning:"), "{}", run.stderr);
    let run = run_cargo(
        dir.path(),
        &["check", "--no-default-features", "--features", "main_pkg"],
    );
    assert!(!run.stderr.contains("warning:"), "{}", run.stderr);
}

#[test]
fn generated_sdk_behaves_the_same_through_the_root_crate() {
    let dir = tempfile::Builder::new()
        .prefix("sdk-split-behaviour-")
        .tempdir()
        .unwrap();
    let report = generate(&behaviour_dump(), &split(0), dir.path()).unwrap();
    assert!(report.crates.len() >= 2, "{:?}", report.crates);
    fs::create_dir_all(dir.path().join("tests")).unwrap();
    fs::write(
        dir.path().join("tests").join("behaviour.rs"),
        include_str!("sdk_tests/behaviour.rs"),
    )
    .unwrap();
    let run = run_cargo(dir.path(), &["test"]);
    assert!(
        run.stdout.contains("test result: ok. 13 passed; 0 failed"),
        "{}",
        run.stdout
    );
    let run = run_cargo(dir.path(), &["test", "--features", "inproc"]);
    assert!(
        run.stdout.contains("test result: ok. 15 passed; 0 failed"),
        "{}",
        run.stdout
    );
}

#[test]
fn the_bundled_runtime_builds_the_split_sdk_without_a_checkout() {
    let dir = tempfile::Builder::new()
        .prefix("standalone-split-sdk-")
        .tempdir()
        .unwrap();
    let options = GenOptions {
        layout: SdkLayout::Split,
        split_crate_bytes: 0,
        ..GenOptions::default()
    };
    generate(&golden_dump(), &options, dir.path()).unwrap();
    let member =
        fs::read_to_string(dir.path().join("crates/ufathom-sdk-engine/Cargo.toml")).unwrap();
    assert!(
        member.contains(r#"ufathom-runtime = { path = "../../vendor/ufathom-runtime" }"#),
        "{member}"
    );
    assert!(
        dir.path()
            .join("vendor/ufathom-runtime/src/lib.rs")
            .is_file()
    );
    for args in [
        &["check", "--all-targets"][..],
        &["build", "--lib"][..],
        &["build", "--bin", "ufathom-sdk-external"][..],
        &[
            "build",
            "--lib",
            "--features",
            "inproc detour hot-unload proxy-version",
        ][..],
    ] {
        let run = run_cargo(dir.path(), args);
        assert!(!run.stderr.contains("warning:"), "{args:?}: {}", run.stderr);
    }
}
