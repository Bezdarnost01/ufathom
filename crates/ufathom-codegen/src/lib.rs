//! Generates a Rust SDK from a dump.
//!
//! [`generate`] turns a [`Dump`] into a Cargo crate: `src/lib.rs`, an editable
//! `src/main.rs` shared by DLL and external-executable entry points, one module
//! per package behind a Cargo feature of the same name, and a manifest. A
//! `.cargo/config.toml` links the C runtime statically on Windows. The crate
//! depends on `ufathom-runtime`.
//!
//! With [`SdkLayout::Split`] the packages are not modules of that crate but
//! live in member crates under `crates/`, which the crate re-exports under the
//! same module paths. Your own code then builds alone when it changes.
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
mod proxy;
mod report;
mod runtime_bundle;
mod scaffold;
mod split;
mod types;

use std::path::{Path, PathBuf};

use ufathom_model::Dump;

pub use error::CodegenError;
pub use output::MANIFEST_NAME;
pub use report::{AlignmentRaise, CrateInfo, Degradation, DegradeReason, GenReport, SizePadding};

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

/// How the SDK is laid out on disk.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SdkLayout {
    /// One crate with one module per package.
    Single,
    /// A small Cargo workspace: member crates under `crates/` hold the
    /// packages, and the root crate re-exports them under the same module
    /// paths and holds the engine description, the entry points and your
    /// `src/main.rs`. Packages that reference each other cyclically share a
    /// crate. The root crate builds alone when only your code changed.
    #[default]
    Split,
}

impl SdkLayout {
    /// The name used on the command line.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Single => "single",
            Self::Split => "split",
        }
    }
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
    /// How the SDK is laid out.
    pub layout: SdkLayout,
    /// With [`SdkLayout::Split`]: the size, in bytes of generated source, at
    /// which a member crate is closed. A package of at least this size gets a
    /// crate of its own; smaller packages of equal depth are packed together
    /// until a crate reaches it. `0` gives every package (or group of
    /// cyclically dependent packages) its own crate.
    pub split_crate_bytes: usize,
}

impl GenOptions {
    /// Options for a crate named `ufathom-sdk` that uses the runtime at `path`.
    #[must_use]
    pub fn with_runtime_path(path: impl Into<PathBuf>) -> Self {
        Self {
            runtime: RuntimeDependency::Path(path.into()),
            ..Self::default()
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
            layout: SdkLayout::default(),
            split_crate_bytes: split::DEFAULT_CRATE_BYTES,
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
    let runtime_value = |bundled_path: &str| match &options.runtime {
        RuntimeDependency::Bundled => format!("{{ path = {} }}", emit::toml_string(bundled_path)),
        RuntimeDependency::Path(path) => format!(
            "{{ path = {} }}",
            emit::toml_string(&path.to_string_lossy().replace('\\', "/"))
        ),
        RuntimeDependency::Version(v) => format!("{{ version = {} }}", emit::toml_string(v)),
    };
    let runtime = runtime_value("vendor/ufathom-runtime");
    let workspace = if bundled {
        runtime_bundle::workspace_manifest()
    } else {
        String::new()
    };

    let built = build::build(dump);
    let sdk_crate = options.crate_name.replace('-', "_");
    let module_sources: Vec<String> = built
        .modules
        .iter()
        .map(|module| emit::module_source(&built, module))
        .collect();

    let members = match options.layout {
        SdkLayout::Single => Vec::new(),
        SdkLayout::Split => {
            let weights: Vec<usize> = module_sources.iter().map(String::len).collect();
            split::plan_crates(
                &built.modules,
                &weights,
                options.split_crate_bytes,
                &options.crate_name,
                &[&options.crate_name, "ufathom-runtime", "ufathom-mem"],
            )
        }
    };
    let exports = split::exports(&built, &members);
    let split_layout = options.layout == SdkLayout::Split;

    let mut files = vec![
        output::OutFile {
            path: "Cargo.toml".to_owned(),
            content: emit::cargo_source(
                &built,
                &options.crate_name,
                &options.crate_version,
                &runtime,
                &workspace,
                split_layout.then_some((members.as_slice(), &exports)),
            ),
            preserve_existing: false,
        },
        output::OutFile {
            path: ".cargo/config.toml".to_owned(),
            content: emit::cargo_config_source().to_owned(),
            preserve_existing: false,
        },
        output::OutFile {
            path: "src/lib.rs".to_owned(),
            content: emit::lib_source(
                &built,
                dump.engine.claimed.as_deref(),
                &sdk_crate,
                &engine_desc::lib_items(dump, &built),
                split_layout.then_some(&exports),
            ),
            preserve_existing: false,
        },
        output::OutFile {
            path: "src/dll.rs".to_owned(),
            content: scaffold::dll_source(),
            preserve_existing: false,
        },
        output::OutFile {
            path: "build.rs".to_owned(),
            content: proxy::build_source(),
            preserve_existing: false,
        },
        output::OutFile {
            path: "src/main.rs".to_owned(),
            content: scaffold::main_source(&options.crate_name),
            preserve_existing: true,
        },
    ];
    if split_layout {
        let member_runtime = runtime_value("../../vendor/ufathom-runtime");
        for group in &members {
            let dir = group.dir();
            files.push(output::OutFile {
                path: format!("{dir}/Cargo.toml"),
                content: split::member_manifest(
                    group,
                    &members,
                    &options.crate_version,
                    &member_runtime,
                ),
                preserve_existing: false,
            });
            files.push(output::OutFile {
                path: format!("{dir}/src/lib.rs"),
                content: split::member_lib_source(
                    &built,
                    group,
                    &members,
                    &built.fname.alias_target,
                ),
                preserve_existing: false,
            });
            for &index in &group.modules {
                let (Some(module), Some(source)) =
                    (built.modules.get(index), module_sources.get(index))
                else {
                    continue;
                };
                files.push(output::OutFile {
                    path: format!("{dir}/src/{}.rs", module.name),
                    content: source.clone(),
                    preserve_existing: false,
                });
            }
        }
    } else {
        for (module, source) in built.modules.iter().zip(module_sources) {
            files.push(output::OutFile {
                path: format!("src/{}.rs", module.name),
                content: source,
                preserve_existing: false,
            });
        }
    }
    files.extend(proxy::def_files());
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
        crates: members
            .iter()
            .map(|group| CrateInfo {
                name: group.name.clone(),
                packages: group
                    .modules
                    .iter()
                    .filter_map(|m| built.modules.get(*m))
                    .map(|m| m.package.clone())
                    .collect(),
                dependencies: group
                    .deps
                    .iter()
                    .filter_map(|d| members.get(*d))
                    .map(|d| d.name.clone())
                    .collect(),
            })
            .collect(),
    })
}
