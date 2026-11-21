//! Typed hooks: a game function named by a type instead of a string.
//!
//! [`GameThread::before`] and friends take a path such as
//! `/Script/Engine.Actor:ReceiveTick` and hand the callback raw bytes. A typo
//! in the path, or a hand-written parameter struct with the wrong layout, only
//! shows up in the game. A generated SDK implements [`GameFunction`] for the
//! parameter struct of every function, so
//!
//! ```ignore
//! game.before_fn::<sdk::engine::AActor_ReceiveTick_Params>(|ctx, params| {
//!     params.delta_seconds *= 0.5;
//! })?;
//! ```
//!
//! names the function with a type the compiler checks, and the callback gets
//! the parameter block as that type.

use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};

use ufathom_mem::Pod;

use super::hook::{CallContext, GameThread, HookError, HookId};
use super::native::NativeContext;

mod sealed {
    pub trait Sealed {}
}

/// How a function is implemented: [`Native`] or [`Script`]. Sealed.
pub trait FunctionKind: sealed::Sealed {
    /// `true` for [`Native`].
    const IS_NATIVE: bool;
}

/// A function with `FUNC_Native`: C++ code runs when it is invoked, through
/// its `UFunction::Func` pointer. Only these can be hooked with
/// [`GameThread::before_native_fn`].
#[derive(Debug, Clone, Copy)]
pub enum Native {}

/// A function without `FUNC_Native`: it runs bytecode, and is always entered
/// through `ProcessEvent`.
#[derive(Debug, Clone, Copy)]
pub enum Script {}

impl sealed::Sealed for Native {}
impl sealed::Sealed for Script {}

impl FunctionKind for Native {
    const IS_NATIVE: bool = true;
}

impl FunctionKind for Script {
    const IS_NATIVE: bool = false;
}

/// A game function known at compile time: its path, and the layout of its
/// parameter block.
///
/// A generated SDK implements this for the parameter struct of each function
/// (`Self` is both the marker and [`Params`](Self::Params)), with the same
/// path its wrapper calls. A hand-written marker works too.
///
/// Implementing it makes a promise the compiler cannot check: `Params` has
/// the layout the engine uses for the block of the function at `PATH`.
/// Generated structs carry size and offset assertions against the dump. A
/// block smaller than `Params` is detected at run time and the callback is
/// skipped.
pub trait GameFunction: 'static {
    /// Full object path of the `UFunction`, such as
    /// `/Script/Engine.Actor:ReceiveTick`.
    const PATH: &'static str;

    /// Whether the function is [`Native`] or a [`Script`] function.
    type Kind: FunctionKind;

    /// The parameter block: inputs, outputs and the return value.
    type Params: Pod;

    /// `true` when the function has `FUNC_Native`. Always
    /// `<Self::Kind as FunctionKind>::IS_NATIVE`; do not override it.
    const IS_NATIVE: bool = <Self::Kind as FunctionKind>::IS_NATIVE;
}

/// A [`GameFunction`] with `FUNC_Native`, accepted by
/// [`GameThread::before_native_fn`] and [`GameThread::after_native_fn`].
/// Implemented for every function whose [`Kind`](GameFunction::Kind) is
/// [`Native`]; hooking a [`Script`] function this way does not compile:
///
/// ```compile_fail,E0277
/// use ufathom_runtime::inproc::{GameFunction, GameThread, Script};
///
/// struct Tick;
/// impl GameFunction for Tick {
///     const PATH: &'static str = "/Script/Engine.Actor:ReceiveTick";
///     type Kind = Script;
///     type Params = [u8; 0];
/// }
/// # fn hook(game: &GameThread) {
/// let _ = game.before_native_fn::<Tick>(|_| {});
/// # }
/// ```
#[diagnostic::on_unimplemented(
    message = "`{Self}` is not a native function",
    label = "this function has no `FUNC_Native`",
    note = "native hooks need `type Kind = Native`; hook a script function with `hook`, `before_fn` or `after_fn`"
)]
pub trait NativeFunction: GameFunction<Kind = Native> {}

impl<F: GameFunction<Kind = Native>> NativeFunction for F {}

/// A [`GameFunction`] without `FUNC_Native`, accepted by
/// [`GameThread::hook`]. Implemented for every function whose
/// [`Kind`](GameFunction::Kind) is [`Script`]; a native one goes to
/// [`GameThread::hook_native`] instead, and hooking it with `hook` does not
/// compile:
///
/// ```compile_fail,E0277
/// use ufathom_runtime::inproc::{GameFunction, GameThread, Native, When};
///
/// struct Destroy;
/// impl GameFunction for Destroy {
///     const PATH: &'static str = "/Script/Engine.Actor:K2_DestroyActor";
///     type Kind = Native;
///     type Params = [u8; 0];
/// }
/// # fn hook(game: &GameThread) {
/// let _ = game.hook::<Destroy>(When::Before, |_, _| {});
/// # }
/// ```
#[diagnostic::on_unimplemented(
    message = "`{Self}` is not a script function",
    label = "this function has `FUNC_Native`",
    note = "script hooks need `type Kind = Script`; hook a native function with `hook_native`"
)]
pub trait ScriptFunction: GameFunction<Kind = Script> {}

impl<F: GameFunction<Kind = Script>> ScriptFunction for F {}

