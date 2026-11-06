//! Reads and writes usmap files and compares them against a dump.
//!
//! `.usmap` is the mappings format Unreal tools use to read assets without the
//! game. Here it has two jobs: an independent oracle to check a [`Dump`]
//! against output made by another tool ([`compare`]), and an output format
//! ([`write_usmap`]).
//!
//! This crate performs no memory access and no I/O. It takes bytes and returns
//! values.
//!
//! # Reading
//!
//! [`Usmap::parse`] reads format versions 0 to 4, with the payload stored or
//! compressed with zstd. Oodle and Brotli payloads are refused with an error
//! that names the method. Files are untrusted: see [`UsmapError`],
//! [`MAX_DECOMPRESSED_SIZE`] and [`MAX_TYPE_DEPTH`]. Optional extension chunks
//! after the struct list are not interpreted.
//!
//! # Writing
//!
//! [`write_usmap`] writes **version 4** (explicit enum values), **uncompressed**,
//! with no package-version block. Version 4 is what Dumper-7 writes and keeps
//! real enum numbers; leaving the payload uncompressed is the form every
//! reader accepts, and the files are small. The output is deterministic:
//! types are written in path order, properties in `(offset, name)` order, and
//! names are numbered in order of first use.
//!
//! usmap describes data members only, so some of a dump has no place in it.
//! Dropped for every type: functions, sizes, offsets, alignment, flags,
//! packages, the class of an object property, the bit of a `bool`, the
//! signature of a delegate. Handled deliberately, per case:
//!
//! - class pointers are written as `ObjectProperty`, soft class pointers as
//!   `SoftObjectProperty`, and inline and sparse multicast delegates both as
//!   `MulticastDelegateProperty`, since the format has no type for them;
//! - a byte backed by an enum is written as an `EnumProperty` over
//!   `ByteProperty`;
//! - a property of an unknown class is written with the `Unknown` tag;
//! - a C array over 255 elements is written with dimension 255, the largest
//!   the format holds;
//! - a short name used by several types is written once, for the first path in
//!   sorted order, since the format cannot tell them apart.
//!
//! [`write_usmap_with_report`] returns a [`WriteReport`] that lists every case
//! of the last four kinds, so none is silent.
//!
//! # Comparing
//!
//! [`compare`] checks a dump against a usmap and returns an [`OracleReport`].

#![forbid(unsafe_code)]

mod build;
mod cursor;
mod decompress;
mod encode;
mod error;
mod format;
mod mapping;
mod model;
mod reader;

#[cfg(test)]
mod tests;

pub use build::WriteReport;
pub use error::UsmapError;
pub use mapping::short_name;
pub use model::{
    Compression, CustomVersion, Usmap, UsmapEnum, UsmapEnumValue, UsmapProperty, UsmapStruct,
    UsmapType, UsmapVersion, VersionInfo,
};
pub use reader::{MAX_DECOMPRESSED_SIZE, MAX_TYPE_DEPTH};

use ufathom_model::Dump;

/// Writes a dump as a usmap file.
///
/// The format is version 4, uncompressed; see the crate documentation for
/// what is lost and how. The result is deterministic for a given dump and
/// does not depend on the order of its lists. Use
/// [`write_usmap_with_report`] to learn what was changed or left out.
#[must_use]
pub fn write_usmap(dump: &Dump) -> Vec<u8> {
    write_usmap_with_report(dump).0
}

/// Writes a dump as a usmap file and reports what usmap could not hold.
///
/// Same output as [`write_usmap`].
#[must_use]
pub fn write_usmap_with_report(dump: &Dump) -> (Vec<u8>, WriteReport) {
    let (usmap, report) = build::build_usmap(dump);
    (encode::encode(&usmap), report)
}
