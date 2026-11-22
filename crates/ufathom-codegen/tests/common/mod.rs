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
    pub success: bool,
    pub stdout: String,
    pub stderr: String,
}

/// The target directory cargo uses for the SDK in `dir`.
///
/// A single-crate SDK shares `target/sdk-check` with every other one. The
/// member crates of a split SDK have the same names and, being workspace
/// members, the same build identity in every SDK, so two different SDKs in
/// one target directory could reuse each other's artifacts. A split SDK gets
/// a directory named after its temp folder without the random suffix.
pub fn target_dir(dir: &Path) -> PathBuf {
    let base = workspace_root().join("target");
    if !dir.join("crates").is_dir() {
        return base.join("sdk-check");
    }
    let name = dir
        .file_name()
        .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
    // `tempfile` appends six random characters.
    let key = name.get(..name.len().saturating_sub(6)).unwrap_or(&name);
    base.join("sdk-check-split").join(key.trim())
}

/// Runs `cargo <args>` on the crate in `dir`, offline, with a target directory
/// shared by every test under `target/`.
///
/// Cargo runs with `dir` as its working directory, so the crate's own
/// `.cargo/config.toml` applies, not the one of this workspace.
///
/// Panics with the full output if cargo cannot be started or fails, so a test
/// never passes without having checked the crate.
pub fn run_cargo(dir: &Path, args: &[&str]) -> CargoRun {
    let run = run_cargo_unchecked(dir, args);
    assert!(
        run.success,
        "cargo {args:?} failed in {}:
--- stdout ---
{}
--- stderr ---
{}",
        dir.display(),
        run.stdout,
        run.stderr,
    );
    run
}

