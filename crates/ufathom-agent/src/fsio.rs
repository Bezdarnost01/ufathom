//! File operations: extended paths, atomic writes, exclusive folders.
//!
//! Every function takes a normal path and converts it with
//! [`extended_path`], so callers never think about the 260 character limit.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::SystemTime;

use crate::paths::extended_path;

/// How many numeric suffixes [`create_unique_dir`] tries before it gives up.
pub const MAX_COLLISION_SUFFIX: u32 = 9999;

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Creates a folder and its parents. Succeeds if it already exists.
///
/// # Errors
///
/// The operating system's error.
pub fn create_dir_all(path: &Path) -> io::Result<()> {
    fs::create_dir_all(extended_path(path))
}

/// Writes `bytes` to `path` so that a reader sees either the previous file or
/// the whole new one, never a part.
///
/// The bytes go to a temporary file in the same folder, are flushed to disk
/// and renamed over `path`. An existing file at `path` is replaced. The
/// temporary file is removed if anything fails.
///
/// # Errors
///
/// The operating system's error from the write or the rename.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let temp = temp_sibling(path);
    let result = write_then_rename(&temp, path, bytes);
    if result.is_err() {
        let _ = fs::remove_file(extended_path(&temp));
    }
    result
}

fn write_then_rename(temp: &Path, path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(extended_path(temp))?;
    file.write_all(bytes)?;
    file.sync_all()?;
    drop(file);
    fs::rename(extended_path(temp), extended_path(path))
}

/// A name for the temporary file of [`write_atomic`]: hidden, unique in this
/// process, and ending in `.tmp` so scanners that look for `.json` skip it.
fn temp_sibling(path: &Path) -> PathBuf {
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let unique = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    path.with_file_name(format!(".{name}.{}.{unique}.tmp", std::process::id()))
}

/// Creates the folder `parent/name`, or `parent/name-2`, `parent/name-3` and so
/// on when it exists. The creation itself is exclusive, so a folder that
/// exists is never reused. Returns the path that was created.
///
/// # Errors
///
/// The operating system's error, or [`io::ErrorKind::AlreadyExists`] after
/// [`MAX_COLLISION_SUFFIX`] suffixes.
pub fn create_unique_dir(parent: &Path, name: &str) -> io::Result<PathBuf> {
    for attempt in 1..=MAX_COLLISION_SUFFIX {
        let candidate = if attempt == 1 {
            parent.join(name)
        } else {
            parent.join(format!("{name}-{attempt}"))
        };
        match fs::create_dir(extended_path(&candidate)) {
            Ok(()) => return Ok(candidate),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        format!("{} has too many folders named {name}", parent.display()),
    ))
}

/// Moves a file into `dir`, keeping its name. If the name is taken the file
/// is named `stem-2.ext`, `stem-3.ext` and so on; nothing is overwritten.
/// Returns the new path.
///
/// # Errors
///
/// The operating system's error from the rename.
pub fn move_into(from: &Path, dir: &Path) -> io::Result<PathBuf> {
    let name = from
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let (stem, extension) = match name.rsplit_once('.') {
        Some((stem, extension)) if !stem.is_empty() => (stem.to_owned(), format!(".{extension}")),
        _ => (name.clone(), String::new()),
    };
    for attempt in 1..=MAX_COLLISION_SUFFIX {
        let target = if attempt == 1 {
            dir.join(&name)
        } else {
            dir.join(format!("{stem}-{attempt}{extension}"))
        };
        if extended_path(&target).exists() {
            continue;
        }
        return fs::rename(extended_path(from), extended_path(&target)).map(|()| target);
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        format!("{} has too many files named {name}", dir.display()),
    ))
}

/// Reads a whole file as text, refusing files over `limit` bytes.
///
/// # Errors
///
/// The operating system's error, [`io::ErrorKind::InvalidData`] for a file
/// over the limit or one that is not UTF-8.
pub fn read_text_limited(path: &Path, limit: u64) -> io::Result<String> {
    let file = File::open(extended_path(path))?;
    let mut text = String::new();
    // One byte more than the limit tells a file at the limit from one over it.
    file.take(limit.saturating_add(1))
        .read_to_string(&mut text)?;
    if text.len() as u64 > limit {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("file is larger than {limit} bytes"),
        ));
    }
    Ok(text)
}

/// A regular file found by [`list_files`].
#[derive(Debug, Clone)]
pub struct FileEntry {
    /// The file name.
    pub name: String,
    /// The path as built from the listed folder.
    pub path: PathBuf,
    /// Last modification time, when the system says.
    pub modified: Option<SystemTime>,
}

/// Lists the regular files of a folder, sorted by name. A missing folder
/// lists as empty. Entries that cannot be inspected are skipped.
///
/// # Errors
///
/// The operating system's error when the folder exists but cannot be read.
pub fn list_files(dir: &Path) -> io::Result<Vec<FileEntry>> {
    let entries = match fs::read_dir(extended_path(dir)) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    let mut files: Vec<FileEntry> = entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let metadata = entry.metadata().ok()?;
            metadata.is_file().then(|| {
                let name = entry.file_name().to_string_lossy().into_owned();
                FileEntry {
                    path: dir.join(&name),
                    name,
                    modified: metadata.modified().ok(),
                }
            })
        })
        .collect();
    files.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(files)
}

/// Removes a file. A file that is already gone is not an error.
///
/// # Errors
///
/// The operating system's error other than "not found".
pub fn remove_file(path: &Path) -> io::Result<()> {
    match fs::remove_file(extended_path(path)) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
        _ => Ok(()),
    }
}

