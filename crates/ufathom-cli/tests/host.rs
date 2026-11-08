//! `probe`, `doctor` and `dump` against `fake-host` children: real operating
//! system reads of a synthetic engine image, with the dump compared to the
//! truth the image was built from.

#![cfg(windows)]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "test code may panic on failure"
)]

mod common;

use common::{Host, arg, run};
use ufathom_model::{Dump, EngineLayout};
use ufathom_testkit::Corruption;
use ufathom_usmap::{Usmap, compare};

/// An image size at which `ZeroedObjectHeaders` is degraded (27 of 535
/// objects, above 5%) yet discovery still solves the layout (its name sample
/// needs 95%). The window is narrow: a bigger image is not degraded and a
/// smaller one cannot be solved.
const DEGRADED_FILLERS: usize = 470;

fn read_dump(path: &std::path::Path) -> Dump {
    Dump::from_json(&std::fs::read_to_string(path).unwrap()).unwrap()
}

fn assert_is_truth(dump: &Dump, host: &Host) {
    let truth = host.local().truth;
    assert_eq!(dump.structs, truth.structs, "structs");
    assert_eq!(dump.enums, truth.enums, "enums");
}

#[test]
fn probe_prints_the_solved_layout_of_a_hosted_image() {
    let mut host = Host::spawn("ue4.25-cp", 5, 300);
    let pid = host.pid().to_string();
    let result = run(&["probe", "--pid", &pid]).expect_code(0);
    result
        .stdout_has(&format!("target:        process {pid}"))
        .stdout_has("object array:  chunked, 0x10000 per chunk; item 0x18 bytes")
        .stdout_has("name pool, stride 4")
        .stdout_has("FName 12 bytes; UObject 0x30 bytes; properties are FProperty fields")
        .stdout_has("GWorld found; GEngine found; ProcessEvent vtable index")
        .stdout_has("solved fields");
    assert!(host.close_and_wait(), "the host must survive being read");
}

#[test]
fn probe_json_has_the_layout_the_image_was_built_from() {
    let mut host = Host::spawn("ue4.25-cp", 5, 300);
    let local = host.local();
    let json = run(&["probe", "--pid", &host.pid().to_string(), "--json"])
        .expect_code(0)
        .json();
    let layout: EngineLayout = serde_json::from_value(json["layout"].clone()).unwrap();
    // The state of the name pool (blocks in use, cursor) and the typed
    // payloads the image has no instance of are not offsets of the image's
    // layout, so they are left out of the comparison.
    let truth = &local.layout;
    assert_eq!(layout.pointer_size, truth.pointer_size);
    assert_eq!(layout.objects, truth.objects);
    assert_eq!(layout.fname, truth.fname);
    assert_eq!(layout.uobject, truth.uobject);
    assert_eq!(layout.ufield, truth.ufield);
    assert_eq!(layout.ustruct, truth.ustruct);
    assert_eq!(layout.uclass, truth.uclass);
    assert_eq!(layout.ufunction, truth.ufunction);
    assert_eq!(layout.uenum, truth.uenum);
    assert_eq!(layout.properties.system, truth.properties.system);
    assert_eq!(
        layout.properties.offset_internal,
        truth.properties.offset_internal
    );
    assert_eq!(
        json["globals"]["process_event_index"].as_u64().unwrap(),
        u64::from(local.globals.process_event_index)
    );
    assert!(!json["evidence"].as_array().unwrap().is_empty());
    assert!(json["warnings"].is_array());
    assert!(json["engine"].is_object());
    assert!(host.close_and_wait());
}

#[test]
fn doctor_reports_every_solved_field_and_what_is_missing() {
    let mut host = Host::spawn("ue4.25-cp", 5, 300);
    let result = run(&["doctor", "--pid", &host.pid().to_string()]).expect_code(0);
    result
        .stdout_has("Solved fields (")
        .stdout_has("agreement")
        .stdout_has("method")
        .stdout_has("uobject.class")
        .stdout_has("uobject.name")
        .stdout_has("Warnings (")
        // The fixture claims no engine version, which is a real gap.
        .stdout_has("Missing (")
        .stdout_has("engine version")
        .stdout_has("Verdict:");
    // Partial agreement is flagged, not hidden: ProcessEvent is found by a
    // vote among the virtual functions of one object.
    result.stdout_has("(partial)");
    assert!(host.close_and_wait());
}

