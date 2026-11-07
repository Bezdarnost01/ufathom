//! PE parsing, version detection and the layout solver.

#![forbid(unsafe_code)]

mod bytes;
mod error;
pub mod limits;
pub mod pe;
pub mod version;

pub use error::ScanError;
pub use pe::{DataDirectory, PeImage, Section, load_pe_file, load_pe_reader};
pub use version::{VersionFacts, detect_version};
