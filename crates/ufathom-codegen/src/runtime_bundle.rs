//! The runtime sources embedded in every generator binary.
//!
//! A released CLI or DLL cannot rely on a source checkout. Keeping the bundle
//! here makes the generated SDK independent of one while still compiling the
//! exact runtime sources from this ufathom build.

use crate::output::OutFile;

/// Workspace metadata appended to the generated crate's own `[workspace]`.
///
/// The vendored crates keep their original manifests, including `*.workspace`
/// inheritance. Their path dependencies resolve from the generated SDK root.
pub(crate) fn workspace_manifest() -> String {
    format!(
        r#"resolver = "3"
members = [".", "vendor/ufathom-mem", "vendor/ufathom-runtime"]
default-members = ["."]

[workspace.package]
version = "{}"
edition = "2024"
rust-version = "1.85"
license = "MIT OR Apache-2.0"
repository = "https://github.com/Bezdarnost01/ufathom"

[workspace.dependencies]
ufathom-mem = {{ path = "vendor/ufathom-mem" }}
iced-x86 = {{ version = "1.21.0", default-features = false, features = ["std", "decoder", "block_encoder", "instr_info"] }}
libc = "0.2.191"
memmap2 = "0.9.12"
tempfile = "3.27.0"
thiserror = "2.0.21"
windows-sys = "0.61.2"

[workspace.lints.rust]
missing_docs = "warn"
unsafe_op_in_unsafe_fn = "deny"

[workspace.lints.clippy]
allow_attributes_without_reason = "warn"
dbg_macro = "warn"
expect_used = "warn"
indexing_slicing = "warn"
panic = "warn"
print_stderr = "warn"
print_stdout = "warn"
todo = "warn"
unimplemented = "warn"
unwrap_used = "warn"
"#,
        env!("CARGO_PKG_VERSION")
    )
}

/// Every source and license file needed by the default runtime dependency.
pub(crate) fn files() -> Vec<OutFile> {
    EMBEDDED
        .iter()
        .map(|(path, content)| OutFile {
            path: (*path).to_owned(),
            content: (*content).to_owned(),
            preserve_existing: false,
        })
        .collect()
}

