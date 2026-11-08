//! The `rust-sdk` format: a Rust crate generated from the dump.
//!
//! The crate is written into `sdk/` of the dump folder by `ufathom-codegen`,
//! on the thread that runs the dump. Its runtime dependency is a version
//! requirement, because the manifest has to name something that builds
//! anywhere; a comment above it shows how to point at a local checkout
//! instead.

use std::path::Path;

use ufathom_codegen::{GenOptions, GenReport, generate};
use ufathom_model::Dump;

use crate::fsio;
use crate::output::SDK_FOLDER;
use crate::paths::extended_path;
use crate::protocol::SdkSummary;

/// What the runtime dependency line of the generated manifest starts with.
const RUNTIME_DEPENDENCY: &str = "ufathom-runtime = ";

/// Two comment lines put above the runtime dependency of the generated
/// manifest.
const RUNTIME_HINT: &str = "# To build against a local checkout of ufathom instead of a release,\n# replace the line below with: ufathom-runtime = { path = \"<path to crates/ufathom-runtime>\" }\n";

/// Generates the SDK for `dump` into `<dump_folder>/sdk`.
///
/// # Errors
///
/// The reason as text, when a file cannot be written or the generator refuses
/// its options.
pub(crate) fn write_sdk(dump: &Dump, dump_folder: &Path) -> Result<SdkSummary, String> {
    let folder = dump_folder.join(SDK_FOLDER);
    let report = generate(dump, &GenOptions::default(), &extended_path(&folder))
        .map_err(|error| format!("cannot generate the Rust SDK: {error}"))?;
    add_runtime_hint(&folder)?;
    Ok(summary(&folder, &report))
}

/// Puts [`RUNTIME_HINT`] above the runtime dependency of the manifest.
fn add_runtime_hint(folder: &Path) -> Result<(), String> {
    let manifest = folder.join("Cargo.toml");
    let text = std::fs::read_to_string(extended_path(&manifest))
        .map_err(|error| format!("cannot read the SDK manifest: {error}"))?;
    let Some(at) = text
        .lines()
        .position(|line| line.starts_with(RUNTIME_DEPENDENCY))
    else {
        return Err(format!(
            "the SDK manifest has no `{}` line to annotate",
            RUNTIME_DEPENDENCY.trim_end_matches([' ', '='])
        ));
    };
    let mut annotated = String::with_capacity(text.len() + RUNTIME_HINT.len());
    for (index, line) in text.lines().enumerate() {
        if index == at {
            annotated.push_str(RUNTIME_HINT);
        }
        annotated.push_str(line);
        annotated.push('\n');
    }
    fsio::write_atomic(&manifest, annotated.as_bytes())
        .map_err(|error| format!("cannot write the SDK manifest: {error}"))
}

fn summary(folder: &Path, report: &GenReport) -> SdkSummary {
    SdkSummary {
        folder: folder.display().to_string(),
        packages: count(report.packages),
        classes: count(report.classes),
        script_structs: count(report.script_structs),
        enums: count(report.enums),
        functions: count(report.functions),
        functions_skipped: count(report.functions_skipped),
        degraded: count(report.degraded.len()),
        files: count(report.files_written.len() + report.files_unchanged.len()),
    }
}

fn count(value: usize) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}
