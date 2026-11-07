//! Real executables, read-only. Ignored by default: the paths come from
//! `ufathom.local.toml` at the workspace root, under `[exe]`, and a key whose
//! path is absent is skipped with a message.
//!
//! Run with `cargo test -p ufathom-scan -- --ignored --nocapture`.
//!
//! The expectations name engine versions only. What these tests print can
//! contain strings from the files, so it is for the person running them and
//! is never written into the repository.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::print_stdout,
    clippy::print_stderr,
    reason = "test code may panic and print; clippy.toml only exempts #[test] functions, not helpers"
)]

use std::path::{Path, PathBuf};
use std::time::Instant;

use ufathom_scan::{ExeFacts, inspect_exe};

/// The path configured for `key`, or `None` with a message saying why not.
fn configured_path(key: &str) -> Option<PathBuf> {
    let file = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../ufathom.local.toml");
    let Ok(text) = std::fs::read_to_string(&file) else {
        eprintln!("SKIP {key}: ufathom.local.toml not found at the workspace root");
        return None;
    };
    let table: toml::Table = text.parse().expect("ufathom.local.toml is valid TOML");
    let path = table
        .get("exe")
        .and_then(|exe| exe.get(key))
        .and_then(toml::Value::as_str)
        .filter(|path| !path.trim().is_empty());
    let Some(path) = path else {
        eprintln!("SKIP {key}: no [exe] {key} entry in ufathom.local.toml");
        return None;
    };
    let path = PathBuf::from(path);
    if !path.is_file() {
        eprintln!("SKIP {key}: the configured file does not exist");
        return None;
    }
    Some(path)
}

/// Inspects the file for `key` and prints what was found.
fn inspect_key(key: &str) -> Option<ExeFacts> {
    let path = configured_path(key)?;
    let started = Instant::now();
    let facts = inspect_exe(&path).unwrap_or_else(|error| panic!("{key}: {error}"));
    let engine = &facts.version.engine;
    println!("== {key} ({:.1?})", started.elapsed());
    println!(
        "   PE: {} bit, {} sections, image size {:#x}",
        if facts.image.is_64_bit { 64 } else { 32 },
        facts.image.sections.len(),
        facts.image.size_of_image
    );
    println!("   claimed:     {:?}", engine.claimed);
    println!("   build:       {:?}", engine.build);
    println!(
        "   min_version: {:?}",
        engine.min_version.map(|v| v.to_string())
    );
    println!(
        "   max_version: {:?}",
        engine.max_version.map(|v| v.to_string())
    );
    println!("   markers:     {}", facts.version.markers.len());
    println!(
        "   looks packed: {} {:?}",
        facts.packing.looks_packed(),
        facts.packing.indicators
    );
    for line in &engine.evidence {
        println!("   - {line}");
    }
    Some(facts)
}

/// Asserts the claimed `(major, minor)`.
fn expect_claim(key: &str, major: u16, minor: u16) {
    let Some(facts) = inspect_key(key) else {
        return;
    };
    let claimed = facts.version.claimed.map(|version| version.branch());
    assert_eq!(
        claimed,
        Some((major, minor)),
        "{key}: claimed {:?}",
        facts.version.engine.claimed
    );
}

/// For files whose exact minor version is not known in advance: whatever is
/// claimed is a 5.x version, or nothing is claimed and a lower bound exists.
fn expect_unknown_minor_of_ue5(key: &str) {
    let Some(facts) = inspect_key(key) else {
        return;
    };
    let engine = &facts.version.engine;
    match &engine.claimed {
        Some(claimed) => assert!(claimed.starts_with('5'), "{key}: claimed {claimed}"),
        None => assert!(
            engine.min_version.is_some(),
            "{key}: nothing claimed and no lower bound"
        ),
    }
}

#[test]
#[ignore = "needs a real executable from ufathom.local.toml"]
fn ue4_13_claims_4_13() {
    expect_claim("ue4_13", 4, 13);
}

#[test]
#[ignore = "needs a real executable from ufathom.local.toml"]
fn ue4_21_claims_4_21() {
    expect_claim("ue4_21", 4, 21);
}

#[test]
#[ignore = "needs a real executable from ufathom.local.toml"]
fn ue4_22_claims_4_22() {
    expect_claim("ue4_22", 4, 22);
}

#[test]
#[ignore = "needs a real executable from ufathom.local.toml"]
fn ue4_25_claims_4_25() {
    expect_claim("ue4_25", 4, 25);
}

#[test]
#[ignore = "needs a real executable from ufathom.local.toml"]
fn ue5_4_claims_5_4() {
    expect_claim("ue5_4", 5, 4);
}

#[test]
#[ignore = "needs a real executable from ufathom.local.toml"]
fn ue5_is_a_ue5_version_or_has_a_lower_bound() {
    expect_unknown_minor_of_ue5("ue5");
}

#[test]
#[ignore = "needs a real executable from ufathom.local.toml"]
fn ue5_unknown_is_a_ue5_version_or_has_a_lower_bound() {
    expect_unknown_minor_of_ue5("ue5_unknown");
}
