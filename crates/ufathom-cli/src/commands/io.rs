//! Reading dumps and writing outputs, shared by the commands that do both.

use std::path::Path;

use anyhow::{Context, Result, anyhow};
use ufathom_codegen::{GenOptions, GenReport, RuntimeDependency, generate};
use ufathom_model::Dump;

use crate::args::RuntimeArgs;
use crate::error::{CliError, usage};

/// Reads a dump file.
///
/// # Errors
///
/// The file cannot be read, is not JSON, is not a dump, or has a
/// `format_version` this build does not read.
pub fn read_dump(path: &Path) -> Result<Dump> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("cannot read the dump file {}", path.display()))?;
    Dump::from_json(&text)
        .map_err(|error| anyhow!("{} is not a usable dump: {error}", path.display()))
}

/// Writes `bytes` to `path`, creating missing folders and replacing an
/// existing file.
///
/// # Errors
///
/// A folder or the file cannot be written.
pub fn write_file(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("cannot create the folder {}", parent.display()))?;
    }
    std::fs::write(path, bytes).with_context(|| format!("cannot write {}", path.display()))
}

/// A byte count as `12.3 MiB`.
pub fn size(bytes: usize) -> String {
    const UNITS: [&str; 4] = ["B", "KiB", "MiB", "GiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    match UNITS.get(unit) {
        Some(name) if unit > 0 => format!("{value:.1} {name}"),
        _ => format!("{bytes} B"),
    }
}

/// The runtime dependency the user chose.
///
/// Nothing is published yet, so there is no default: neither flag is a usage
/// error that says why.
///
/// # Errors
///
/// [`CliError::Usage`] when neither flag is given, or the path does not hold
/// a crate.
pub fn runtime_options(args: &RuntimeArgs) -> Result<GenOptions, CliError> {
    match (&args.runtime_path, &args.runtime_version) {
        (Some(path), None) => {
            let absolute = std::path::absolute(path)
                .map_err(|error| usage(format!("--runtime-path {}: {error}", path.display())))?;
            if !absolute.join("Cargo.toml").is_file() {
                return Err(usage(format!(
                    "--runtime-path {} has no Cargo.toml; point it at the crates/ufathom-runtime folder",
                    path.display()
                )));
            }
            Ok(GenOptions::with_runtime_path(absolute))
        }
        (None, Some(version)) => Ok(GenOptions {
            runtime: RuntimeDependency::Version(version.clone()),
            ..GenOptions::default()
        }),
        (None, None) => Err(usage(
            "the SDK needs a runtime dependency: pass --runtime-path <crates/ufathom-runtime folder> \
             or --runtime-version <version>. There is no default because ufathom-runtime is not \
             published yet",
        )),
        (Some(_), Some(_)) => Err(usage(
            "--runtime-path and --runtime-version cannot be used together",
        )),
    }
}

/// Generates the SDK crate for `dump` into `folder`.
///
/// # Errors
///
/// The generator refused its options or a file could not be written.
pub fn generate_sdk(dump: &Dump, options: &GenOptions, folder: &Path) -> Result<GenReport> {
    generate(dump, options, folder)
        .with_context(|| format!("cannot generate the SDK into {}", folder.display()))
}

/// Prints what a generation run did.
pub fn print_gen_report(report: &GenReport, folder: &Path) {
    println!(
        "SDK:           {} packages, {} classes, {} script structs, {} enums, {} functions",
        report.packages, report.classes, report.script_structs, report.enums, report.functions
    );
    println!(
        "               {} functions without a wrapper; {} items that could not be represented exactly",
        report.functions_skipped,
        report.degraded.len()
    );
    for (reason, count) in report.by_reason() {
        println!("                 {count:>6}  {reason}");
    }
    println!(
        "               {} files written, {} unchanged, {} removed in {}",
        report.files_written.len(),
        report.files_unchanged.len(),
        report.files_removed.len(),
        folder.display()
    );
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "test code may panic on failure"
)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn args(path: Option<&str>, version: Option<&str>) -> RuntimeArgs {
        RuntimeArgs {
            runtime_path: path.map(PathBuf::from),
            runtime_version: version.map(str::to_owned),
        }
    }

    #[test]
    fn neither_runtime_flag_is_a_usage_error_that_explains_why() {
        let Err(CliError::Usage(message)) = runtime_options(&args(None, None)) else {
            panic!("expected a usage error");
        };
        assert!(message.contains("--runtime-path"), "{message}");
        assert!(message.contains("--runtime-version"), "{message}");
        assert!(message.contains("not published"), "{message}");
    }

    #[test]
    fn a_version_gives_a_registry_dependency() {
        let options = runtime_options(&args(None, Some("0.1"))).unwrap();
        assert_eq!(
            options.runtime,
            RuntimeDependency::Version("0.1".to_owned())
        );
    }

    #[test]
    fn a_path_must_hold_a_crate_and_is_made_absolute() {
        let dir = tempfile::tempdir().unwrap();
        let missing = runtime_options(&args(dir.path().to_str(), None));
        assert!(matches!(missing, Err(CliError::Usage(_))));
        std::fs::write(dir.path().join("Cargo.toml"), "").unwrap();
        let options = runtime_options(&args(dir.path().to_str(), None)).unwrap();
        assert_eq!(
            options.runtime,
            RuntimeDependency::Path(dir.path().to_owned())
        );
    }

    #[test]
    fn sizes_are_readable() {
        assert_eq!(size(0), "0 B");
        assert_eq!(size(1023), "1023 B");
        assert_eq!(size(1536), "1.5 KiB");
        assert_eq!(size(5 * 1024 * 1024), "5.0 MiB");
    }

    #[test]
    fn write_file_creates_folders_and_replaces_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a/b/out.txt");
        write_file(&path, b"one").unwrap();
        write_file(&path, b"two").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"two");
    }

    #[test]
    fn a_file_that_is_not_a_dump_is_named_in_the_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("x.json");
        std::fs::write(&path, "{\"format_version\": 999}").unwrap();
        let error = read_dump(&path).unwrap_err().to_string();
        assert!(error.contains("x.json"), "{error}");
        assert!(error.contains("not a usable dump"), "{error}");
        let error = read_dump(&dir.path().join("none.json")).unwrap_err();
        assert!(format!("{error:#}").contains("cannot read"), "{error:#}");
    }
}