#[test]
fn doctor_works_on_a_name_array_image_with_legacy_properties() {
    let mut host = Host::spawn("ue4.21", 6, 300);
    let result = run(&["doctor", "--pid", &host.pid().to_string()]).expect_code(0);
    result.stdout_has("Solved fields (").stdout_has("Verdict:");
    // A legacy engine has no FField members, so none is reported missing.
    assert!(!result.stdout.contains("ffield."), "{}", result.stdout);
    let text = run(&["probe", "--pid", &host.pid().to_string()]).expect_code(0);
    text.stdout_has("name array")
        .stdout_has("properties are UProperty objects");
    assert!(host.close_and_wait());
}

#[test]
fn dump_writes_exactly_the_truth_of_a_hosted_image() {
    for (profile, seed) in [("ue4.25-cp", 5), ("ue4.21", 6)] {
        let mut host = Host::spawn(profile, seed, 300);
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("deep/er/dump.json");
        let result =
            run(&["dump", "--pid", &host.pid().to_string(), "-o", &arg(&out)]).expect_code(0);
        result
            .stdout_has("degraded:      no")
            .stdout_has("0 objects failed")
            .stdout_has("wrote:")
            .stdout_has("classes");
        assert_is_truth(&read_dump(&out), &host);
        assert!(host.close_and_wait(), "{profile}: the host must survive");
    }
}

