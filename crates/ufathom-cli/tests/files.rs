//! `gen`, `usmap`, `check-oracle` and `diff` on dumps produced in the test.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "test code may panic on failure"
)]

mod common;

use common::{arg, fixture, run, runtime_crate, write_dump};
use ufathom_model::Dump;
use ufathom_usmap::{Usmap, compare};

/// A folder with the dump of a UE 4.25 style image written as `old.json`.
struct Dumps {
    dir: tempfile::TempDir,
    dump: Dump,
}

impl Dumps {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let dump = write_dump(&fixture("ue4.25-cp", 5, 80), &dir.path().join("old.json"));
        Self { dir, dump }
    }

    fn path(&self, name: &str) -> std::path::PathBuf {
        self.dir.path().join(name)
    }
}

#[test]
fn gen_with_a_runtime_path_writes_a_crate_that_points_at_it() {
    let dumps = Dumps::new();
    let sdk = dumps.path("sdk");
    let runtime = runtime_crate();
    let result = run(&[
        "gen",
        &arg(&dumps.path("old.json")),
        "-o",
        &arg(&sdk),
        "--runtime-path",
        &arg(&runtime),
    ])
    .expect_code(0);
    result.stdout_has("SDK:").stdout_has("files written");

    let manifest = std::fs::read_to_string(sdk.join("Cargo.toml")).unwrap();
    let dependency = manifest
        .lines()
        .find(|line| line.starts_with("ufathom-runtime = "))
        .unwrap();
    assert!(dependency.contains("path = "), "{dependency}");
    assert!(sdk.join("src/lib.rs").is_file());
    let classes = dumps
        .dump
        .structs
        .iter()
        .filter(|def| def.kind == ufathom_model::StructKind::Class)
        .count();
    assert!(classes > 0);
    result.stdout_has(&format!("{classes} classes"));
}

