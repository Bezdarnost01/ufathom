//! Writing the crate to disk, touching only files whose content changed.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};

use crate::error::CodegenError;

/// Name of the manifest file in the output directory.
pub const MANIFEST_NAME: &str = "ufathom-sdk.manifest";

const MANIFEST_HEADER: &str = "ufathom-codegen manifest 1";

/// 64-bit FNV-1a. Not cryptographic: it only has to notice that text changed.
/// It is fixed here, not taken from the standard library, so the manifest is
/// the same on every toolchain.
pub(crate) fn content_hash(text: &str) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in text.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")
}

/// A file to write, relative to the output directory, with `/` separators.
pub(crate) struct OutFile {
    pub path: String,
    pub content: String,
}

/// What [`write_files`] did.
pub(crate) struct Written {
    pub written: Vec<String>,
    pub unchanged: Vec<String>,
    pub removed: Vec<String>,
}

fn io_err(path: &Path, source: io::Error) -> CodegenError {
    CodegenError::Io {
        path: path.to_owned(),
        source,
    }
}

/// A relative path made only of normal components, or `None`.
fn safe_relative(rel: &str) -> Option<PathBuf> {
    let path = Path::new(rel);
    path.components()
        .all(|c| matches!(c, Component::Normal(_)))
        .then(|| path.to_owned())
}

fn read_manifest(dir: &Path) -> BTreeMap<String, String> {
    let Ok(text) = fs::read_to_string(dir.join(MANIFEST_NAME)) else {
        return BTreeMap::new();
    };
    let mut lines = text.lines();
    if lines.next() != Some(MANIFEST_HEADER) {
        return BTreeMap::new();
    }
    lines
        .filter_map(|line| {
            let (hash, path) = line.split_once('\t')?;
            Some((path.to_owned(), hash.to_owned()))
        })
        .collect()
}

/// Writes every file whose hash differs from the manifest or that is missing,
/// removes files of the previous run that are no longer generated, and
/// rewrites the manifest if it changed.
pub(crate) fn write_files(dir: &Path, files: &[OutFile]) -> Result<Written, CodegenError> {
    fs::create_dir_all(dir).map_err(|e| io_err(dir, e))?;
    let old = read_manifest(dir);
    let mut result = Written {
        written: Vec::new(),
        unchanged: Vec::new(),
        removed: Vec::new(),
    };
    let mut new: BTreeMap<String, String> = BTreeMap::new();
    for file in files {
        let hash = content_hash(&file.content);
        let rel = safe_relative(&file.path)
            .ok_or_else(|| CodegenError::Internal(format!("unsafe output path {}", file.path)))?;
        let full = dir.join(&rel);
        let current = full.is_file()
            && old.get(&file.path) == Some(&hash)
            && fs::metadata(&full).map(|m| m.len()).ok() == Some(file.content.len() as u64);
        if current {
            result.unchanged.push(file.path.clone());
        } else {
            if let Some(parent) = full.parent() {
                fs::create_dir_all(parent).map_err(|e| io_err(parent, e))?;
            }
            fs::write(&full, &file.content).map_err(|e| io_err(&full, e))?;
            result.written.push(file.path.clone());
        }
        new.insert(file.path.clone(), hash);
    }
    for stale in old.keys().filter(|p| !new.contains_key(*p)) {
        if let Some(rel) = safe_relative(stale) {
            let full = dir.join(rel);
            match fs::remove_file(&full) {
                Ok(()) => result.removed.push(stale.clone()),
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => return Err(io_err(&full, e)),
            }
        }
    }
    let mut manifest = format!("{MANIFEST_HEADER}\n");
    for (path, hash) in &new {
        manifest.push_str(&format!("{hash}\t{path}\n"));
    }
    let manifest_path = dir.join(MANIFEST_NAME);
    if fs::read_to_string(&manifest_path).ok().as_deref() != Some(manifest.as_str()) {
        fs::write(&manifest_path, manifest).map_err(|e| io_err(&manifest_path, e))?;
    }
    result.written.sort();
    result.unchanged.sort();
    result.removed.sort();
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(path: &str, content: &str) -> OutFile {
        OutFile {
            path: path.to_owned(),
            content: content.to_owned(),
        }
    }

    #[test]
    fn hash_is_fixed() {
        assert_eq!(content_hash(""), "cbf29ce484222325");
        assert_eq!(content_hash("a"), "af63dc4c8601ec8c");
    }

    #[test]
    fn second_run_writes_only_changed_files_and_removes_stale_ones() {
        let dir = tempfile::tempdir().unwrap();
        let first = write_files(
            dir.path(),
            &[
                file("src/a.rs", "a"),
                file("src/b.rs", "b"),
                file("Cargo.toml", "c"),
            ],
        )
        .unwrap();
        assert_eq!(first.written.len(), 3);
        let second = write_files(
            dir.path(),
            &[
                file("src/a.rs", "a"),
                file("src/b.rs", "B"),
                file("Cargo.toml", "c"),
            ],
        )
        .unwrap();
        assert_eq!(second.written, vec!["src/b.rs".to_owned()]);
        assert_eq!(second.unchanged.len(), 2);
        let third = write_files(
            dir.path(),
            &[file("src/a.rs", "a"), file("Cargo.toml", "c")],
        )
        .unwrap();
        assert_eq!(third.removed, vec!["src/b.rs".to_owned()]);
        assert!(!dir.path().join("src/b.rs").exists());
    }

    #[test]
    fn a_deleted_file_is_written_again() {
        let dir = tempfile::tempdir().unwrap();
        write_files(dir.path(), &[file("src/a.rs", "a")]).unwrap();
        fs::remove_file(dir.path().join("src/a.rs")).unwrap();
        let again = write_files(dir.path(), &[file("src/a.rs", "a")]).unwrap();
        assert_eq!(again.written, vec!["src/a.rs".to_owned()]);
    }

    #[test]
    fn unsafe_manifest_entries_are_ignored() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join(MANIFEST_NAME),
            format!("{MANIFEST_HEADER}\n0000000000000000\t../escape.rs\n"),
        )
        .unwrap();
        let r = write_files(dir.path(), &[file("a.rs", "a")]).unwrap();
        assert!(r.removed.is_empty());
    }
}
