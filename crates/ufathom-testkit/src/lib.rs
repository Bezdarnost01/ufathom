//! Synthetic Unreal Engine images, corruption injection and a fake host
//! process, for tests only.
//!
//! [`FixtureBuilder`] writes an in-memory image that looks like a running
//! game: a PE module with `.text`, `.rdata`, `.data` and `.rsrc`, heap regions
//! with gaps, a name table, a global object array and the reflection data of
//! a few engine classes. The image is written from an
//! [`EngineLayout`](ufathom_model::EngineLayout) alone; the expected answer
//! ([`Fixture::layout`], [`Fixture::globals`], [`Fixture::truth`]) is returned
//! beside it and never written into it.
//!
//! The same inputs always give byte-identical output.
//!
//! This crate is test infrastructure: it may panic on a mistake in its own
//! inputs, but everything it writes is shaped like target data.
//!
//! # Measured and assumed values
//!
//! See [`profile`]. Only the `*_measured` profile constructors carry measured
//! values; everything else is derived from the public type descriptions of the
//! `jmap` project and is tagged `ASSUMED`.

#![deny(unsafe_op_in_unsafe_fn)]
#![allow(
    clippy::expect_used,
    clippy::panic,
    reason = "test infrastructure: a mistake in builder inputs may panic; target-shaped data is still produced correctly"
)]

mod code;
mod corrupt;
mod data;
mod fixture;
mod names;
mod pe;
pub mod profile;
mod reflect;
mod rng;
mod schema;
mod shuffle;
mod space;

pub use corrupt::Corruption;
pub use data::{DataAnchors, ItemAnchors};
pub use fixture::{
    Anchors, ExpectedEngine, ExpectedGlobals, Fixture, FixtureBuilder, MODULE_NAME, Truth,
};
pub use names::NameAnchors;
pub use pe::{DIRECTORY_RESOURCE, HEADER_SIZE, NT_OFFSET, RT_VERSION, SECTION_NAMES};
pub use profile::{BuildOptions, EngineIdentity, PoolEntryFormat, Profile, ProfileError, Shape};
pub use reflect::{ChainOrder, ObjectRecord, Slot, SpecialNames};
pub use rng::Rng;
pub use space::{
    Addresses, DATA_CAP, DATA_RVA, HostedPlacement, PAGE, Part, PartKind, Placement, RDATA_CAP,
    RDATA_RVA, RSRC_RVA, TEXT_CAP, TEXT_RVA,
};

/// The folder Cargo puts this build's executables and libraries in, such as
/// `target/debug`, found from the running test executable.
///
/// Test executables sit in `target/<profile>/deps` with the classic layout and
/// deeper, under `target/<profile>/build/<package>/<hash>`, with the newer
/// build directory layout. The artifact folder is the nearest ancestor holding
/// Cargo's `.cargo-lock`; failing that, the parent of the nearest `build`
/// folder, then the parent of `deps`.
///
/// # Panics
///
/// When the path of the running executable cannot be read.
#[must_use]
#[allow(
    clippy::expect_used,
    reason = "test support: no executable path means no test"
)]
pub fn artifact_dir() -> std::path::PathBuf {
    let exe = std::env::current_exe().expect("path of the running test");
    let ancestors = || exe.ancestors().skip(1);
    ancestors()
        .find(|dir| dir.join(".cargo-lock").is_file())
        .or_else(|| {
            ancestors()
                .find(|dir| dir.file_name().is_some_and(|name| name == "build"))
                .and_then(std::path::Path::parent)
        })
        .or_else(|| exe.parent().and_then(std::path::Path::parent))
        .expect("the test executable has a parent folder")
        .to_owned()
}
