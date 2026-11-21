//! One registration for a game function, whatever mechanism sees its calls.
//!
//! [`GameThread::hook`] and [`GameThread::hook_native`] take the function as a
//! [`GameFunction`] marker and a [`When`], and pick the mechanism from the
//! marker's [`Kind`](GameFunction::Kind), so an author does not need to know
//! which engine path a function takes:
//!
//! - a [`Script`](super::Script) function is entered through `ProcessEvent`,
//!   which has the decoded parameter block: `hook` registers on it and the
//!   callback gets the block as [`F::Params`](GameFunction::Params);
//! - a [`Native`](super::Native) function has a C++ body that every invocation
//!   reaches through `UFunction::Func`, bytecode callers and `ProcessEvent`
//!   (`UFunction::Invoke`) alike: `hook_native` registers on `Func`, and the
//!   callback gets the [`NativeContext`].
//!
//! # One mechanism, so one call fires once
//!
//! A native function called through `ProcessEvent` passes through its `Func`
//! as well. Registering on both would fire twice for those calls, so a native
//! function is registered on `Func` alone, which also sees the bytecode
//! callers `ProcessEvent` never does. A direct C++ call of the native
//! implementation, bypassing the `UFunction`, is seen by neither; that needs a
//! [`Detour`](crate::detour::Detour).
//!
//! # Two methods, not one
//!
//! The callbacks differ in type: `(&mut CallContext, &mut F::Params)` against
//! `&mut NativeContext`. One method generic over both would need the compiler
//! to pick a closure signature from the marker, which it cannot infer, so the
//! author would have to annotate every closure. Two methods keep closures
//! plain, and the marker bound ([`ScriptFunction`], [`NativeFunction`]) turns
//! a wrong choice into an error naming the other method.
//!
//! Both return the [`HookId`] the other hooks return, so
//! [`HookId::into_guard`], [`GameThread::remove`] and
//! [`GameThread::remove_owned_by`] work as they do for them. The owning module
//! is recorded by the same registration the typed hooks use, from the
//! callback's own type.

use super::hook::{CallContext, GameThread, HookError, HookId};
use super::native::NativeContext;
use super::typed::{NativeFunction, ScriptFunction};

/// Whether a hook runs before or after the function.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum When {
    /// Before the function runs. A script hook may change the inputs and skip
    /// the function.
    Before,
    /// After it ran, with its outputs and return value.
    After,
}

impl GameThread {
    /// Calls `callback` on the game thread before or after each
    /// `ProcessEvent` call of the script function `F`, with its parameter
    /// block as [`F::Params`](super::GameFunction::Params).
    ///
    /// [`When::Before`] is [`before_fn`](Self::before_fn): the callback may
    /// change the inputs and [`skip_original`](CallContext::skip_original).
    /// [`When::After`] is [`after_fn`](Self::after_fn): it sees the outputs and
    /// the return value. A block that does not fit `F::Params` skips the
    /// callback, as there.
    ///
    /// ```ignore
    /// game.hook::<sdk::engine::AActor_ReceiveTick_Params>(When::Before, |ctx, params| {
    ///     params.delta_seconds *= 0.5;
    /// })?;
    /// ```
    ///
    /// A native function is a compile error here; use
    /// [`hook_native`](Self::hook_native).
    ///
    /// # Errors
    ///
    /// As [`before`](Self::before).
    pub fn hook<F: ScriptFunction>(
        &self,
        when: When,
        callback: impl Fn(&mut CallContext<'_>, &mut F::Params) + Send + Sync + 'static,
    ) -> Result<HookId, HookError> {
        match when {
            When::Before => self.before_fn::<F>(callback),
            When::After => self.after_fn::<F>(callback),
        }
    }

    /// Calls `callback` on the game thread before or after each invocation of
    /// the native function `F`, once per call: from Blueprint bytecode and from
    /// `ProcessEvent` alike (see the [module documentation](self)).
    ///
    /// The callback gets the [`NativeContext`] and no typed parameters: where
    /// a native invocation keeps its arguments depends on its caller.
    /// [`When::After`] sees the return value at [`NativeContext::result`]. The
    /// callback observes the call and cannot skip it.
    ///
    /// ```ignore
    /// game.hook_native::<sdk::engine::AActor_K2_DestroyActor_Params>(When::Before, |ctx| {
    ///     let _actor = ctx.object();
    /// })?;
    /// ```
    ///
    /// A script function is a compile error here; use
    /// [`hook`](Self::hook).
    ///
    /// # Errors
    ///
    /// As [`before_native`](Self::before_native).
    pub fn hook_native<F: NativeFunction>(
        &self,
        when: When,
        callback: impl Fn(&mut NativeContext<'_>) + Send + Sync + 'static,
    ) -> Result<HookId, HookError> {
        match when {
            When::Before => self.before_native_fn::<F>(callback),
            When::After => self.after_native_fn::<F>(callback),
        }
    }
}
