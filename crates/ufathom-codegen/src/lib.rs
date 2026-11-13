//! Generates a Rust SDK from a dump.
//!
//! [`generate`] turns a [`Dump`] into one Cargo crate: `src/lib.rs`, an editable
//! `src/main.rs` shared by DLL and external-executable entry points, one module
//! per package behind a Cargo feature of the same name, and a manifest. The
//! crate depends on `ufathom-runtime`.
//!
//! Every generated type is `#[repr(C, align(N))]` with explicit padding and
//! asserts its size, alignment and field offsets at compile time. What cannot
//! be represented exactly is degraded to raw bytes or left out, and listed in
//! the [`GenReport`], so the generated crate always compiles.
//!
//! In a fresh output directory the same dump gives the same bytes. A generated
//! file is written only when its content hash differs from the manifest of the
//! previous run. An existing `main.rs` is preserved as user-owned code.

#![forbid(unsafe_code)]

mod build;
mod emit;
mod engine_desc;
mod error;
mod header;
mod names;
mod output;
mod planner;
mod report;
mod runtime_bundle;
mod scaffold;
mod types;

use std::path::{Path, PathBuf};

use ufathom_model::Dump;

pub use error::CodegenError;
pub use output::MANIFEST_NAME;
pub use report::{AlignmentRaise, Degradation, DegradeReason, GenReport, SizePadding};

/// Where the generated crate finds `ufathom-runtime`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuntimeDependency {
    /// Matching runtime sources embedded in ufathom and written under `vendor/`.
    Bundled,
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
    /// A crate named `ufathom-sdk` with the matching runtime sources included.
    fn default() -> Self {
        Self {
            crate_name: "ufathom-sdk".to_owned(),
            crate_version: "0.1.0".to_owned(),
            runtime: RuntimeDependency::Bundled,
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
    let bundled = matches!(&options.runtime, RuntimeDependency::Bundled);
    let runtime = match &options.runtime {
        RuntimeDependency::Bundled => r#"{ path = "vendor/ufathom-runtime" }"#.to_owned(),
        RuntimeDependency::Path(path) => format!(
            "{{ path = {} }}",
            emit::toml_string(&path.to_string_lossy().replace('\\', "/"))
        ),
        RuntimeDependency::Version(v) => format!("{{ version = {} }}", emit::toml_string(v)),
    };
    let workspace = if bundled {
        runtime_bundle::workspace_manifest()
    } else {
        String::new()
    };

    let built = build::build(dump);
    let sdk_crate = options.crate_name.replace('-', "_");

    let mut files = vec![
        output::OutFile {
            path: "Cargo.toml".to_owned(),
            content: emit::cargo_source(
                &built,
                &options.crate_name,
                &options.crate_version,
                &runtime,
                &workspace,
            ),
            preserve_existing: false,
        },
        output::OutFile {
            path: "src/lib.rs".to_owned(),
            content: emit::lib_source(
                &built,
                dump.engine.claimed.as_deref(),
                &sdk_crate,
                &engine_desc::lib_items(dump),
            ),
            preserve_existing: false,
        },
        output::OutFile {
            path: "src/dll.rs".to_owned(),
            content: scaffold::dll_source().to_owned(),
            preserve_existing: false,
        },
        output::OutFile {
            path: "src/main.rs".to_owned(),
            content: scaffold::main_source(&options.crate_name),
            preserve_existing: true,
        },
    ];
    for module in &built.modules {
        files.push(output::OutFile {
            path: format!("src/{}.rs", module.name),
            content: emit::module_source(&built, module),
            preserve_existing: false,
        });
    }
    if bundled {
        files.extend(runtime_bundle::files());
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
        alignments_raised: built.alignments_raised,
        sizes_padded: built.sizes_padded,
        files_written: written.written,
        files_unchanged: written.unchanged,
        files_removed: written.removed,
    })
}
