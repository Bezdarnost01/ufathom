//! Errors returned while reading a usmap file.

use thiserror::Error;

/// Why a usmap file could not be read.
///
/// A usmap file is untrusted input, so every variant describes a property of
/// the bytes, never a bug in this crate. Where a byte position is known it is
/// named `offset`. Offsets are positions in the file for the header and for an
/// uncompressed body. For a compressed body they are positions inside the
/// decompressed payload, because the compressed bytes have no useful
/// structure to point into.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum UsmapError {
    /// A read ran past the end of the data.
    #[error("unexpected end of data at offset {offset}: {needed} byte(s) needed, {available} left")]
    Truncated {
        /// Where the read started.
        offset: usize,
        /// How many bytes the read needed.
        needed: usize,
        /// How many bytes were left.
        available: usize,
    },
    /// The file does not start with the usmap magic number.
    #[error("bad magic {found:#06x} at offset 0 (a usmap file starts with 0x30c4)")]
    BadMagic {
        /// The two bytes found, read as a little-endian `u16`.
        found: u16,
    },
    /// The format version is newer than this reader knows.
    #[error("unsupported usmap version {version} at offset 2 (supported: 0 to 4)")]
    UnsupportedVersion {
        /// The version byte.
        version: u8,
    },
    /// The payload uses a compression method this crate does not implement,
    /// or one that no usmap writer is known to use.
    #[error(
        "compression method {method} ({name}) at offset {offset} is not supported; \
         only none and zstd are"
    )]
    UnsupportedCompression {
        /// The method byte.
        method: u8,
        /// `Oodle` or `Brotli` for the known methods, `unknown` otherwise.
        name: &'static str,
        /// Where the method byte sits.
        offset: usize,
    },
    /// The header says the payload is larger than the reader is willing to
    /// allocate.
    #[error("declared decompressed size {declared} exceeds the limit of {limit} bytes")]
    DecompressedSizeTooLarge {
        /// The size the header declares.
        declared: u32,
        /// [`MAX_DECOMPRESSED_SIZE`](crate::MAX_DECOMPRESSED_SIZE).
        limit: u32,
    },
    /// The header declares more payload bytes than the file holds.
    #[error(
        "header declares {declared} payload byte(s) at offset {offset}, but only {available} follow"
    )]
    PayloadTruncated {
        /// Where the payload starts.
        offset: usize,
        /// The compressed size from the header.
        declared: u32,
        /// How many bytes follow the header.
        available: usize,
    },
    /// An uncompressed file whose two size fields disagree.
    #[error(
        "uncompressed payload declares {compressed} byte(s) stored but {decompressed} byte(s) in total"
    )]
    UncompressedSizeMismatch {
        /// The compressed size field.
        compressed: u32,
        /// The decompressed size field.
        decompressed: u32,
    },
    /// The zstd stream is invalid.
    #[error("zstd payload is invalid: {message}")]
    Decompress {
        /// The decoder's description.
        message: String,
    },
    /// The zstd stream decoded to a different length than the header says.
    #[error("zstd payload decoded to {actual} byte(s), header declared {declared}")]
    DecompressedSizeMismatch {
        /// The size the header declares.
        declared: u32,
        /// The size produced. When the stream is longer than declared, this
        /// is `declared + 1`, since decoding stops at the first excess byte.
        actual: usize,
    },
    /// A count is larger than the data left could possibly hold.
    #[error("{what} count {count} at offset {offset} cannot fit in the {available} byte(s) left")]
    CountTooLarge {
        /// Which list the count belongs to.
        what: &'static str,
        /// The declared count.
        count: u64,
        /// Where the count sits.
        offset: usize,
        /// How many bytes were left after the count.
        available: usize,
    },
    /// A name index points outside the name table.
    #[error("name index {index} at offset {offset} is outside the name table of {count} name(s)")]
    NameIndexOutOfRange {
        /// The index read.
        index: u32,
        /// Where the index sits.
        offset: usize,
        /// Number of names in the table.
        count: usize,
    },
    /// A name is not valid UTF-8.
    #[error("name at offset {offset} is not valid UTF-8")]
    InvalidUtf8 {
        /// Where the name's bytes start.
        offset: usize,
    },
    /// A property type tag is not a known type.
    #[error("unknown property type tag {tag:#04x} at offset {offset}")]
    UnknownPropertyType {
        /// The tag byte.
        tag: u8,
        /// Where the tag sits.
        offset: usize,
    },
    /// Container property types are nested deeper than the limit.
    #[error("property types are nested deeper than {limit} levels at offset {offset}")]
    TypeNestingTooDeep {
        /// Where the offending tag sits.
        offset: usize,
        /// [`MAX_TYPE_DEPTH`](crate::MAX_TYPE_DEPTH).
        limit: usize,
    },
}