/// The parameter block as `P`, when it is large enough and aligned for it.
/// A zero-sized `P` (a function without parameters) always fits: the engine
/// passes no block for it, and a reference to a zero-sized value needs no
/// memory.
pub(super) fn cast_block<P: Pod>(block: &mut [u8]) -> Option<&mut P> {
    if size_of::<P>() == 0 {
        // SAFETY: a reference to a zero-sized `P` only needs a non-null,
        // aligned address, which `dangling` is; it reads and writes nothing.
        return Some(unsafe { core::ptr::NonNull::<P>::dangling().as_mut() });
    }
    if block.len() < size_of::<P>() || block.as_ptr().align_offset(align_of::<P>()) != 0 {
        return None;
    }
    // SAFETY: the block is large enough and aligned for `P`, uniquely
    // borrowed, and `P: Pod` accepts every byte pattern.
    Some(unsafe { &mut *block.as_mut_ptr().cast::<P>() })
}

/// Runs `callback` with the block of `context` as `P`. The block is lent to
/// the callback as `P`, so the context handed along has an empty block for
/// the duration of the call: the two would otherwise alias. When the block
/// does not fit `P` the callback is not called.
fn call_typed<P: Pod>(
    context: &mut CallContext<'_>,
    callback: &(dyn Fn(&mut CallContext<'_>, &mut P) + Send + Sync),
) {
    let block = core::mem::take(&mut context.params);
    let outcome = match cast_block::<P>(block) {
        Some(params) => catch_unwind(AssertUnwindSafe(|| callback(context, params))),
        None => Ok(()),
    };
    // Put back before anything else runs, a panic included: later callbacks
    // of the same call share this context.
    context.params = block;
    if let Err(panic) = outcome {
        resume_unwind(panic);
    }
}

impl GameThread {
    /// Calls `callback` on the game thread before each `ProcessEvent` call of
    /// `F`, with its parameter block as [`F::Params`](GameFunction::Params).
    /// The callback may change the inputs and skip the function.
    ///
    /// If the live block is smaller than `F::Params`, or not aligned for it
    /// (the SDK was generated for another build), the callback is not called
    /// for that call.
    ///
    /// Inside the callback, [`CallContext::params`] and
    /// [`CallContext::params_mut`] are empty: the block is the second
    /// argument.
    ///
    /// Like [`before`](Self::before), this sees only calls that go through
    /// `ProcessEvent`. Blueprint calls of a native function do not; use
    /// [`before_native_fn`](Self::before_native_fn) for those.
    ///
    /// # Errors
    ///
    /// As [`before`](Self::before).
    pub fn before_fn<F: GameFunction>(
        &self,
        callback: impl Fn(&mut CallContext<'_>, &mut F::Params) + Send + Sync + 'static,
    ) -> Result<HookId, HookError> {
        self.before(F::PATH, typed(callback))
    }

    /// Calls `callback` on the game thread after each `ProcessEvent` call of
    /// `F`, with its outputs and return value in
    /// [`F::Params`](GameFunction::Params). A block that does not fit is
    /// handled as in [`before_fn`](Self::before_fn).
    ///
    /// # Errors
    ///
    /// As [`after`](Self::after).
    pub fn after_fn<F: GameFunction>(
        &self,
        callback: impl Fn(&mut CallContext<'_>, &mut F::Params) + Send + Sync + 'static,
    ) -> Result<HookId, HookError> {
        self.after(F::PATH, typed(callback))
    }

    /// Like [`before_native`](Self::before_native), for a function known at
    /// compile time. `F` must be [`Native`]; a script function is a compile
    /// error.
    ///
    /// The callback gets the [`NativeContext`] only: where the arguments of a
    /// native invocation are depends on its caller, so they are not decoded
    /// into [`F::Params`](GameFunction::Params).
    ///
    /// # Errors
    ///
    /// As [`before_native`](Self::before_native).
    pub fn before_native_fn<F: NativeFunction>(
        &self,
        callback: impl Fn(&mut NativeContext<'_>) + Send + Sync + 'static,
    ) -> Result<HookId, HookError> {
        self.before_native(F::PATH, callback)
    }

    /// Like [`after_native`](Self::after_native), for a function known at
    /// compile time. `F` must be [`Native`].
    ///
    /// # Errors
    ///
    /// As [`after_native`](Self::after_native).
    pub fn after_native_fn<F: NativeFunction>(
        &self,
        callback: impl Fn(&mut NativeContext<'_>) + Send + Sync + 'static,
    ) -> Result<HookId, HookError> {
        self.after_native(F::PATH, callback)
    }
}

/// Adapts a typed callback to the untyped one the tables store.
fn typed<P: Pod>(
    callback: impl Fn(&mut CallContext<'_>, &mut P) + Send + Sync + 'static,
) -> impl Fn(&mut CallContext<'_>) + Send + Sync + 'static {
    move |context: &mut CallContext<'_>| call_typed(context, &callback)
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "tests may panic"
)]
mod tests {
    use super::*;

    #[repr(C, align(16))]
    #[derive(Clone, Copy)]
    struct NoParams;
    // SAFETY: zero-sized: every bit pattern of zero bytes is valid.
    unsafe impl Pod for NoParams {}

    #[test]
    fn a_function_without_parameters_always_fits() {
        assert!(cast_block::<NoParams>(&mut []).is_some());
        assert!(cast_block::<NoParams>(&mut [0u8; 3]).is_some());
    }

    #[test]
    fn a_block_must_be_large_and_aligned_enough() {
        let mut words = [0u64; 2];
        // SAFETY: `words` is 16 initialised bytes, viewed as bytes.
        let bytes = unsafe { core::slice::from_raw_parts_mut(words.as_mut_ptr().cast::<u8>(), 16) };
        assert!(cast_block::<[u32; 4]>(bytes).is_some());
        assert!(cast_block::<[u32; 5]>(bytes).is_none(), "too small");
        assert!(cast_block::<u64>(&mut bytes[4..]).is_none(), "misaligned");
        *cast_block::<u32>(bytes).unwrap() = 7;
        assert_eq!(words[0] & 0xffff_ffff, 7);
    }
}
