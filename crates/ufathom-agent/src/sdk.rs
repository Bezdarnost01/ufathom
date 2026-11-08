//! The `rust-sdk` format: a Rust crate generated from the dump.
//!
//! The crate is written into `sdk/` of the dump folder by `ufathom-codegen`,
//! on the thread that runs the dump. Matching runtime sources are included,
//! so the result builds without an ufathom checkout.

use std::path::Path;

use ufathom_codegen::{GenOptions, GenReport, generate};
use ufathom_model::Dump;

use crate::output::SDK_FOLDER;
use crate::paths::extended_path;
use crate::protocol::SdkSummary;

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
    Ok(summary(&folder, &report))
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
