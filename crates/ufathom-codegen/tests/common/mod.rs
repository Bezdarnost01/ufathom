//! Helpers shared by the codegen integration tests: dump builders, the
//! `cargo check` driver and the golden file comparison.

#![allow(
    dead_code,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::too_many_arguments,
    reason = "each test binary uses a different part of this module; test helpers may panic"
)]

pub mod dumps;

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use ufathom_codegen::{GenOptions, GenReport, generate};
use ufathom_model::{Dump, EngineInfo, EngineLayout, EnumDef, StructDef, TargetInfo, ToolInfo};

/// The workspace root.
pub fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("crate is two levels below the workspace root")
        .to_owned()
}

/// Wraps definitions in a `Dump` with the given layout.
pub fn make_dump(layout: EngineLayout, structs: Vec<StructDef>, enums: Vec<EnumDef>) -> Dump {
    let mut dump = Dump::new(
        ToolInfo {
            name: "codegen-tests".to_owned(),
            version: "0".to_owned(),
        },
        TargetInfo {
            module: "module.exe".to_owned(),
            build_id: "test".to_owned(),
        },
        EngineInfo {
            claimed: Some("4.25.1".to_owned()),
            ..EngineInfo::default()
        },
        layout,
    );
    let mut packages: Vec<String> = structs
        .iter()
        .map(|s| s.package.clone())
        .chain(enums.iter().map(|e| e.package.clone()))
        .collect();
    packages.sort();
    packages.dedup();
    dump.packages = packages;
    dump.structs = structs;
    dump.enums = enums;
    dump.normalize();
    dump
}

/// Options that point the generated crate at the runtime in this workspace.
pub fn local_options() -> GenOptions {
    GenOptions::with_runtime_path(workspace_root().join("crates").join("ufathom-runtime"))
}

fn cargo() -> PathBuf {
    std::env::var_os("CARGO").map_or_else(|| PathBuf::from("cargo"), PathBuf::from)
}

/// The result of running cargo on a generated SDK.
pub struct CargoRun {
    pub elapsed: Duration,
    pub stdout: String,
    pub stderr: String,
}

/// Runs `cargo <args>` on the crate in `dir`, offline, with a target directory
/// shared by every test under `target/`.
///
/// Panics with the full output if cargo cannot be started or fails, so a test
/// never passes without having checked the crate.
pub fn run_cargo(dir: &Path, args: &[&str]) -> CargoRun {
    let root = workspace_root();
    let target = root.join("target").join("sdk-check");
    let lock = root.join("Cargo.lock");
    if lock.is_file() && !dir.join("Cargo.lock").exists() {
        // Resolve to the versions the workspace already uses, offline.
        std::fs::copy(&lock, dir.join("Cargo.lock")).expect("copy Cargo.lock");
    }
    let started = Instant::now();
    let output = Command::new(cargo())
        .args(args)
        .arg("--offline")
        .arg("--manifest-path")
        .arg(dir.join("Cargo.toml"))
        .env("CARGO_TARGET_DIR", &target)
        .env_remove("RUSTFLAGS")
        .env_remove("CARGO_ENCODED_RUSTFLAGS")
        .output()
        .unwrap_or_else(|e| {
            panic!(
                "cannot run cargo ({}): {e}. The compile tests need a working cargo.",
                cargo().display()
            )
        });
    let elapsed = started.elapsed();
    assert!(
        output.status.success(),
        "cargo {args:?} failed in {}:\n--- stdout ---\n{}\n--- stderr ---\n{}",
        dir.display(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    CargoRun {
        elapsed,
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    }
}

/// Generates `dump` into a fresh temp directory and runs `cargo check` on it.
/// Returns the directory (kept alive by the caller), the report and the time
/// cargo took.
pub fn generate_and_check(dump: &Dump, label: &str) -> (tempfile::TempDir, GenReport, Duration) {
    let dir = tempfile::Builder::new()
        .prefix(&format!("sdk-{label}-"))
        .tempdir()
        .expect("temp dir");
    let report = generate(dump, &local_options(), dir.path()).expect("generate");
    let run = run_cargo(dir.path(), &["check", "--all-targets"]);
    // The generated crate must be warning-free too.
    assert!(
        !run.stderr.contains("warning:"),
        "warnings in the generated crate {label}:
{}",
        run.stderr
    );
    (dir, report, run.elapsed)
}

/// Reads every file under `dir` as `(relative path with '/', text with LF)`.
pub fn read_tree(dir: &Path) -> Vec<(String, String)> {
    fn walk(base: &Path, dir: &Path, out: &mut Vec<(String, String)>) {
        let mut entries: Vec<_> = std::fs::read_dir(dir)
            .expect("read dir")
            .map(|e| e.expect("entry").path())
            .collect();
        entries.sort();
        for path in entries {
            if path.is_dir() {
                walk(base, &path, out);
            } else {
                let rel = path
                    .strip_prefix(base)
                    .expect("under base")
                    .to_string_lossy()
                    .replace('\\', "/");
                let text = std::fs::read_to_string(&path).expect("utf-8 file");
                out.push((rel, text.replace("\r\n", "\n")));
            }
        }
    }
    let mut out = Vec::new();
    if dir.is_dir() {
        walk(dir, dir, &mut out);
    }
    out
}