/// Opens a file for appending, creating it and its folder if needed.
///
/// # Errors
///
/// The operating system's error.
pub fn open_append(path: &Path) -> io::Result<File> {
    if let Some(parent) = path.parent() {
        create_dir_all(parent)?;
    }
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(extended_path(path))
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
    use super::*;

    #[test]
    fn an_atomic_write_creates_the_file_and_leaves_no_temporary() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.json");
        write_atomic(&path, b"{\"a\":1}").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"{\"a\":1}");
        let names: Vec<_> = list_files(dir.path())
            .unwrap()
            .into_iter()
            .map(|entry| entry.name)
            .collect();
        assert_eq!(names, vec!["out.json"], "no temporary file may remain");
    }

    #[test]
    fn an_atomic_write_replaces_an_existing_file_whole() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("status.json");
        write_atomic(&path, &[b'a'; 10_000]).unwrap();
        write_atomic(&path, b"short").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"short");
    }

    #[test]
    fn a_failed_atomic_write_leaves_the_old_file_and_no_temporary() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("target");
        // A folder at the destination makes the final rename fail.
        fs::create_dir(&path).unwrap();
        fs::write(path.join("keep"), b"x").unwrap();
        assert!(write_atomic(&path, b"data").is_err());
        assert!(path.join("keep").exists());
        let leftovers: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .filter_map(Result::ok)
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(leftovers, vec!["target"], "temporary file was left behind");
    }

    #[test]
    fn an_atomic_write_into_a_missing_folder_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        assert!(write_atomic(&dir.path().join("no").join("x"), b"1").is_err());
    }

    #[test]
    fn concurrent_atomic_writes_never_produce_a_torn_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("status.json");
        let writers: Vec<_> = (0..4u8)
            .map(|id| {
                let path = path.clone();
                std::thread::spawn(move || {
                    for _ in 0..50 {
                        let _ = write_atomic(&path, &[b'A' + id; 4096]);
                    }
                })
            })
            .collect();
        for _ in 0..200 {
            if let Ok(bytes) = fs::read(&path) {
                assert_eq!(bytes.len(), 4096);
                assert!(bytes.iter().all(|byte| *byte == bytes[0]), "torn read");
            }
        }
        for writer in writers {
            writer.join().unwrap();
        }
    }

    #[test]
    fn unique_folders_get_numeric_suffixes_and_are_never_reused() {
        let dir = tempfile::tempdir().unwrap();
        let first = create_unique_dir(dir.path(), "dump").unwrap();
        let second = create_unique_dir(dir.path(), "dump").unwrap();
        let third = create_unique_dir(dir.path(), "dump").unwrap();
        assert_eq!(first, dir.path().join("dump"));
        assert_eq!(second, dir.path().join("dump-2"));
        assert_eq!(third, dir.path().join("dump-3"));
        fs::write(first.join("f"), b"1").unwrap();
        assert!(first.join("f").exists());
    }

    #[test]
    fn moving_never_overwrites() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("archive");
        fs::create_dir(&archive).unwrap();
        for round in 0..3u8 {
            let source = dir.path().join("cmd.json");
            fs::write(&source, [round]).unwrap();
            move_into(&source, &archive).unwrap();
            assert!(!source.exists());
        }
        assert_eq!(fs::read(archive.join("cmd.json")).unwrap(), [0]);
        assert_eq!(fs::read(archive.join("cmd-2.json")).unwrap(), [1]);
        assert_eq!(fs::read(archive.join("cmd-3.json")).unwrap(), [2]);
    }

    #[test]
    fn listing_returns_files_sorted_and_skips_folders() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("b.json"), b"1").unwrap();
        fs::write(dir.path().join("a.json"), b"1").unwrap();
        fs::create_dir(dir.path().join("sub")).unwrap();
        let names: Vec<_> = list_files(dir.path())
            .unwrap()
            .into_iter()
            .map(|entry| entry.name)
            .collect();
        assert_eq!(names, vec!["a.json", "b.json"]);
    }

    #[test]
    fn listing_a_missing_folder_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        assert!(list_files(&dir.path().join("none")).unwrap().is_empty());
    }

    #[test]
    fn reading_refuses_a_file_over_the_limit() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("big");
        fs::write(&path, vec![b'x'; 101]).unwrap();
        assert_eq!(read_text_limited(&path, 101).unwrap().len(), 101);
        let error = read_text_limited(&path, 100).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn reading_refuses_text_that_is_not_utf8() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bin");
        fs::write(&path, [0xFF, 0xFE, 0x80]).unwrap();
        assert!(read_text_limited(&path, 100).is_err());
    }

    #[test]
    fn removing_a_missing_file_is_fine() {
        let dir = tempfile::tempdir().unwrap();
        remove_file(&dir.path().join("nothing")).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn a_path_over_260_characters_can_be_written_and_read() {
        let dir = tempfile::tempdir().unwrap();
        let mut deep = dir.path().to_owned();
        while deep.as_os_str().len() < 300 {
            deep.push("a-folder-with-a-fairly-long-name");
        }
        create_dir_all(&deep).unwrap();
        let path = deep.join("status.json");
        assert!(path.as_os_str().len() > 260);
        write_atomic(&path, b"{}").unwrap();
        assert_eq!(read_text_limited(&path, 10).unwrap(), "{}");
        let unique = create_unique_dir(&deep, "dump").unwrap();
        assert!(unique.exists() || extended_path(&unique).exists());
        let listed = list_files(&deep).unwrap();
        assert_eq!(listed.len(), 1);
    }
}
