//! Owning a hook: [`HookGuard`], and removing what one module registered.
//!
//! A mod that unloads itself must take its callbacks out first, or the engine
//! would call into unmapped code. Two helpers cover that:
//!
//! - [`HookId::into_guard`] ties one callback to a value: dropping the
//!   [`HookGuard`] removes it.
//! - [`GameThread::remove_owned_by`] removes everything registered by code in
//!   one module, for the case where the callbacks are not individually
//!   tracked. The module is found from the callback's type: its code is
//!   instantiated in the crate that registers it, so in the DLL of the mod
//!   that wrote it.

use super::hook::{GameThread, HookId, lock};
use super::os;

/// Owns one registered callback and removes it when dropped.
///
/// Get one from [`HookId::into_guard`]. [`forget`](Self::forget) gives the
/// callback up to the hook instead, to keep it until
/// [`GameThread::remove`] or an uninstall.
#[must_use = "dropping a HookGuard removes the callback at once; keep it, or call `forget`"]
pub struct HookGuard {
    id: HookId,
    game: &'static GameThread,
}

impl HookId {
    /// Wraps this id in a [`HookGuard`] that removes the callback from `game`
    /// when it is dropped, including on an early return or a panic. `game`
    /// must be the hook the id came from.
    ///
    /// ```text
    /// let _tick = game.before(PATH, |_| { /* ... */ })?.into_guard(game);
    /// // removed at the end of the scope
    /// ```
    pub fn into_guard(self, game: &'static GameThread) -> HookGuard {
        HookGuard { id: self, game }
    }
}

impl HookGuard {
    /// The id of the guarded callback.
    #[must_use]
    pub fn id(&self) -> HookId {
        self.id
    }

    /// Releases the callback from this guard without removing it, and
    /// returns its id for [`GameThread::remove`].
    pub fn forget(self) -> HookId {
        let id = self.id;
        core::mem::forget(self);
        id
    }
}

impl Drop for HookGuard {
    fn drop(&mut self) {
        // `false` only when the callback is already gone, for example after
        // `remove_owned_by`.
        self.game.remove(self.id);
    }
}

impl core::fmt::Debug for HookGuard {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("HookGuard")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

/// Code that exists once per type `T`, so its address tells which module
/// instantiated `T`.
fn anchor<T>() {}

/// The module that holds the code of `T`, for a type defined or instantiated
/// by the caller: base address of the DLL or executable, 0 when unknown or on
/// a platform without modules.
pub(super) fn owner_of<T>() -> usize {
    os::module_of_address(anchor::<T> as *const () as usize)
}

/// [`owner_of`] for the type of `value`, which lets a closure's type be named.
pub(super) fn owner_of_val<T>(_value: &T) -> usize {
    owner_of::<T>()
}

impl GameThread {
    /// Removes every callback (of [`before`](Self::before),
    /// [`after`](Self::after) and the native variants) and every queued task
    /// registered by code in the module whose base address is `module_base`,
    /// and unhooks native functions left with no callback. Returns how many
    /// callbacks and tasks were removed.
    ///
    /// The module of a callback is the one that holds its closure's code,
    /// found when it is registered. Use it before unloading a module that
    /// shares this hook with other modules; a mod that links its own copy of
    /// the runtime owns everything in it, see
    /// [`remove_owned_by_current_module`](Self::remove_owned_by_current_module).
    ///
    /// On Windows `module_base` is an `HMODULE`. Callbacks whose module could
    /// not be determined are owned by 0, as is everything on platforms
    /// without modules.
    pub fn remove_owned_by(&self, module_base: usize) -> usize {
        let callbacks = self.callbacks.remove_owned_by(module_base).0;
        let native = self.native.remove_owned_by(module_base);
        let mut tasks = lock(&self.tasks);
        let queued = tasks.len();
        tasks.retain(|(module, _)| *module != module_base);
        callbacks + native + (queued - tasks.len())
    }

    /// [`remove_owned_by`](Self::remove_owned_by) for the module that holds
    /// this runtime's own code: the DLL or executable it is linked into.
    ///
    /// Call it before [`uninstall_and_wait`](Self::uninstall_and_wait) when
    /// that module is about to be unloaded: callbacks and tasks it registered
    /// would otherwise stay in the table, and run again after a later
    /// [`rescan`](Self::rescan), from code that is gone.
    pub fn remove_owned_by_current_module(&self) -> usize {
        self.remove_owned_by(os::current_module())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_owner_of_a_type_is_the_module_that_holds_this_test() {
        let local = || 1 + 1;
        let owner = owner_of_val(&local);
        assert_eq!(owner, os::current_module());
        if cfg!(windows) {
            assert_ne!(owner, 0);
        }
    }
}
