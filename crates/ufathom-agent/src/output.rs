//! Where dumps go: `<output>/<build id>/<session>/<dump label>/`.
//!
//! - The **build id** names the binary: the engine version it claims and a
//!   short hash of the executable, such as `ue4.25.1-3fa9c2d1`. Nothing about
//!   the game is invented; the folder name uses only what the executable
//!   reports, sanitized.
//! - The **session** is one load of the DLL: its start time and the process id.
//! - The **dump label** is what the caller asked for, or `dump-001`, `dump-002`
//!   and so on within the session.
//!
//! A dump folder is created exclusively. A name that is taken gets a numeric
//! suffix, so nothing is ever overwritten.

use std::io;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::clock;
use crate::fsio;
use crate::paths::sanitize_component;

/// File name of the dump in JSON form.
pub const DUMP_JSON: &str = "dump.json";
/// File name of the mappings file.
pub const MAPPINGS_USMAP: &str = "Mappings.usmap";
/// Name of the folder that holds the generated Rust SDK, inside a dump folder.
pub const SDK_FOLDER: &str = "sdk";
/// File name of the record that describes the run.
pub const IDENTITY_JSON: &str = "identity.json";
/// File name of the log of one dump.
pub const DUMP_LOG: &str = "ufathom.log";
/// Name of the folder that holds the control files, beside the build folders.
pub const CONTROL_FOLDER: &str = "control";
/// Name of the folder that holds the DLL's own session logs.
pub const LOG_FOLDER: &str = "logs";

/// Characters of the executable's hash that go into the build id.
const SHORT_HASH_LEN: usize = 8;

/// The build folder name for an executable.
///
/// `claimed_version` is the engine version the binary claims (`4.25.1`), and
/// `executable_sha256` the lower-case hex hash of the executable. Either may
/// be unknown, which the name shows as `ue-unknown` or `nohash`.
#[must_use]
pub fn build_id(claimed_version: Option<&str>, executable_sha256: Option<&str>) -> String {
    let version = claimed_version
        .map(|claimed| sanitize_component(claimed, ""))
        .filter(|clean| clean != "unnamed")
        .map_or_else(|| "ue-unknown".to_owned(), |clean| format!("ue{clean}"));
    let hash = executable_sha256
        .map(|hash| sanitize_component(hash, ""))
        .filter(|clean| clean != "unnamed")
        .map_or_else(
            || "nohash".to_owned(),
            |clean| clean.chars().take(SHORT_HASH_LEN).collect(),
        );
    format!("{version}-{hash}")
}

/// The session folder name: start time in UTC, then the process id, such as
/// `20261107-101530-p4321`. Folders of one build sort by start time.
#[must_use]
pub fn session_name(started: SystemTime, pid: u32) -> String {
    format!("{}-p{pid}", clock::compact(started))
}

/// The label of a dump: the sanitized request, or `dump-NNN` for the
/// `sequence`-th dump of the session.
#[must_use]
pub fn dump_label(requested: Option<&str>, sequence: u64) -> String {
    match requested.map(str::trim).filter(|label| !label.is_empty()) {
        Some(label) => sanitize_component(label, "dump"),
        None => format!("dump-{sequence:03}"),
    }
}

/// Creates `<output>/<build>/<session>/<label>` and returns it.
///
/// The build and session folders may already exist. The label folder is
/// created exclusively; if its name is taken the folder is named
/// `<label>-2`, `<label>-3` and so on.
///
/// # Errors
///
/// The operating system's error from creating a folder.
pub fn create_dump_folder(
    output: &Path,
    build: &str,
    session: &str,
    label: &str,
) -> io::Result<PathBuf> {
    let session_folder = output.join(build).join(session);
    fsio::create_dir_all(&session_folder)?;
    fsio::create_unique_dir(&session_folder, label)
}

