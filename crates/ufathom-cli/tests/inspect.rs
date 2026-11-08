//! `inspect-exe` on a PE file written from the testkit's fixture image.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "test code may panic on failure"
)]

mod common;

use std::path::Path;

use common::{arg, fixture, run};
use ufathom_mem::{MemoryReaderExt, Snapshot};
use ufathom_scan::PeImage;

/// Writes a testkit module back out as the PE file it models: headers, then
/// each section's raw data at the offset its header states.
fn pe_file(memory: &Snapshot, base: u64) -> Vec<u8> {
    let image = PeImage::parse_from_memory(memory, base).unwrap();
    let mut file = memory
        .read_vec::<u8>(base, image.size_of_headers as usize)
        .unwrap();
    for section in &image.sections {
        let data = memory
            .read_vec::<u8>(base + u64::from(section.rva), section.raw_size as usize)
            .unwrap();
        let start = section.raw_offset as usize;
        if file.len() < start {
            file.resize(start, 0);
        }
        file.truncate(start);
        file.extend_from_slice(&data);
    }
    file
}

fn write_exe(dir: &Path, profile: &str) -> (std::path::PathBuf, ufathom_testkit::Fixture) {
    let fx = fixture(profile, 5, 50);
    let path = dir.join("module.exe");
    std::fs::write(&path, pe_file(&fx.memory, fx.addresses.module_base)).unwrap();
    (path, fx)
}

#[test]
fn inspect_exe_reports_the_engine_version_and_sections_of_a_file() {
    let dir = tempfile::tempdir().unwrap();
    for profile in ["ue4.21", "ue4.25-cp"] {
        let (path, fx) = write_exe(dir.path(), profile);
        let claimed = fx.engine.claimed.clone().unwrap();
        let text = run(&["inspect-exe", &arg(&path)]).expect_code(0);
        text.stdout_has(&format!("engine:        {claimed}"))
            .stdout_has(&fx.engine.build)
            .stdout_has("PE 64-bit, 4 sections")
            .stdout_has(".text")
            .stdout_has(".rsrc")
            .stdout_has("no sign of packing");
    }
}

#[test]
fn inspect_exe_json_carries_the_same_facts() {
    let dir = tempfile::tempdir().unwrap();
    let (path, fx) = write_exe(dir.path(), "ue4.25-cp");
    let json = run(&["inspect-exe", &arg(&path), "--json"])
        .expect_code(0)
        .json();
    assert_eq!(json["file"], "module.exe");
    assert_eq!(json["bits"], 64);
    assert_eq!(
        json["engine"]["claimed"].as_str(),
        fx.engine.claimed.as_deref()
    );
    assert_eq!(
        json["engine"]["build"].as_str(),
        Some(fx.engine.build.as_str())
    );
    let sections: Vec<_> = json["sections"]
        .as_array()
        .unwrap()
        .iter()
        .map(|section| section["name"].as_str().unwrap())
        .collect();
    assert_eq!(sections, [".text", ".rdata", ".data", ".rsrc"]);
    assert_eq!(json["looks_packed"], false);
    assert!(json["packing_indicators"].as_array().unwrap().is_empty());
    // The JSON names the file, never its location.
    assert!(!json.to_string().contains(dir.path().to_str().unwrap()));
}

#[test]
fn a_file_that_is_not_a_pe_image_is_a_failure_that_says_so() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("notes.txt");
    std::fs::write(&path, "this is not an executable at all, only text").unwrap();
    run(&["inspect-exe", &arg(&path)])
        .expect_code(1)
        .stderr_has("cannot inspect")
        .stderr_has("notes.txt");
}

#[test]
fn a_truncated_pe_file_is_a_failure_not_a_crash() {
    let dir = tempfile::tempdir().unwrap();
    let (path, _) = write_exe(dir.path(), "ue4.25-cp");
    let bytes = std::fs::read(&path).unwrap();
    std::fs::write(&path, &bytes[..bytes.len() / 3]).unwrap();
    run(&["inspect-exe", &arg(&path)]).expect_code(1);
}
