//! Generates a Rust SDK from a dump.
//!
//! [`generate`] turns a [`Dump`] into one Cargo crate: `src/lib.rs`, one module
//! per package behind a Cargo feature of the same name, and a manifest. The
//! crate depends on `ufathom-runtime`.
//!
//! Every generated type is `#[repr(C, align(N))]` with explicit padding and
//! asserts its size, alignment and field offsets at compile time. What cannot
//! be represented exactly is degraded to raw bytes or left out, and listed in
//! the [`GenReport`], so the generated crate always compiles.
//!
//! The output is deterministic: the same dump gives the same bytes. A file is
//! written only when its content hash differs from the manifest of the previous
//! run.

#![forbid(unsafe_code)]

mod build;
mod emit;
mod error;
mod names;
mod output;
mod planner;
mod report;
mod types;

use std::path::{Path, PathBuf};

use ufathom_model::Dump;

pub use error::CodegenError;
pub use output::MANIFEST_NAME;
pub use report::{Degradation, DegradeReason, GenReport};

/// Where the generated crate finds `ufathom-runtime`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuntimeDependency {
    /// A path to the runtime crate.
    Path(PathBuf),
    /// A version requirement from a registry.
    Version(String),
}

/// Options for [`generate`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GenOptions {
    /// Name of the generated crate.
    pub crate_name: String,
    /// Version of the generated crate.
    pub crate_version: String,
    /// How the generated crate depends on the runtime.
    pub runtime: RuntimeDependency,
}

impl GenOptions {
    /// Options for a crate named `ufathom-sdk` that uses the runtime at `path`.
    #[must_use]
    pub fn with_runtime_path(path: impl Into<PathBuf>) -> Self {
        Self {
            crate_name: "ufathom-sdk".to_owned(),
            crate_version: "0.1.0".to_owned(),
            runtime: RuntimeDependency::Path(path.into()),
        }
    }
}

impl Default for GenOptions {
    /// A crate named `ufathom-sdk` that depends on this release of the runtime
    /// from a registry.
    fn default() -> Self {
        Self {
            crate_name: "ufathom-sdk".to_owned(),
            crate_version: "0.1.0".to_owned(),
            runtime: RuntimeDependency::Version(env!("CARGO_PKG_VERSION").to_owned()),
        }
    }
}

fn valid_crate_name(name: &str) -> bool {
    !name.is_empty()
        && name.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// Generates the SDK crate for `dump` into `out_dir`.
///
/// Existing files whose content is unchanged are left alone, and files a
/// previous run wrote that are no longer generated are removed. Only files
/// listed in the manifest are ever removed.
///
/// # Errors
///
/// - [`CodegenError::InvalidOption`] for a crate name or version Cargo would
///   reject.
/// - [`CodegenError::Io`] if a file cannot be written.
pub fn generate(
    dump: &Dump,
    options: &GenOptions,
    out_dir: &Path,
) -> Result<GenReport, CodegenError> {
    if !valid_crate_name(&options.crate_name) {
        return Err(CodegenError::InvalidOption(format!(
            "crate name {:?}",
            options.crate_name
        )));
    }
    if options.crate_version.is_empty()
        || !options
            .crate_version
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '+')
    {
        return Err(CodegenError::InvalidOption(format!(
            "crate version {:?}",
            options.crate_version
        )));
    }
    let runtime = match &options.runtime {
        RuntimeDependency::Path(path) => format!(
            "{{ path = {} }}",
            emit::toml_string(&path.to_string_lossy().replace('\\', "/"))
        ),
        RuntimeDependency::Version(v) => format!("{{ version = {} }}", emit::toml_string(v)),
    };

    let built = build::build(dump);

    let mut files = vec![
        output::OutFile {
            path: "Cargo.toml".to_owned(),
            content: emit::cargo_source(
                &built,
                &options.crate_name,
                &options.crate_version,
                &runtime,
            ),
        },
        output::OutFile {
            path: "src/lib.rs".to_owned(),
            content: emit::lib_source(&built, dump.engine.claimed.as_deref()),
        },
    ];
    for module in &built.modules {
        files.push(output::OutFile {
            path: format!("src/{}.rs", module.name),
            content: emit::module_source(&built, module),
        });
    }
    let written = output::write_files(out_dir, &files)?;

    Ok(GenReport {
        packages: built.modules.len(),
        classes: built.structs.iter().filter(|s| s.is_class).count(),
        script_structs: built.structs.iter().filter(|s| !s.is_class).count(),
        enums: built.enums.len(),
        fields: built.fields,
        functions: built.functions,
        functions_skipped: built.functions_skipped,
        degraded: built.degraded,
        files_written: written.written,
        files_unchanged: written.unchanged,
        files_removed: written.removed,
    })
}
