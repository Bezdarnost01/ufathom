//! [`DetourError`]: why a signature, a module lookup or a detour failed.

/// Why a detour operation failed.
///
/// Every variant that concerns an address names it, so a log line is enough
/// to find the function.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum DetourError {
    /// The signature text is malformed.
    #[error("invalid signature: {0}")]
    InvalidSignature(String),
    /// No module with this name is loaded in the process.
    #[error("module {0:?} is not loaded")]
    ModuleNotFound(String),
    /// The headers of a module in memory are not a usable PE image.
    #[error("module at {base:#x} is not a usable PE image: {reason}")]
    BadImage {
        /// Base address of the module.
        base: usize,
        /// What is wrong with the headers.
        reason: &'static str,
    },
    /// A signature search must match exactly once.
    #[error(
        "the signature matched {matches} times in the executable sections; exactly one match is required"
    )]
    SignatureMatches {
        /// How many places matched: 0, or more than one.
        matches: usize,
    },
    /// The target is not in committed, executable memory.
    #[error("{address:#x} is not in committed executable memory ({reason})")]
    NotExecutable {
        /// The address that was checked.
        address: usize,
        /// Which check failed.
        reason: &'static str,
    },
    /// The replacement function pointer is null or not executable.
    #[error("the replacement {address:#x} is not an executable function")]
    BadReplacement {
        /// The replacement address.
        address: usize,
    },
    /// The bytes at the target are not the bytes the author expected.
    #[error(
        "the bytes at {address:#x} differ from the expected signature at offset {offset}: found {found:#04x}"
    )]
    SignatureMismatch {
        /// The target address.
        address: usize,
        /// Offset of the first differing byte.
        offset: usize,
        /// The byte found there.
        found: u8,
    },
    /// The expected signature does not cover every byte that would be
    /// overwritten.
    #[error(
        "the expected signature covers {given} bytes at {address:#x} but {needed} bytes would be overwritten"
    )]
    SignatureTooShort {
        /// The target address.
        address: usize,
        /// Bytes the patch overwrites (whole instructions, at least 5).
        needed: usize,
        /// Bytes the signature covers.
        given: usize,
    },
    /// The first instructions of the function cannot be moved safely.
    #[error("cannot detour {address:#x}: {reason}")]
    Unrelocatable {
        /// The target address.
        address: usize,
        /// Why the instructions at the start of the function are refused.
        reason: String,
    },
    /// No free memory within reach of the target (about 2 GB) could be
    /// allocated for the trampoline.
    #[error("no memory for a trampoline within 2 GB of {address:#x}")]
    NoNearMemory {
        /// The target address.
        address: usize,
    },
    /// This library already holds a detour that overlaps the target.
    #[error("a detour of this library already covers {address:#x}")]
    AlreadyHooked {
        /// The target address.
        address: usize,
    },
    /// A thread was stopped inside the bytes to be overwritten, at a place
    /// where it cannot be moved to the trampoline.
    #[error("a thread is executing inside the bytes to patch at {address:#x}")]
    ThreadInPatch {
        /// The target address.
        address: usize,
    },
    /// The bytes at the target changed while the detour was being installed
    /// or removed. Another patcher is at work.
    #[error(
        "the code at {address:#x} was changed by someone else; the detour was left as it is and its memory kept"
    )]
    Modified {
        /// The target address.
        address: usize,
    },
    /// An operating system call failed.
    #[error("{call} failed with error {code}")]
    Os {
        /// The call that failed.
        call: &'static str,
        /// `GetLastError`, or 0 when the call reports none.
        code: u32,
    },
}
