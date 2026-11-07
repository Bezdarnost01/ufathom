//! PE parsing, version detection and the layout solver.

#![forbid(unsafe_code)]

mod bytes;
mod ctx;
mod error;
mod inspect;
pub mod limits;
mod members;
mod names;
mod objects;
mod options;
pub mod pe;
mod props;
mod result;
mod structs;
mod universe;
mod uobject;
pub mod version;

pub use error::ScanError;
pub use inspect::{ExeFacts, PackingHints, inspect_exe};
pub use options::{DiscoverOptions, LayoutOverrides};
pub use pe::{DataDirectory, PeImage, Section, load_pe_file, load_pe_reader};
pub use result::{Discovery, Evidence, Globals};
pub use version::{VersionFacts, detect_version};
