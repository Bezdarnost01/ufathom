//! SHA-256 of a file, read in chunks so a cancel is honoured quickly.

use std::fmt::Write as _;
use std::fs::File;
use std::io::{self, Read};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use sha2::{Digest, Sha256};

use crate::paths::extended_path;

/// Bytes read per step. Large enough to be efficient, small enough that a
/// cancel is noticed within a few milliseconds.
pub const CHUNK_SIZE: usize = 1 << 20;

/// Why a file could not be hashed.
#[derive(Debug)]
pub enum HashError {
    /// The cancel flag was set while reading.
    Cancelled,
    /// The file could not be opened or read.
    Io(io::Error),
}

/// Lower-case hex of the SHA-256 of the file at `path`.
///
/// Reads in chunks of [`CHUNK_SIZE`] and checks `cancel` before each one.
///
/// # Errors
///
/// [`HashError::Cancelled`] when `cancel` was set, [`HashError::Io`] when the
/// file cannot be read.
pub fn sha256_file(path: &Path, cancel: &AtomicBool) -> Result<String, HashError> {
    let mut file = File::open(extended_path(path)).map_err(HashError::Io)?;
    let mut hasher = Sha256::new();
    let mut chunk = vec![0_u8; CHUNK_SIZE];
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err(HashError::Cancelled);
        }
        let read = match file.read(&mut chunk) {
            Ok(0) => break,
            Ok(read) => read,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(HashError::Io(error)),
        };
        if let Some(filled) = chunk.get(..read) {
            hasher.update(filled);
        }
    }
    Ok(hex(&hasher.finalize()))
}

/// Lower-case hex of bytes.
fn hex(bytes: &[u8]) -> String {
    bytes.iter().fold(String::new(), |mut text, byte| {
        let _ = write!(text, "{byte:02x}");
        text
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
    fn known_digests() {
        let dir = tempfile::tempdir().unwrap();
        let empty = dir.path().join("empty");
        std::fs::write(&empty, b"").unwrap();
        let cancel = AtomicBool::new(false);
        assert_eq!(
            sha256_file(&empty, &cancel).unwrap(),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        let abc = dir.path().join("abc");
        std::fs::write(&abc, b"abc").unwrap();
        assert_eq!(
            sha256_file(&abc, &cancel).unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn a_file_larger_than_one_chunk_hashes_like_the_whole() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("big");
        let bytes: Vec<u8> = (0..CHUNK_SIZE * 2 + 17)
            .map(|index| u8::try_from(index % 251).unwrap())
            .collect();
        std::fs::write(&path, &bytes).unwrap();
        let expected = hex(&Sha256::digest(&bytes));
        assert_eq!(
            sha256_file(&path, &AtomicBool::new(false)).unwrap(),
            expected
        );
    }

    #[test]
    fn a_set_cancel_flag_stops_before_reading() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f");
        std::fs::write(&path, b"data").unwrap();
        assert!(matches!(
            sha256_file(&path, &AtomicBool::new(true)),
            Err(HashError::Cancelled)
        ));
    }

    #[test]
    fn a_missing_file_is_an_io_error() {
        let dir = tempfile::tempdir().unwrap();
        assert!(matches!(
            sha256_file(&dir.path().join("none"), &AtomicBool::new(false)),
            Err(HashError::Io(_))
        ));
    }
}