/// The control folder under the output folder.
#[must_use]
pub fn control_folder(output: &Path) -> PathBuf {
    output.join(CONTROL_FOLDER)
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
    use std::time::{Duration, UNIX_EPOCH};

    use super::*;

    const HASH: &str = "3fa9c2d1aabbccddeeff00112233445566778899aabbccddeeff001122334455";

    #[test]
    fn the_build_id_has_the_version_and_a_short_hash() {
        assert_eq!(build_id(Some("4.25.1"), Some(HASH)), "ue4.25.1-3fa9c2d1");
    }

    #[test]
    fn an_unknown_version_or_hash_is_named_as_such() {
        assert_eq!(build_id(None, Some(HASH)), "ue-unknown-3fa9c2d1");
        assert_eq!(build_id(Some("4.22.3"), None), "ue4.22.3-nohash");
        assert_eq!(build_id(None, None), "ue-unknown-nohash");
        assert_eq!(build_id(Some(""), Some("")), "ue-unknown-nohash");
    }

    #[test]
    fn what_the_binary_reports_cannot_escape_the_folder() {
        let id = build_id(Some("..\\..\\4.25 beta/x"), Some("../../x"));
        assert!(
            !id.contains('/') && !id.contains('\\') && !id.contains(' '),
            "{id}"
        );
        assert!(!id.starts_with('.'), "{id}");
    }

    #[test]
    fn the_session_name_sorts_by_time_and_names_the_process() {
        let early = session_name(UNIX_EPOCH + Duration::from_secs(1_794_046_530), 4321);
        let late = session_name(UNIX_EPOCH + Duration::from_secs(1_794_046_531), 7);
        assert_eq!(early, "20261107-101530-p4321");
        assert!(early < late);
    }

    #[test]
    fn a_requested_label_is_sanitized_and_a_missing_one_is_numbered() {
        assert_eq!(dump_label(Some("main menu"), 1), "main_menu");
        assert_eq!(dump_label(Some("../x"), 1), "_x");
        assert_eq!(dump_label(None, 7), "dump-007");
        assert_eq!(dump_label(Some("   "), 12), "dump-012");
        assert_eq!(dump_label(Some("CON"), 1), "_CON");
    }

    #[test]
    fn dump_folders_nest_build_session_label() {
        let dir = tempfile::tempdir().unwrap();
        let folder = create_dump_folder(
            dir.path(),
            "ue4.25.1-aabbccdd",
            "20261107-101530-p1",
            "menu",
        )
        .unwrap();
        assert_eq!(
            folder,
            dir.path()
                .join("ue4.25.1-aabbccdd")
                .join("20261107-101530-p1")
                .join("menu")
        );
        assert!(folder.is_dir());
    }

    #[test]
    fn a_taken_label_gets_a_numeric_suffix_and_the_first_folder_is_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let first = create_dump_folder(dir.path(), "b", "s", "menu").unwrap();
        std::fs::write(first.join("dump.json"), b"original").unwrap();
        let second = create_dump_folder(dir.path(), "b", "s", "menu").unwrap();
        let third = create_dump_folder(dir.path(), "b", "s", "menu").unwrap();
        assert_eq!(second.file_name().unwrap(), "menu-2");
        assert_eq!(third.file_name().unwrap(), "menu-3");
        assert_eq!(std::fs::read(first.join("dump.json")).unwrap(), b"original");
    }

    #[test]
    fn other_sessions_and_builds_do_not_collide() {
        let dir = tempfile::tempdir().unwrap();
        let a = create_dump_folder(dir.path(), "b1", "s1", "x").unwrap();
        let b = create_dump_folder(dir.path(), "b1", "s2", "x").unwrap();
        let c = create_dump_folder(dir.path(), "b2", "s1", "x").unwrap();
        assert_eq!(a.file_name().unwrap(), "x");
        assert_eq!(b.file_name().unwrap(), "x");
        assert_eq!(c.file_name().unwrap(), "x");
    }
}
