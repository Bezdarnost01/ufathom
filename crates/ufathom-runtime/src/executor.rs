//! The only way to call a game function.

use crate::object::UObjectBase;
use crate::ptr::Ptr;

/// Why a call into the game did not happen or failed.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum CallError {
    /// The executor cannot run the call here, for example because the caller
    /// is not on the game thread.
    #[error("function call refused: {0}")]
    Refused(String),
    /// No function with this path exists in the target.
    #[error("function not found: {0}")]
    FunctionNotFound(String),
    /// The object address is null, or its index and serial no longer match.
    #[error("object {addr:#x} is not valid")]
    InvalidObject {
        /// The object address that was passed.
        addr: u64,
    },
    /// The parameter block is smaller than the size the function declares.
    #[error("parameter block is {actual} bytes, the function takes {expected}")]
    ParamsSize {
        /// Size the function declares.
        expected: usize,
        /// Size that was passed.
        actual: usize,
    },
    /// The call itself failed.
    #[error("call failed: {0}")]
    Failed(String),
}

/// Calls a function of the game with a parameter block.
///
/// Generated wrappers take `&impl Executor`. A reader (`MemoryReader`) is
/// not an `Executor`, so a call through an external session does not compile.
///
/// # Safety
///
/// This is an `unsafe` trait on purpose: an implementation runs game code, so
/// it must only be written by code that can guarantee it is sound to do so
/// (for example, an in-process executor that is on the game thread and has
/// validated the object). Nothing implements it by accident. This crate's
/// implementation is `InProcessExecutor`, behind the `inproc` feature. A test
/// double that never touches a real game may implement it too.
///
/// An implementation must:
///
/// - run `function_path` on `object` only if both are valid for the target;
/// - treat `params` as the function's parameter block: it is a view over a
///   `#[repr(C)]` struct, so its start has the alignment the engine expects,
///   and its length is the size of the block;
/// - write the results of out parameters and the return value back into
///   `params`.
pub unsafe trait Executor {
    /// Calls the function at `function_path` (for example
    /// `/Script/Engine.Actor:K2_DestroyActor`) on `object`.
    ///
    /// The path is passed rather than a resolved handle so a wrapper needs no
    /// session; an executor is expected to cache the lookup.
    ///
    /// # Errors
    ///
    /// A [`CallError`] if the call is refused or fails. On error the contents
    /// of `params` are unspecified.
    fn process_event(
        &self,
        object: Ptr<UObjectBase>,
        function_path: &str,
        params: &mut [u8],
    ) -> Result<(), CallError>;
}