#[test]
fn gen_with_a_runtime_version_writes_a_registry_dependency() {
    let dumps = Dumps::new();
    let sdk = dumps.path("sdk");
    run(&[
        "gen",
        &arg(&dumps.path("old.json")),
        "-o",
        &arg(&sdk),
        "--runtime-version",
        "0.1",
    ])
    .expect_code(0);
    let manifest = std::fs::read_to_string(sdk.join("Cargo.toml")).unwrap();
    assert!(
        manifest.contains(r#"ufathom-runtime = { version = "0.1" }"#),
        "{manifest}"
    );
}

#[test]
fn gen_twice_leaves_unchanged_files_alone() {
    let dumps = Dumps::new();
    let sdk = dumps.path("sdk");
    let args = [
        "gen",
        &arg(&dumps.path("old.json")),
        "-o",
        &arg(&sdk),
        "--runtime-version",
        "0.1",
    ];
    run(&args).expect_code(0).stdout_has("0 unchanged");
    run(&args).expect_code(0).stdout_has("0 files written");
}

#[test]
fn usmap_writes_a_file_the_oracle_check_accepts() {
    let dumps = Dumps::new();
    let usmap_path = dumps.path("out.usmap");
    run(&[
        "usmap",
        &arg(&dumps.path("old.json")),
        "-o",
        &arg(&usmap_path),
    ])
    .expect_code(0)
    .stdout_has("wrote:");

    let bytes = std::fs::read(&usmap_path).unwrap();
    let usmap = Usmap::parse(&bytes).expect("the written file is a usmap");
    assert!(compare(&dumps.dump, &usmap).is_clean());

    let text = run(&[
        "check-oracle",
        &arg(&dumps.path("old.json")),
        &arg(&usmap_path),
    ])
    .expect_code(0);
    text.stdout_has("result: clean");

    let json = run(&[
        "check-oracle",
        &arg(&dumps.path("old.json")),
        &arg(&usmap_path),
        "--json",
    ])
    .expect_code(0)
    .json();
    assert_eq!(json["missing_structs"], serde_json::json!([]));
    assert_eq!(
        json["counts"]["matched_structs"].as_u64().unwrap(),
        usmap.structs.len() as u64
    );
}

#[test]
fn check_oracle_fails_when_the_dump_lacks_something_the_usmap_has() {
    let dumps = Dumps::new();
    let usmap_path = dumps.path("out.usmap");
    run(&[
        "usmap",
        &arg(&dumps.path("old.json")),
        "-o",
        &arg(&usmap_path),
    ])
    .expect_code(0);

    // The same dump without one class.
    let mut smaller = dumps.dump.clone();
    let removed = smaller
        .structs
        .iter()
        .position(|def| def.name == "Pawn")
        .expect("the fixture has a Pawn");
    smaller.structs.remove(removed);
    std::fs::write(dumps.path("smaller.json"), smaller.to_json().unwrap()).unwrap();

    let result = run(&[
        "check-oracle",
        &arg(&dumps.path("smaller.json")),
        &arg(&usmap_path),
    ])
    .expect_code(1);
    result
        .stdout_has("Pawn")
        .stderr_has("does not satisfy the usmap");
    let json = run(&[
        "check-oracle",
        &arg(&dumps.path("smaller.json")),
        &arg(&usmap_path),
        "--json",
    ])
    .expect_code(1)
    .json();
    assert_eq!(json["missing_structs"], serde_json::json!(["Pawn"]));
}

#[test]
fn check_oracle_refuses_a_file_that_is_not_a_usmap() {
    let dumps = Dumps::new();
    std::fs::write(dumps.path("junk.usmap"), b"not a usmap").unwrap();
    run(&[
        "check-oracle",
        &arg(&dumps.path("old.json")),
        &arg(&dumps.path("junk.usmap")),
    ])
    .expect_code(1)
    .stderr_has("not a usable usmap");
}

#[test]
fn diff_of_a_dump_with_itself_is_empty() {
    let dumps = Dumps::new();
    let old = arg(&dumps.path("old.json"));
    run(&["diff", &old, &old])
        .expect_code(0)
        .stdout_has("No differences.");
    let json = run(&["diff", &old, &old, "--json"]).expect_code(0).json();
    assert_eq!(json["added_types"], serde_json::json!([]));
    assert_eq!(json["changed_types"], serde_json::json!([]));
}

#[test]
fn diff_reports_what_changed_between_two_dumps() {
    let dumps = Dumps::new();
    let mut newer = dumps.dump.clone();
    // One class gone, one field gone from another, one field moved.
    newer.structs.retain(|def| def.name != "Pawn");
    let actor = newer
        .structs
        .iter_mut()
        .find(|def| def.name == "Actor")
        .expect("the fixture has an Actor");
    let removed_field = actor.fields.remove(0).name;
    let moved = actor.fields.last_mut().expect("Actor keeps fields");
    moved.offset += 8;
    let moved_name = moved.name.clone();
    std::fs::write(dumps.path("new.json"), newer.to_json().unwrap()).unwrap();

    let old = arg(&dumps.path("old.json"));
    let new = arg(&dumps.path("new.json"));
    let text = run(&["diff", &old, &new]).expect_code(0);
    text.stdout_has("Types removed")
        .stdout_has("Pawn")
        .stdout_has("Types changed")
        .stdout_has(&removed_field)
        .stdout_has(&moved_name);

    let json = run(&["diff", &old, &new, "--json"]).expect_code(0).json();
    let removed: Vec<_> = json["removed_types"]
        .as_array()
        .unwrap()
        .iter()
        .map(|key| key["name"].as_str().unwrap())
        .collect();
    assert_eq!(removed, ["Pawn"]);
    let changed: Vec<_> = json["changed_types"]
        .as_array()
        .unwrap()
        .iter()
        .map(|change| change["key"]["name"].as_str().unwrap())
        .collect();
    assert!(changed.contains(&"Actor"), "{changed:?}");
}
