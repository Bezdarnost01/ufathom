//! `ufathom inspect-exe` on real executables. Ignored by default: the paths
//! come from `ufathom.local.toml` at the workspace root, under `[exe]`, and a
//! key whose path is absent is skipped with a message.
//!
//! Run with `cargo test -p ufathom-cli --test real_files -- --ignored --nocapture`.
//!
//! These tests print and assert engine versions only. The command's own
//! output can carry strings from the file, so it is parsed, never echoed.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::print_stdout,
    clippy::print_stderr,
    reason = "test code may panic and print; clippy.toml only exempts #[test] functions, not helpers"
)]

mod common;

use std::path::{Path, PathBuf};

use common::run;

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

/// What the command found, as versions only.
struct Versions {
    claimed: Option<String>,
    at_least: Option<String>,
}

/// Runs `inspect-exe --json` on the file for `key`.
fn inspect(key: &str) -> Option<Versions> {
    let path = configured_path(key)?;
    let run = run(&["inspect-exe", path.to_str().unwrap(), "--json"]);
    // Not `{run:?}`: the output can hold strings of the file.
    assert_eq!(
        run.code, 0,
        "{key}: inspect-exe failed with code {}",
        run.code
    );
    let json = run.json();
    let claimed = json["engine"]["claimed"].as_str().map(str::to_owned);
    let minimum = &json["engine"]["min_version"];
    let at_least = minimum["major"]
        .as_u64()
        .zip(minimum["minor"].as_u64())
        .map(|(major, minor)| format!("{major}.{minor}"));
    println!(
        "{key}: claimed {}, at least {}",
        claimed.as_deref().unwrap_or("nothing"),
        at_least.as_deref().unwrap_or("nothing")
    );
    Some(Versions { claimed, at_least })
}

/// Asserts the claimed `major.minor`.
fn expect_claim(key: &str, branch: &str) {
    let Some(found) = inspect(key) else {
        return;
    };
    let claimed = found.claimed.unwrap_or_default();
    let matches = claimed == branch || claimed.starts_with(&format!("{branch}."));
    assert!(matches, "{key}: expected {branch}, claimed {claimed:?}");
}

/// For files whose exact minor version is not known in advance: whatever is
/// claimed is a 5.x version, or nothing is claimed and a lower bound exists.
fn expect_ue5(key: &str) {
    let Some(found) = inspect(key) else {
        return;
    };
    match found.claimed {
        Some(claimed) => assert!(claimed.starts_with("5."), "{key}: claimed {claimed}"),
        None => assert!(
            found.at_least.is_some(),
            "{key}: nothing claimed and no lower bound"
        ),
    }
}

#[test]
#[ignore = "needs a real executable from ufathom.local.toml"]
fn ue4_13_claims_4_13() {
    expect_claim("ue4_13", "4.13");
}

#[test]
#[ignore = "needs a real executable from ufathom.local.toml"]
fn ue4_21_claims_4_21() {
    expect_claim("ue4_21", "4.21");
}

#[test]
#[ignore = "needs a real executable from ufathom.local.toml"]
fn ue4_22_claims_4_22() {
    expect_claim("ue4_22", "4.22");
}

#[test]
#[ignore = "needs a real executable from ufathom.local.toml"]
fn ue4_25_claims_4_25() {
    expect_claim("ue4_25", "4.25");
}

#[test]
#[ignore = "needs a real executable from ufathom.local.toml"]
fn ue5_4_claims_5_4() {
    expect_claim("ue5_4", "5.4");
}

#[test]
#[ignore = "needs a real executable from ufathom.local.toml"]
fn ue5_is_a_ue5_version_or_has_a_lower_bound() {
    expect_ue5("ue5");
}

#[test]
#[ignore = "needs a real executable from ufathom.local.toml"]
fn ue5_unknown_is_a_ue5_version_or_has_a_lower_bound() {
    expect_ue5("ue5_unknown");
}
