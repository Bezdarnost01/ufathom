//! State shared by every phase of one pass.

use std::sync::atomic::{AtomicBool, Ordering};

use ufathom_mem::{Addr, MemoryReader};
use ufathom_model::{Diagnostic, EngineLayout, Severity};

use crate::error::ReflectError;
use crate::par::Exec;

/// What every phase reads with.
pub(crate) struct Ctx<'a> {
    /// The target, uncached. Used to re-read and to re-validate: a cached page
    /// would repeat what the first read saw.
    pub(crate) raw: &'a dyn MemoryReader,
    /// The target through the cache of this pass.
    pub(crate) cached: &'a dyn MemoryReader,
    pub(crate) layout: &'a EngineLayout,
    pub(crate) exec: Exec<'a>,
    pub(crate) cancel: Option<&'a AtomicBool>,
}

impl Ctx<'_> {
    pub(crate) fn cancelled(&self) -> bool {
        self.cancel.is_some_and(|flag| flag.load(Ordering::Relaxed))
    }

    pub(crate) fn check(&self) -> Result<(), ReflectError> {
        if self.cancelled() {
            Err(ReflectError::Cancelled)
        } else {
            Ok(())
        }
    }
}

/// Diagnostics and counters collected by a phase.
#[derive(Debug, Default)]
pub(crate) struct Tally {
    pub(crate) diagnostics: Vec<Diagnostic>,
    pub(crate) failures: u64,
    pub(crate) rereads: u64,
}

impl Tally {
    pub(crate) fn merge(&mut self, other: Self) {
        self.diagnostics.extend(other.diagnostics);
        self.failures += other.failures;
        self.rereads += other.rereads;
    }

    /// Records a problem that leaves an object skipped or incomplete.
    pub(crate) fn failure(&mut self, path: String, message: String) {
        self.failures += 1;
        self.diagnostics.push(Diagnostic {
            path,
            severity: Severity::Error,
            message,
        });
    }
}

/// How a diagnostic names an object whose path cannot be trusted or built.
pub(crate) fn describe(index: u32, addr: Addr) -> String {
    format!("<slot {index} at {addr:#x}>")
}

/// A diagnostic of the given severity.
pub(crate) fn diagnostic(path: &str, severity: Severity, message: String) -> Diagnostic {
    Diagnostic {
        path: path.to_owned(),
        severity,
        message,
    }
}

/// Reads a little-endian `u32` at `offset` of `bytes`.
pub(crate) fn u32_at(bytes: &[u8], offset: u32) -> Option<u32> {
    let at = usize::try_from(offset).ok()?;
    let end = at.checked_add(4)?;
    bytes
        .get(at..end)
        .and_then(|slice| <[u8; 4]>::try_from(slice).ok())
        .map(u32::from_le_bytes)
}
