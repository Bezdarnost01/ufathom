//! What a caller can tell [`build_dump`](crate::build_dump), and what it returns.

use std::sync::atomic::AtomicBool;
use std::time::Duration;

use rayon::ThreadPool;
use ufathom_model::Dump;

/// Share of objects that may fail before a dump is called degraded.
pub const DEFAULT_DEGRADED_THRESHOLD: f64 = 0.05;

/// Settings for [`build_dump`](crate::build_dump).
#[derive(Debug, Clone)]
pub struct ReflectOptions<'a> {
    /// Thread pool for the parallel phases. `None` runs everything on the
    /// calling thread.
    ///
    /// The global rayon pool is never used, in either case: its threads cannot
    /// be stopped, which would block unloading the DLL.
    pub pool: Option<&'a ThreadPool>,
    /// Set from another thread to stop the dump. It is checked between
    /// objects in every phase; once set, [`build_dump`](crate::build_dump)
    /// returns [`ReflectError::Cancelled`](crate::ReflectError::Cancelled).
    pub cancel: Option<&'a AtomicBool>,
    /// The dump is degraded when more than this share of the objects failed.
    /// `0.05` means 5%.
    pub degraded_threshold: f64,
    /// Written to the dump header.
    pub tool_name: String,
    /// Written to the dump header.
    pub tool_version: String,
}

impl Default for ReflectOptions<'_> {
    fn default() -> Self {
        Self {
            pool: None,
            cancel: None,
            degraded_threshold: DEFAULT_DEGRADED_THRESHOLD,
            tool_name: "ufathom".to_owned(),
            tool_version: env!("CARGO_PKG_VERSION").to_owned(),
        }
    }
}

/// Wall time spent in each phase. Not part of the dump, which must be
/// deterministic.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PhaseTimings {
    /// Loading the name table.
    pub names: Duration,
    /// Reading every object header.
    pub headers: Duration,
    /// Classifying objects by their class chain.
    pub classify: Duration,
    /// Building the path of every type.
    pub paths: Duration,
    /// Parsing structs, classes, functions and enums, with re-validation.
    pub parse: Duration,
    /// Sorting and assembling the dump.
    pub assemble: Duration,
    /// The whole call.
    pub total: Duration,
}

/// Counts describing one pass.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ReflectStats {
    /// Occupied slots of the object array.
    pub objects: u64,
    /// Free slots of the object array.
    pub null_slots: u64,
    /// Script structs in the dump.
    pub structs: u64,
    /// Classes in the dump.
    pub classes: u64,
    /// Enums in the dump.
    pub enums: u64,
    /// Functions in the dump, over all classes.
    pub functions: u64,
    /// Objects that could not be dumped completely: skipped, or kept with
    /// missing members. Each has a diagnostic of severity `Error`. This is the
    /// numerator of the degraded check.
    pub failures: u64,
    /// Objects that were read a second time without the cache, because the
    /// first read failed or did not validate.
    pub rereads: u64,
    /// Page lookups answered by the pass's cache.
    pub cache_hits: u64,
    /// Page lookups that read the target.
    pub cache_misses: u64,
    /// Time per phase.
    pub timings: PhaseTimings,
}

/// The result of [`build_dump`](crate::build_dump).
#[derive(Debug, Clone)]
pub struct ReflectOutput {
    /// The dump, normalized, diagnostics included.
    pub dump: Dump,
    /// More than [`ReflectOptions::degraded_threshold`] of the objects failed.
    /// The dump is still complete in the sense that it holds everything that
    /// could be read; the caller decides whether to trust it.
    pub degraded: bool,
    /// What the pass did.
    pub stats: ReflectStats,
}
