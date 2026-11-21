//! [`HookMode`]: how `ProcessEvent` is hooked, and the one inline detour of
//! the [`Detour`](HookMode::Detour) mode.
//!
//! In [`Vtable`](HookMode::Vtable) mode every virtual table that holds the
//! engine's `ProcessEvent` gets its slot swapped, so only tables found through
//! a live object are covered and later classes need a rescan. In
//! [`Detour`](HookMode::Detour) mode the engine's implementation itself is
//! detoured once, so every caller of it is covered, whatever table it came
//! through and whenever the class appeared.
//!
//! # What the detour trusts
//!
//! A detour made by an author checks the bytes at the target against a
//! signature the author wrote down. Here there is no author: the target is the
//! slot value most virtual tables share (the vote [`GameThread::install`]
//! already takes), so the bytes it is checked against are its own current
//! bytes. What still protects the game is the safety analysis of the
//! [`detour`](crate::detour) module: whole instructions are decoded, and the
//! function is refused when the stolen bytes contain a `ret`, a trap or a
//! branch into themselves, or cannot be relocated. What it cannot notice is a
//! vote that elected a function other than `ProcessEvent`, or bytes that another
//! tool already patched.
//!
//! [`GameThread::install`]: super::GameThread::install

/// How [`GameThread::install_with`](super::GameThread::install_with) hooks
/// `ProcessEvent`.
///
/// | | [`Vtable`](Self::Vtable) | [`Detour`](Self::Detour) |
/// |---|---|---|
/// | Patches | data pointers in virtual tables | the first bytes of the engine's `ProcessEvent` |
/// | Class with no live object yet | missed until a rescan | covered |
/// | Class that overrides `ProcessEvent` | missed | covered when it calls the base implementation |
/// | Rescans | needed | no-ops |
/// | Needs | nothing | the `detour` feature, Windows x86-64 |
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[non_exhaustive]
pub enum HookMode {
    /// Swap the `ProcessEvent` slot in every native virtual table that holds
    /// the engine's implementation. No code is patched. The default.
    #[default]
    Vtable,
    /// Detour the engine's `UObject::ProcessEvent` once, found as the slot
    /// value most virtual tables share. Patches code, so it needs the
    /// `detour` feature and Windows x86-64; without them
    /// [`install_with`](super::GameThread::install_with) fails with
    /// [`HookError::DetourUnavailable`](super::HookError::DetourUnavailable).
    ///
    /// There is no author signature to check the target against: the bytes
    /// found there are trusted, subject to the safety analysis of the
    /// [`detour`](crate::detour) module. A class that overrides `ProcessEvent`
    /// without calling the base implementation is not covered.
    ///
    /// [`rescan`](super::GameThread::rescan), the background rescans and
    /// [`hooked_slots`](super::GameThread::hooked_slots) have nothing to do in
    /// this mode, and an uninstalled hook is not installed again.
    Detour,
}

impl core::fmt::Display for HookMode {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(match self {
            Self::Vtable => "vtable",
            Self::Detour => "detour",
        })
    }
}

pub(super) use engine::EngineDetour;

/// The detour of [`HookMode::Detour`] when this build can patch code.
#[cfg(all(feature = "detour", windows, target_arch = "x86_64"))]
mod engine {
    use core::ffi::c_void;
    use std::sync::Mutex;
    use std::time::Duration;

    use ufathom_mem::{MemoryReader, SelfMemory};

    use super::super::hook::{HookError, ORIGINAL, hooked_process_event, lock};
    use crate::detour::{Detour, DetourError};

    /// What became of the detour.
    enum State {
        /// Not installed yet.
        Empty,
        /// Patched into the engine.
        Live(Detour),
        /// The bytes are back, the trampoline memory is still there.
        Restored(Detour),
        /// Restored and freed.
        Released,
    }

    /// The detour of the engine's `ProcessEvent`, part of
    /// [`GameThread`](super::super::GameThread).
    pub(in super::super) struct EngineDetour {
        state: Mutex<State>,
    }

    crate::detour_fn! {
        /// The detour's replacement: counts the thread as inside a detour, so
        /// [`Detour::uninstall_and_wait`] sees it from its first instruction,
        /// and enters the same code a swapped slot enters.
        fn detour_process_event(this: *mut c_void, function: *mut c_void, parms: *mut c_void) {
            // SAFETY: the arguments are the ones the engine passed to
            // `ProcessEvent`; `ORIGINAL` holds the trampoline, stored before
            // the patch was written.
            unsafe { hooked_process_event(this, function, parms) };
        }
    }

    impl EngineDetour {
        pub(in super::super) const AVAILABLE: bool = true;

        pub(in super::super) fn new() -> Self {
            Self {
                state: Mutex::new(State::Empty),
            }
        }

