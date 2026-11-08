//! Generates an SDK from a real dump and checks that it compiles without
//! warnings. Local only: it needs a dump file that is never committed.
//!
//! ```text
//! UFATHOM_REAL_DUMP=path/to/dump.json UFATHOM_REAL_OUT=some/empty/dir \
//!     cargo test -p ufathom-codegen --test real_dump -- --ignored --nocapture
//! ```
//!
//! `UFATHOM_REAL_OUT` must be outside the repository. `CARGO_TARGET_DIR` is
//! set to a `target` folder inside it, so nothing is shared with the workspace.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr,
    reason = "a manual check that reports on stderr"
)]

mod common;

use std::path::PathBuf;
use std::process::Command;
use std::time::Instant;

use ufathom_codegen::generate;
use ufathom_model::Dump;

#[test]
#[ignore = "needs a local dump: set UFATHOM_REAL_DUMP and UFATHOM_REAL_OUT"]
fn real_dump_generates_and_compiles_without_warnings() {
    let dump_path =
        PathBuf::from(std::env::var_os("UFATHOM_REAL_DUMP").expect("UFATHOM_REAL_DUMP"));
    let out = PathBuf::from(std::env::var_os("UFATHOM_REAL_OUT").expect("UFATHOM_REAL_OUT"));
    let dump = Dump::from_json(&std::fs::read_to_string(&dump_path).unwrap()).unwrap();

    let started = Instant::now();
    let report = generate(&dump, &common::local_options(), &out).unwrap();
    eprintln!(
        "generated in {:.2?}: {} packages, {} classes, {} structs, {} enums, {} fields, {} functions ({} skipped)",
        started.elapsed(),
        report.packages,
        report.classes,
        report.script_structs,
        report.enums,
        report.fields,
        report.functions,
        report.functions_skipped
    );
    eprintln!("degradations by reason:");
    for (reason, count) in report.by_reason() {
        eprintln!("  {reason}: {count}");
        // A few examples of each, to see what kind of thing it is.
        for d in report
            .degraded
            .iter()
            .filter(|d| d.reason == reason)
            .take(4)
        {
            eprintln!("    {} / {}: {}", d.owner, d.member, d.detail);
        }
    }

    let lock = common::workspace_root().join("Cargo.lock");
    if lock.is_file() && !out.join("Cargo.lock").exists() {
        std::fs::copy(&lock, out.join("Cargo.lock")).unwrap();
    }
    let started = Instant::now();
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let output = Command::new(cargo)
        .args(["check", "--offline", "--manifest-path"])
        .arg(out.join("Cargo.toml"))
        .env("CARGO_TARGET_DIR", out.join("target"))
        .env_remove("RUSTFLAGS")
        .output()
        .expect("cargo can be started");
    eprintln!("cargo check took {:.1?}", started.elapsed());
    let stderr = String::from_utf8_lossy(&output.stderr);
    let warnings = stderr.matches("warning:").count();
    let errors = stderr.matches("error[").count() + stderr.matches("error:").count();
    eprintln!("warnings: {warnings}, errors: {errors}");
    assert!(output.status.success(), "cargo check failed:\n{stderr}");
    assert_eq!(warnings, 0, "{stderr}");
}
