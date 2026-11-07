//! The control folder: `inbox/`, `outbox/`, `archive/` and `status.json`.

use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use crate::fsio::{self, FileEntry};
use crate::output::control_folder;
use crate::protocol::{CommandResult, MAX_COMMAND_BYTES, StatusDocument};

/// File name of the status document inside the control folder.
pub const STATUS_FILE: &str = "status.json";
/// Name of the folder clients drop commands into.
pub const INBOX_FOLDER: &str = "inbox";
/// Name of the folder results are published to.
pub const OUTBOX_FOLDER: &str = "outbox";
/// Name of the folder handled commands are moved to.
pub const ARCHIVE_FOLDER: &str = "archive";

/// A command file younger than this that does not parse may still be being
/// written by a client that did not use a temporary name. Reading it is
/// retried on the next poll instead of answering with an error.
pub const SETTLE_TIME: Duration = Duration::from_millis(300);

/// The paths of the control folder.
#[derive(Debug, Clone)]
pub struct ControlDir {
    root: PathBuf,
}

impl ControlDir {
    /// The control folder of an output folder.
    #[must_use]
    pub fn new(output: &Path) -> Self {
        Self {
            root: control_folder(output),
        }
    }

    /// The control folder itself.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Where commands are dropped.
    #[must_use]
    pub fn inbox(&self) -> PathBuf {
        self.root.join(INBOX_FOLDER)
    }

    /// Where results are published.
    #[must_use]
    pub fn outbox(&self) -> PathBuf {
        self.root.join(OUTBOX_FOLDER)
    }

    /// Where handled commands are moved.
    #[must_use]
    pub fn archive(&self) -> PathBuf {
        self.root.join(ARCHIVE_FOLDER)
    }

    /// The status document.
    #[must_use]
    pub fn status_path(&self) -> PathBuf {
        self.root.join(STATUS_FILE)
    }

    /// Creates the folders.
    ///
    /// # Errors
    ///
    /// The operating system's error.
    pub fn create(&self) -> io::Result<()> {
        for folder in [self.inbox(), self.outbox(), self.archive()] {
            fsio::create_dir_all(&folder)?;
        }
        Ok(())
    }

    /// Publishes the status document, atomically.
    ///
    /// # Errors
    ///
    /// The operating system's error, or a serialization error.
    pub fn write_status(&self, document: &StatusDocument) -> io::Result<()> {
        let json = serde_json::to_vec_pretty(document).map_err(io::Error::other)?;
        fsio::write_atomic(&self.status_path(), &json)
    }

    /// Publishes a result as `outbox/<name>.json`, atomically. A result of
    /// the same name is replaced: the newest answer to an id wins.
    ///
    /// # Errors
    ///
    /// The operating system's error, or a serialization error.
    pub fn publish_result(&self, name: &str, result: &CommandResult) -> io::Result<()> {
        let json = serde_json::to_vec_pretty(result).map_err(io::Error::other)?;
        fsio::write_atomic(&self.outbox().join(format!("{name}.json")), &json)
    }

    /// The command files waiting in the inbox, oldest name first. Hidden files
    /// and anything not ending in `.json` (such as a client's temporary file)
    /// are not commands.
    ///
    /// # Errors
    ///
    /// The operating system's error when the inbox exists but cannot be read.
    pub fn pending(&self) -> io::Result<Vec<FileEntry>> {
        Ok(fsio::list_files(&self.inbox())?
            .into_iter()
            .filter(|entry| !entry.name.starts_with('.') && entry.name.ends_with(".json"))
            .collect())
    }

    /// Reads a command file, refusing one over [`MAX_COMMAND_BYTES`].
    ///
    /// # Errors
    ///
    /// The operating system's error, or [`io::ErrorKind::InvalidData`] for a
    /// file that is too large or not UTF-8.
    pub fn read_command(&self, entry: &FileEntry) -> io::Result<String> {
        fsio::read_text_limited(&entry.path, MAX_COMMAND_BYTES)
    }

    /// Moves a handled command into `archive/`. A name that is taken gets a
    /// numeric suffix.
    ///
    /// # Errors
    ///
    /// The operating system's error.
    pub fn archive_command(&self, entry: &FileEntry) -> io::Result<PathBuf> {
        fsio::move_into(&entry.path, &self.archive())
    }
}

/// Whether a file has been left alone long enough that a half-written state is
/// no longer a likely explanation for it being malformed.
#[must_use]
pub fn is_settled(entry: &FileEntry, now: SystemTime) -> bool {
    entry.modified.is_none_or(|modified| {
        now.duration_since(modified)
            .map_or(true, |age| age >= SETTLE_TIME)
    })
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
    fn creating_makes_the_three_folders() {
        let dir = tempfile::tempdir().unwrap();
        let control = ControlDir::new(dir.path());
        control.create().unwrap();
        control.create().unwrap();
        assert!(control.inbox().is_dir());
        assert!(control.outbox().is_dir());
        assert!(control.archive().is_dir());
        assert_eq!(control.root(), dir.path().join("control"));
    }

    #[test]
    fn only_visible_json_files_are_commands() {
        let dir = tempfile::tempdir().unwrap();
        let control = ControlDir::new(dir.path());
        control.create().unwrap();
        for name in [
            "b.json",
            "a.json",
            ".hidden.json",
            "x.json.tmp",
            "notes.txt",
        ] {
            std::fs::write(control.inbox().join(name), b"{}").unwrap();
        }
        let names: Vec<_> = control
            .pending()
            .unwrap()
            .into_iter()
            .map(|entry| entry.name)
            .collect();
        assert_eq!(names, vec!["a.json", "b.json"]);
    }

    #[test]
    fn a_missing_inbox_has_no_commands() {
        let dir = tempfile::tempdir().unwrap();
        assert!(ControlDir::new(dir.path()).pending().unwrap().is_empty());
    }

    #[test]
    fn archiving_moves_the_file_and_keeps_earlier_archives() {
        let dir = tempfile::tempdir().unwrap();
        let control = ControlDir::new(dir.path());
        control.create().unwrap();
        for round in 0..2u8 {
            std::fs::write(control.inbox().join("c.json"), [round]).unwrap();
            let entry = control.pending().unwrap().remove(0);
            control.archive_command(&entry).unwrap();
        }
        assert!(control.pending().unwrap().is_empty());
        assert_eq!(
            std::fs::read(control.archive().join("c.json")).unwrap(),
            [0]
        );
        assert_eq!(
            std::fs::read(control.archive().join("c-2.json")).unwrap(),
            [1]
        );
    }

    #[test]
    fn a_fresh_file_is_not_settled_and_an_old_one_is() {
        let now = SystemTime::now();
        let entry = |modified| FileEntry {
            name: "x.json".to_owned(),
            path: PathBuf::from("x.json"),
            modified,
        };
        assert!(!is_settled(&entry(Some(now)), now));
        assert!(is_settled(&entry(Some(now - Duration::from_secs(5))), now));
        assert!(is_settled(&entry(None), now));
        assert!(
            is_settled(&entry(Some(now + Duration::from_secs(60))), now),
            "a modification time in the future must not stall a command forever"
        );
    }
}