#[test]
fn dump_can_write_a_usmap_and_an_sdk_beside_the_json() {
    let mut host = Host::spawn("ue4.25-cp", 5, 300);
    let dir = tempfile::tempdir().unwrap();
    let json = dir.path().join("dump.json");
    let usmap_path = dir.path().join("out.usmap");
    let sdk = dir.path().join("sdk");
    let result = run(&[
        "dump",
        "--pid",
        &host.pid().to_string(),
        "-o",
        &arg(&json),
        "--usmap",
        &arg(&usmap_path),
        "--sdk",
        &arg(&sdk),
        "--runtime-version",
        "0.1",
        "--threads",
        "1",
    ])
    .expect_code(0);
    result.stdout_has("SDK:").stdout_has("1 threads");

    let dump = read_dump(&json);
    assert_is_truth(&dump, &host);
    let usmap = Usmap::parse(&std::fs::read(&usmap_path).unwrap()).unwrap();
    assert!(compare(&dump, &usmap).is_clean());
    let manifest = std::fs::read_to_string(sdk.join("Cargo.toml")).unwrap();
    assert!(manifest.contains(r#"{ version = "0.1" }"#), "{manifest}");
    assert!(sdk.join("src/lib.rs").is_file());
    assert!(host.close_and_wait());
}

#[test]
fn two_dumps_of_one_host_are_byte_identical() {
    let mut host = Host::spawn("ue4.25-cp", 5, 300);
    let dir = tempfile::tempdir().unwrap();
    let pid = host.pid().to_string();
    for name in ["a.json", "b.json"] {
        run(&["dump", "--pid", &pid, "-o", &arg(&dir.path().join(name))]).expect_code(0);
    }
    assert_eq!(
        std::fs::read(dir.path().join("a.json")).unwrap(),
        std::fs::read(dir.path().join("b.json")).unwrap()
    );
    assert!(host.close_and_wait());
}

#[test]
fn a_degraded_dump_exits_3_but_still_writes_its_files() {
    let mut host = Host::spawn_with(
        "ue4.25-cp",
        5,
        DEGRADED_FILLERS,
        &[Corruption::ZeroedObjectHeaders],
    );
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("dump.json");
    let pid = host.pid().to_string();
    let result = run(&["dump", "--pid", &pid, "-o", &arg(&out)]).expect_code(3);
    result
        .stdout_has("degraded:      YES")
        .stderr_has("the dump is degraded")
        .stderr_has("--allow-degraded");
    let dump = read_dump(&out);
    assert!(!dump.structs.is_empty());
    assert!(
        dump.diagnostics
            .iter()
            .any(|d| d.message.contains("index 0")),
        "the damage is recorded in the dump"
    );

    // Accepting the degraded result gives the same files and exit code 0.
    let accepted = dir.path().join("accepted.json");
    run(&[
        "dump",
        "--pid",
        &pid,
        "-o",
        &arg(&accepted),
        "--allow-degraded",
    ])
    .expect_code(0)
    .stdout_has("degraded:      YES");
    assert_eq!(
        std::fs::read(&out).unwrap(),
        std::fs::read(&accepted).unwrap()
    );
    assert!(host.close_and_wait());
}

#[test]
fn an_image_that_cannot_be_solved_fails_and_names_the_field() {
    // Zeroed headers on a small image defeat the name offset.
    let mut host = Host::spawn_with("ue4.25-cp", 7, 40, &[Corruption::ZeroedObjectHeaders]);
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("dump.json");
    let pid = host.pid().to_string();

    run(&["probe", "--pid", &pid])
        .expect_code(1)
        .stderr_has("discovery failed")
        .stderr_has("uobject.name")
        .stderr_has("--layout");
    let doctor = run(&["doctor", "--pid", &pid]).expect_code(1);
    doctor
        .stdout_has("DISCOVERY FAILED")
        .stderr_has("uobject.name");
    run(&["dump", "--pid", &pid, "-o", &arg(&out)])
        .expect_code(1)
        .stderr_has("uobject.name");
    assert!(!out.exists(), "a failed dump writes nothing");
    assert!(host.close_and_wait(), "reading a damaged host is harmless");
}

#[test]
fn a_process_that_is_gone_is_a_short_actionable_failure() {
    let pid = {
        let host = Host::spawn("ue4.25-cp", 5, 50);
        host.pid()
        // Dropping the host kills and reaps the child.
    };
    run(&["probe", "--pid", &pid.to_string()])
        .expect_code(1)
        .stderr_has(&format!("process {pid} does not exist"))
        .stderr_has("ufathom ps");
}

#[test]
fn a_layout_file_states_values_and_a_wrong_one_is_a_contradiction() {
    let mut host = Host::spawn("ue4.25-cp", 5, 300);
    let truth = host.local().layout;
    let dir = tempfile::tempdir().unwrap();
    let pid = host.pid().to_string();

    let right = dir.path().join("right.toml");
    std::fs::write(
        &right,
        format!(
            "[overrides]\n\"uobject.class\" = {}\nuobject.name = {}\n",
            truth.uobject.class, truth.uobject.name
        ),
    )
    .unwrap();
    run(&["probe", "--pid", &pid, "--layout", &arg(&right)]).expect_code(0);

    let wrong = dir.path().join("wrong.json");
    let bad = truth.uobject.class + 8;
    std::fs::write(
        &wrong,
        format!(r#"{{"overrides": {{"uobject.class": {bad}}}}}"#),
    )
    .unwrap();
    run(&["probe", "--pid", &pid, "--layout", &arg(&wrong)])
        .expect_code(1)
        .stderr_has("uobject.class");

    let typo = dir.path().join("typo.toml");
    std::fs::write(&typo, "[overrides]\n\"uobject.klass\" = 1\n").unwrap();
    run(&["probe", "--pid", &pid, "--layout", &arg(&typo)])
        .expect_code(1)
        .stderr_has("unknown override")
        .stderr_has("uobject.class");
    assert!(host.close_and_wait());
}

#[test]
fn a_hint_and_a_module_steer_discovery() {
    let mut host = Host::spawn("ue4.25-cp", 5, 300);
    let pid = host.pid().to_string();
    run(&["probe", "--pid", &pid, "--hint", "4.25"]).expect_code(0);
    run(&["probe", "--pid", &pid, "--module", "fake-host.exe"]).expect_code(0);
    run(&["probe", "--pid", &pid, "--module", "no-such-module.exe"])
        .expect_code(1)
        .stderr_has("no-such-module.exe");
    assert!(host.close_and_wait());
}