/// As [`run_cargo`], but a failing build is returned (`success` is false)
/// instead of panicking, for tests that expect cargo to refuse.
pub fn run_cargo_unchecked(dir: &Path, args: &[&str]) -> CargoRun {
    let root = workspace_root();
    let target = target_dir(dir);
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
        .current_dir(dir)
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
    CargoRun {
        elapsed: started.elapsed(),
        success: output.status.success(),
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

/// The generated source file of a package module, in either layout: `src/` of
/// a single-crate SDK or `src/` of the member crate that holds it.
pub fn module_file(dir: &Path, module: &str) -> PathBuf {
    let name = format!("{module}.rs");
    let own = dir.join("src").join(&name);
    if own.is_file() {
        return own;
    }
    std::fs::read_dir(dir.join("crates"))
        .expect("a split SDK has a crates folder")
        .map(|e| e.expect("entry").path().join("src").join(&name))
        .find(|p| p.is_file())
        .unwrap_or_else(|| panic!("no source file for module {module}"))
}

/// The text of every module file of a generated SDK, in either layout, in
/// path order. The root crate's own files (`lib.rs`, `main.rs`, `dll.rs`) and
/// the member crates' `lib.rs` are not included.
pub fn all_module_sources(dir: &Path) -> String {
    let mut folders = vec![dir.join("src")];
    if let Ok(members) = std::fs::read_dir(dir.join("crates")) {
        let mut members: Vec<_> = members.map(|e| e.expect("entry").path()).collect();
        members.sort();
        folders.extend(members.into_iter().map(|m| m.join("src")));
    }
    let mut text = String::new();
    for folder in folders {
        let mut files: Vec<_> = std::fs::read_dir(&folder)
            .expect("read dir")
            .map(|e| e.expect("entry").path())
            .filter(|p| {
                !matches!(
                    p.file_name().and_then(|n| n.to_str()),
                    Some("lib.rs" | "main.rs" | "dll.rs")
                )
            })
            .collect();
        files.sort();
        for file in files {
            text.push_str(&std::fs::read_to_string(file).expect("utf-8 file"));
        }
    }
    text
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

/// Export names of a PE file, read from its export directory.
#[allow(dead_code, reason = "used by some test binaries only")]
pub fn pe_exports(bytes: &[u8]) -> Vec<String> {
    let u16_at = |o: usize| u16::from_le_bytes(bytes[o..o + 2].try_into().unwrap());
    let u32_at = |o: usize| u32::from_le_bytes(bytes[o..o + 4].try_into().unwrap());
    let pe = u32_at(0x3c) as usize;
    let sections = usize::from(u16_at(pe + 6));
    let optional = pe + 24;
    let table = optional + usize::from(u16_at(pe + 20));
    let file_offset = |rva: u32| -> usize {
        (0..sections)
            .map(|i| table + i * 40)
            .find_map(|s| {
                let size = u32_at(s + 8).max(u32_at(s + 16));
                let (va, raw) = (u32_at(s + 12), u32_at(s + 20));
                (va..va + size)
                    .contains(&rva)
                    .then(|| (rva - va + raw) as usize)
            })
            .expect("RVA inside a section")
    };
    let directory_rva = u32_at(optional + 112);
    if directory_rva == 0 {
        return Vec::new();
    }
    let directory = file_offset(directory_rva);
    let count = u32_at(directory + 24) as usize;
    let names = file_offset(u32_at(directory + 32));
    (0..count)
        .map(|i| {
            let start = file_offset(u32_at(names + 4 * i));
            let end = start + bytes[start..].iter().position(|&b| b == 0).unwrap();
            String::from_utf8(bytes[start..end].to_vec()).unwrap()
        })
        .collect()
}

/// One export of a PE file: its name, if it has one, and its ordinal.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct PeExport {
    pub name: Option<String>,
    pub ordinal: u32,
}

/// Every export of a PE file, with ordinals, read from its export directory.
/// An export without a name (`NONAME`) has `name: None`. Sorted by ordinal.
#[allow(dead_code, reason = "used by some test binaries only")]
pub fn pe_export_table(bytes: &[u8]) -> Vec<PeExport> {
    let u16_at = |o: usize| u16::from_le_bytes(bytes[o..o + 2].try_into().unwrap());
    let u32_at = |o: usize| u32::from_le_bytes(bytes[o..o + 4].try_into().unwrap());
    let pe = u32_at(0x3c) as usize;
    let sections = usize::from(u16_at(pe + 6));
    let optional = pe + 24;
    let table = optional + usize::from(u16_at(pe + 20));
    let file_offset = |rva: u32| -> usize {
        (0..sections)
            .map(|i| table + i * 40)
            .find_map(|s| {
                let size = u32_at(s + 8).max(u32_at(s + 16));
                let (va, raw) = (u32_at(s + 12), u32_at(s + 20));
                (va..va + size)
                    .contains(&rva)
                    .then(|| (rva - va + raw) as usize)
            })
            .expect("RVA inside a section")
    };
    let directory_rva = u32_at(optional + 112);
    if directory_rva == 0 {
        return Vec::new();
    }
    let directory = file_offset(directory_rva);
    let base = u32_at(directory + 16);
    let functions = u32_at(directory + 20) as usize;
    let names = u32_at(directory + 24) as usize;
    let function_table = file_offset(u32_at(directory + 28));
    let name_table = file_offset(u32_at(directory + 32));
    let ordinal_table = file_offset(u32_at(directory + 36));
    let mut named = std::collections::BTreeMap::new();
    for i in 0..names {
        let start = file_offset(u32_at(name_table + 4 * i));
        let end = start + bytes[start..].iter().position(|&b| b == 0).unwrap();
        let name = String::from_utf8(bytes[start..end].to_vec()).unwrap();
        named.insert(usize::from(u16_at(ordinal_table + 2 * i)), name);
    }
    let mut out: Vec<PeExport> = (0..functions)
        // An RVA of 0 is a gap in the ordinal range, not an export.
        .filter(|&i| u32_at(function_table + 4 * i) != 0)
        .map(|i| PeExport {
            name: named.get(&i).cloned(),
            ordinal: base + u32::try_from(i).unwrap(),
        })
        .collect();
    out.sort();
    out
}

/// Names of the DLLs a PE file imports, lower-cased, read from its import
/// directory. Delay-load imports are not included.
#[allow(dead_code, reason = "used by some test binaries only")]
pub fn pe_imports(bytes: &[u8]) -> Vec<String> {
    let u16_at = |o: usize| u16::from_le_bytes(bytes[o..o + 2].try_into().unwrap());
    let u32_at = |o: usize| u32::from_le_bytes(bytes[o..o + 4].try_into().unwrap());
    let pe = u32_at(0x3c) as usize;
    let sections = usize::from(u16_at(pe + 6));
    let optional = pe + 24;
    let table = optional + usize::from(u16_at(pe + 20));
    let file_offset = |rva: u32| -> usize {
        (0..sections)
            .map(|i| table + i * 40)
            .find_map(|s| {
                let size = u32_at(s + 8).max(u32_at(s + 16));
                let (va, raw) = (u32_at(s + 12), u32_at(s + 20));
                (va..va + size)
                    .contains(&rva)
                    .then(|| (rva - va + raw) as usize)
            })
            .expect("RVA inside a section")
    };
    // The import directory is the second data directory, after the exports.
    let directory_rva = u32_at(optional + 112 + 8);
    if directory_rva == 0 {
        return Vec::new();
    }
    let mut names = Vec::new();
    let mut descriptor = file_offset(directory_rva);
    // Each descriptor is 20 bytes; a zero name RVA ends the table.
    while u32_at(descriptor + 12) != 0 {
        let start = file_offset(u32_at(descriptor + 12));
        let end = start + bytes[start..].iter().position(|&b| b == 0).unwrap();
        names.push(
            String::from_utf8(bytes[start..end].to_vec())
                .unwrap()
                .to_lowercase(),
        );
        descriptor += 20;
    }
    names
}

/// Whether `dll` is a C or C++ runtime that the game or the system may lack:
/// the Visual C++ runtime, the C++ standard library or the universal CRT.
pub fn is_c_runtime_import(dll: &str) -> bool {
    dll.starts_with("vcruntime")
        || dll.starts_with("msvcp")
        || dll.starts_with("msvcr")
        || dll.starts_with("ucrtbase")
        || dll.starts_with("api-ms-win-crt-")
}