static EMBEDDED: &[(&str, &str)] = &[
    ("LICENSE-MIT", include_str!("../../../LICENSE-MIT")),
    ("LICENSE-APACHE", include_str!("../../../LICENSE-APACHE")),
    (
        "vendor/ufathom-runtime/Cargo.toml",
        include_str!("../../ufathom-runtime/Cargo.toml"),
    ),
    (
        "vendor/ufathom-runtime/src/lib.rs",
        include_str!("../../ufathom-runtime/src/lib.rs"),
    ),
    (
        "vendor/ufathom-runtime/src/containers.rs",
        include_str!("../../ufathom-runtime/src/containers.rs"),
    ),
    (
        "vendor/ufathom-runtime/src/desc.rs",
        include_str!("../../ufathom-runtime/src/desc.rs"),
    ),
    (
        "vendor/ufathom-runtime/src/engine_mem.rs",
        include_str!("../../ufathom-runtime/src/engine_mem.rs"),
    ),
    (
        "vendor/ufathom-runtime/src/inproc/mod.rs",
        include_str!("../../ufathom-runtime/src/inproc/mod.rs"),
    ),
    (
        "vendor/ufathom-runtime/src/inproc/guard.rs",
        include_str!("../../ufathom-runtime/src/inproc/guard.rs"),
    ),
    (
        "vendor/ufathom-runtime/src/inproc/hook.rs",
        include_str!("../../ufathom-runtime/src/inproc/hook.rs"),
    ),
    (
        "vendor/ufathom-runtime/src/inproc/modload.rs",
        include_str!("../../ufathom-runtime/src/inproc/modload.rs"),
    ),
    (
        "vendor/ufathom-runtime/src/inproc/mode.rs",
        include_str!("../../ufathom-runtime/src/inproc/mode.rs"),
    ),
    (
        "vendor/ufathom-runtime/src/inproc/native.rs",
        include_str!("../../ufathom-runtime/src/inproc/native.rs"),
    ),
    (
        "vendor/ufathom-runtime/src/inproc/os.rs",
        include_str!("../../ufathom-runtime/src/inproc/os.rs"),
    ),
    (
        "vendor/ufathom-runtime/src/inproc/patch.rs",
        include_str!("../../ufathom-runtime/src/inproc/patch.rs"),
    ),
    (
        "vendor/ufathom-runtime/src/detour/analyze.rs",
        include_str!("../../ufathom-runtime/src/detour/analyze.rs"),
    ),
    (
        "vendor/ufathom-runtime/src/detour/error.rs",
        include_str!("../../ufathom-runtime/src/detour/error.rs"),
    ),
    (
        "vendor/ufathom-runtime/src/detour/image.rs",
        include_str!("../../ufathom-runtime/src/detour/image.rs"),
    ),
    (
        "vendor/ufathom-runtime/src/detour/inflight.rs",
        include_str!("../../ufathom-runtime/src/detour/inflight.rs"),
    ),
    (
        "vendor/ufathom-runtime/src/detour/memory.rs",
        include_str!("../../ufathom-runtime/src/detour/memory.rs"),
    ),
    (
        "vendor/ufathom-runtime/src/detour/mod.rs",
        include_str!("../../ufathom-runtime/src/detour/mod.rs"),
    ),
    (
        "vendor/ufathom-runtime/src/detour/signature.rs",
        include_str!("../../ufathom-runtime/src/detour/signature.rs"),
    ),
    (
        "vendor/ufathom-runtime/src/detour/threads.rs",
        include_str!("../../ufathom-runtime/src/detour/threads.rs"),
    ),
    (
        "vendor/ufathom-runtime/src/inproc/typed.rs",
        include_str!("../../ufathom-runtime/src/inproc/typed.rs"),
    ),
    (
        "vendor/ufathom-runtime/src/inproc/unified.rs",
        include_str!("../../ufathom-runtime/src/inproc/unified.rs"),
    ),
    (
        "vendor/ufathom-runtime/src/session.rs",
        include_str!("../../ufathom-runtime/src/session.rs"),
    ),
    (
        "vendor/ufathom-runtime/src/executor.rs",
        include_str!("../../ufathom-runtime/src/executor.rs"),
    ),
    (
        "vendor/ufathom-runtime/src/name.rs",
        include_str!("../../ufathom-runtime/src/name.rs"),
    ),
    (
        "vendor/ufathom-runtime/src/object.rs",
        include_str!("../../ufathom-runtime/src/object.rs"),
    ),
    (
        "vendor/ufathom-runtime/src/owned.rs",
        include_str!("../../ufathom-runtime/src/owned.rs"),
    ),
    (
        "vendor/ufathom-runtime/src/proxy/mod.rs",
        include_str!("../../ufathom-runtime/src/proxy/mod.rs"),
    ),
    (
        "vendor/ufathom-runtime/src/proxy/resolve.rs",
        include_str!("../../ufathom-runtime/src/proxy/resolve.rs"),
    ),
    (
        "vendor/ufathom-runtime/src/proxy/stubs.rs",
        include_str!("../../ufathom-runtime/src/proxy/stubs.rs"),
    ),
    (
        "vendor/ufathom-runtime/src/proxy/tables.rs",
        include_str!("../../ufathom-runtime/src/proxy/tables.rs"),
    ),
    (
        "vendor/ufathom-runtime/src/ptr.rs",
        include_str!("../../ufathom-runtime/src/ptr.rs"),
    ),
    (
        "vendor/ufathom-runtime/src/traits.rs",
        include_str!("../../ufathom-runtime/src/traits.rs"),
    ),
    (
        "vendor/ufathom-runtime/src/ubool.rs",
        include_str!("../../ufathom-runtime/src/ubool.rs"),
    ),
    (
        "vendor/ufathom-runtime/src/write.rs",
        include_str!("../../ufathom-runtime/src/write.rs"),
    ),
    (
        "vendor/ufathom-mem/Cargo.toml",
        include_str!("../../ufathom-mem/Cargo.toml"),
    ),
    (
        "vendor/ufathom-mem/src/lib.rs",
        include_str!("../../ufathom-mem/src/lib.rs"),
    ),
    (
        "vendor/ufathom-mem/src/cache.rs",
        include_str!("../../ufathom-mem/src/cache.rs"),
    ),
    (
        "vendor/ufathom-mem/src/error.rs",
        include_str!("../../ufathom-mem/src/error.rs"),
    ),
    (
        "vendor/ufathom-mem/src/live.rs",
        include_str!("../../ufathom-mem/src/live.rs"),
    ),
    (
        "vendor/ufathom-mem/src/maps.rs",
        include_str!("../../ufathom-mem/src/maps.rs"),
    ),
    (
        "vendor/ufathom-mem/src/pod.rs",
        include_str!("../../ufathom-mem/src/pod.rs"),
    ),
    (
        "vendor/ufathom-mem/src/protect.rs",
        include_str!("../../ufathom-mem/src/protect.rs"),
    ),
    (
        "vendor/ufathom-mem/src/reader.rs",
        include_str!("../../ufathom-mem/src/reader.rs"),
    ),
    (
        "vendor/ufathom-mem/src/snapshot.rs",
        include_str!("../../ufathom-mem/src/snapshot.rs"),
    ),
    (
        "vendor/ufathom-mem/src/types.rs",
        include_str!("../../ufathom-mem/src/types.rs"),
    ),
    (
        "vendor/ufathom-mem/src/minidump/mod.rs",
        include_str!("../../ufathom-mem/src/minidump/mod.rs"),
    ),
    (
        "vendor/ufathom-mem/src/minidump/bytes.rs",
        include_str!("../../ufathom-mem/src/minidump/bytes.rs"),
    ),
    (
        "vendor/ufathom-mem/src/minidump/parse.rs",
        include_str!("../../ufathom-mem/src/minidump/parse.rs"),
    ),
    (
        "vendor/ufathom-mem/src/minidump/tests.rs",
        include_str!("../../ufathom-mem/src/minidump/tests.rs"),
    ),
    (
        "vendor/ufathom-mem/src/sys/mod.rs",
        include_str!("../../ufathom-mem/src/sys/mod.rs"),
    ),
    (
        "vendor/ufathom-mem/src/sys/windows.rs",
        include_str!("../../ufathom-mem/src/sys/windows.rs"),
    ),
    (
        "vendor/ufathom-mem/src/sys/linux.rs",
        include_str!("../../ufathom-mem/src/sys/linux.rs"),
    ),
    (
        "vendor/ufathom-mem/src/sys/unsupported.rs",
        include_str!("../../ufathom-mem/src/sys/unsupported.rs"),
    ),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workspace_metadata_stays_in_sync_with_the_real_workspace() {
        let root = include_str!("../../../Cargo.toml");
        for dependency in [
            r#"iced-x86 = { version = "1.21.0", default-features = false, features = ["std", "decoder", "block_encoder", "instr_info"] }"#,
            r#"libc = "0.2.191""#,
            r#"memmap2 = "0.9.12""#,
            r#"thiserror = "2.0.21""#,
            r#"windows-sys = "0.61.2""#,
            r#"tempfile = "3.27.0""#,
        ] {
            assert!(root.contains(dependency), "missing {dependency}");
            assert!(
                workspace_manifest().contains(dependency),
                "bundle is out of sync: {dependency}"
            );
        }
        let mem_manifest = include_str!("../../ufathom-mem/Cargo.toml");
        assert!(mem_manifest.contains("tempfile = { workspace = true }"));
    }

    #[test]
    fn every_runtime_and_mem_source_file_is_embedded() {
        fn sources(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
            for entry in std::fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    sources(&path, out);
                } else if path.extension().is_some_and(|e| e == "rs") {
                    out.push(path);
                }
            }
        }
        let crates = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        for name in ["ufathom-runtime", "ufathom-mem"] {
            let src = crates.join(name).join("src");
            let mut files = Vec::new();
            sources(&src, &mut files);
            assert!(!files.is_empty());
            for file in files {
                let relative = file.strip_prefix(&src).unwrap().to_string_lossy();
                let wanted = format!("vendor/{name}/src/{}", relative.replace('\\', "/"));
                assert!(
                    EMBEDDED.iter().any(|(path, _)| *path == wanted),
                    "{wanted} is not bundled into generated SDKs"
                );
            }
        }
    }

    #[test]
    fn every_embedded_path_is_safe_and_nonempty() {
        for (path, content) in EMBEDDED {
            assert!(!path.is_empty());
            assert!(!path.starts_with(['/', '\\']));
            assert!(!path.contains(".."));
            assert!(!content.is_empty(), "{path} is empty");
        }
    }
}
