//! Counting the threads that are inside a replacement function, so the
//! module that holds it is not unloaded while one runs.

use core::sync::atomic::{AtomicUsize, Ordering};

/// Threads inside a [`detour_fn!`](crate::detour_fn) replacement, summed over
/// every detour of the process.
static IN_FLIGHT: AtomicUsize = AtomicUsize::new(0);

/// Number of threads currently inside a replacement defined with
/// [`detour_fn!`](crate::detour_fn) or holding an [`InFlightGuard`].
#[must_use]
pub fn in_flight() -> usize {
    IN_FLIGHT.load(Ordering::Acquire)
}

/// Marks the current thread as running code that must not be unmapped, for as
/// long as the value lives.
///
/// [`detour_fn!`](crate::detour_fn) creates one around the whole body of the
/// replacement. Create one by hand around code that calls
/// [`Detour::original`](super::Detour::original) outside a replacement, so
/// that [`Detour::uninstall_and_wait`](super::Detour::uninstall_and_wait)
/// does not free the trampoline under it.
#[must_use = "the thread is counted only while the guard lives"]
pub struct InFlightGuard(());

impl InFlightGuard {
    /// Counts the current thread until the guard is dropped.
    pub fn enter() -> Self {
        IN_FLIGHT.fetch_add(1, Ordering::AcqRel);
        Self(())
    }
}

impl Drop for InFlightGuard {
    fn drop(&mut self) {
        IN_FLIGHT.fetch_sub(1, Ordering::AcqRel);
    }
}

/// Defines an `extern "system"` function to use as a detour replacement.
///
/// The body runs inside an [`InFlightGuard`], so
/// [`Detour::uninstall_and_wait`](crate::detour::Detour::uninstall_and_wait)
/// and [`uninstall_all_and_wait`](crate::detour::uninstall_all_and_wait) wait
/// for it. It also runs inside `catch_unwind`, so a panic never unwinds into
/// game code (which would abort the process).
///
/// # On a panic
///
/// The panic message goes through the panic hook as usual: the DLL's own
/// hook writes it to the log. The original function is **not** called, since
/// the body may have done part of its work already. The function returns
/// `Default::default()` of its return type (nothing for `()`), or the
/// expression given with `on_panic = ...;` first when the type has no
/// default, as raw pointers do not:
///
/// ```ignore
/// ufathom_runtime::detour_fn! {
///     /// Replaces `int Game::Score(int)`.
///     fn score(points: i32) -> i32 {
///         points.saturating_mul(2)
///     }
/// }
///
/// ufathom_runtime::detour_fn! {
///     on_panic = core::ptr::null_mut();
///     fn lookup(key: u32) -> *mut u8 {
///         core::ptr::null_mut()
///     }
/// }
/// ```
///
/// Arguments are plain identifiers with types (`name: Type`), so a
/// replacement for `this`-call member functions takes `this` first. A
/// `return` in the body returns from the body, not past the guard.
#[macro_export]
macro_rules! detour_fn {
    (
        on_panic = $fallback:expr;
        $(#[$meta:meta])*
        $vis:vis fn $name:ident($($arg:ident : $ty:ty),* $(,)?) $(-> $ret:ty)? $body:block
    ) => {
        $(#[$meta])*
        $vis extern "system" fn $name($($arg: $ty),*) $(-> $ret)? {
            let _in_flight = $crate::detour::InFlightGuard::enter();
            match ::std::panic::catch_unwind(::std::panic::AssertUnwindSafe(
                move || $(-> $ret)? { $body },
            )) {
                Ok(value) => value,
                Err(_) => $fallback,
            }
        }
    };
    (
        $(#[$meta:meta])*
        $vis:vis fn $name:ident($($arg:ident : $ty:ty),* $(,)?) $(-> $ret:ty)? $body:block
    ) => {
        $crate::detour_fn! {
            on_panic = ::core::default::Default::default();
            $(#[$meta])*
            $vis fn $name($($arg: $ty),*) $(-> $ret)? $body
        }
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_guard_counts_while_it_lives() {
        let before = in_flight();
        let guard = InFlightGuard::enter();
        assert_eq!(in_flight(), before + 1);
        drop(guard);
        assert_eq!(in_flight(), before);
    }
}