        /// Detours the function `find_target` returns, unless that was done
        /// already (then `find_target` is not called).
        ///
        /// # Safety
        ///
        /// `find_target` returns the start of the engine's
        /// `UObject::ProcessEvent` (contract of
        /// [`GameThread::install_with`](super::super::GameThread::install_with)).
        pub(in super::super) unsafe fn install(
            &self,
            find_target: impl FnOnce() -> Result<usize, HookError>,
        ) -> Result<(), HookError> {
            let mut state = lock(&self.state);
            if !matches!(*state, State::Empty) {
                return Ok(());
            }
            let target = find_target()?;
            // SAFETY: `target` is a function start by the caller's contract;
            // the replacement has its signature (`this`, the function and the
            // parameter block; `extern "system"` is the C convention on
            // x86-64) and stays mapped until `uninstall_and_wait` reports the
            // hook idle (contract of `install_with`).
            let detour = unsafe {
                Detour::install_trusting(target, detour_process_event as *const (), &ORIGINAL)
            }
            .map_err(|error| HookError::Detour {
                target: target as u64,
                reason: error.to_string(),
            })?;
            *state = State::Live(detour);
            Ok(())
        }

        /// Puts the original bytes back. [`HookError::Chained`] when another
        /// tool patched the function afterwards: the patch stays, forwarding,
        /// and a later call retries.
        pub(in super::super) fn uninstall(&self) -> Result<(), HookError> {
            let mut state = lock(&self.state);
            let State::Live(detour) = &*state else {
                return Ok(());
            };
            match detour.restore_in_place() {
                Ok(()) => {}
                Err(DetourError::Modified { address }) => {
                    return Err(HookError::Chained {
                        slot: address as u64,
                        holder: jump_destination(address),
                        blocked: 1,
                    });
                }
                Err(error) => {
                    return Err(HookError::Detour {
                        target: detour.target() as u64,
                        reason: error.to_string(),
                    });
                }
            }
            if let State::Live(detour) = core::mem::replace(&mut *state, State::Released) {
                *state = State::Restored(detour);
            }
            Ok(())
        }

        /// After [`uninstall`](Self::uninstall): waits up to `timeout` until
        /// nothing runs in the detour's memory and frees it. `false` when a
        /// thread is still there, or the detour was not uninstalled.
        pub(in super::super) fn release(&self, timeout: Duration) -> Result<bool, HookError> {
            let mut state = lock(&self.state);
            match &*state {
                State::Empty | State::Released => Ok(true),
                State::Live(_) => Ok(false),
                State::Restored(detour) => {
                    let released =
                        detour
                            .release_when_idle(timeout)
                            .map_err(|error| HookError::Detour {
                                target: detour.target() as u64,
                                reason: error.to_string(),
                            })?;
                    if released {
                        *state = State::Released;
                    }
                    Ok(released)
                }
            }
        }

        /// The patched function, while the patch is in the engine's code.
        pub(in super::super) fn target(&self) -> Option<usize> {
            match &*lock(&self.state) {
                State::Live(detour) => Some(detour.target()),
                State::Empty | State::Restored(_) | State::Released => None,
            }
        }
    }

    /// Where the `jmp rel32` at `address` goes, or 0 when the bytes there are
    /// not one: what a foreign patch over the detour usually is.
    fn jump_destination(address: usize) -> u64 {
        let mut bytes = [0u8; 5];
        if SelfMemory::new()
            .read_exact(address as u64, &mut bytes)
            .is_err()
        {
            return 0;
        }
        let [opcode, a, b, c, d] = bytes;
        if opcode != 0xE9 {
            return 0;
        }
        let displacement = i64::from(i32::from_le_bytes([a, b, c, d]));
        (address as u64)
            .wrapping_add(5)
            .wrapping_add_signed(displacement)
    }
}

/// The stand-in of a build that cannot patch code: every use fails clearly,
/// and nothing is ever installed.
#[cfg(not(all(feature = "detour", windows, target_arch = "x86_64")))]
mod engine {
    use std::time::Duration;

    use super::super::hook::HookError;

    /// Nothing: this build has no detours.
    pub(in super::super) struct EngineDetour;

    impl EngineDetour {
        pub(in super::super) const AVAILABLE: bool = false;

        pub(in super::super) fn new() -> Self {
            Self
        }

        /// # Safety
        ///
        /// None: it always fails.
        pub(in super::super) unsafe fn install(
            &self,
            _find_target: impl FnOnce() -> Result<usize, HookError>,
        ) -> Result<(), HookError> {
            Err(HookError::DetourUnavailable)
        }

        pub(in super::super) fn uninstall(&self) -> Result<(), HookError> {
            Ok(())
        }

        pub(in super::super) fn release(&self, _timeout: Duration) -> Result<bool, HookError> {
            Ok(true)
        }

        pub(in super::super) fn target(&self) -> Option<usize> {
            None
        }
    }
}
