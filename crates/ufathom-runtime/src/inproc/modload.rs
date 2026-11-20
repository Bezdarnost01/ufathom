//! Rescanning when the loader maps a module.
//!
//! A native class has a virtual table only once the module that defines it is
//! loaded, and the engine registers the class (and creates its default
//! object) some time after that, on the game thread. Blueprint classes reuse
//! the virtual tables of their native parents, so a new native class is the
//! only way a new virtual table appears. [`GameThread::rescan`] hooks it; this
//! file decides when to call it, without a timer running between loads.
//!
//! On Windows the loader tells a registered callback each time it maps or
//! unmaps a DLL (`LdrRegisterDllNotification`). That callback runs inside the
//! loader lock, so it does the least possible: it counts the load and signals
//! an event. A background thread waits on the event and rescans after
//! [`FIRST_RESCAN`] and again after [`SECOND_RESCAN`], because the classes of
//! a module appear some time after it loads.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use super::hook::{GameThread, HookError};

#[cfg(windows)]
use std::sync::atomic::AtomicBool;
#[cfg(windows)]
use std::sync::{Arc, Mutex};
#[cfg(windows)]
use std::thread::{self, JoinHandle};

#[cfg(windows)]
use super::hook::lock;

/// How long after a module load the first rescan runs: its own startup code
/// has had a moment to register classes.
pub const FIRST_RESCAN: Duration = Duration::from_secs(1);
/// How long after a module load the second rescan runs, for classes that are
/// registered later in the game's own loading.
pub const SECOND_RESCAN: Duration = Duration::from_secs(5);
/// A burst of loads postpones the rescans by at most this much, so a game
/// that keeps loading modules is still scanned.
const MAX_POSTPONE: Duration = Duration::from_secs(3);

/// Module loads the notification callback has seen, process-wide.
static LOADS_SEEN: AtomicU64 = AtomicU64::new(0);

/// When rescans are due after module loads. Loads that arrive close together
/// share their rescans.
#[derive(Debug, Default)]
#[cfg_attr(
    not(windows),
    allow(dead_code, reason = "only the Windows watcher thread uses it")
)]
struct Schedule {
    first: Option<Instant>,
    second: Option<Instant>,
    /// When the first load of the current burst arrived.
    burst_start: Option<Instant>,
}

#[cfg_attr(
    not(windows),
    allow(dead_code, reason = "only the Windows watcher thread uses it")
)]
impl Schedule {
    /// A module loaded at `now`: both rescans are due again from `now`, but
    /// no later than [`MAX_POSTPONE`] past where the burst's first load put
    /// them.
    fn loaded(&mut self, now: Instant) {
        let start = *self.burst_start.get_or_insert(now);
        self.first = Some((now + FIRST_RESCAN).min(start + FIRST_RESCAN + MAX_POSTPONE));
        self.second = Some((now + SECOND_RESCAN).min(start + SECOND_RESCAN + MAX_POSTPONE));
    }

    /// When the next rescan is due, if any is waiting.
    fn next_due(&self) -> Option<Instant> {
        match (self.first, self.second) {
            (Some(first), Some(second)) => Some(first.min(second)),
            (due, None) | (None, due) => due,
        }
    }

    /// Whether a rescan is due at `now`. The rescans that are due are merged
    /// into one.
    fn take_due(&mut self, now: Instant) -> bool {
        let mut due = false;
        for slot in [&mut self.first, &mut self.second] {
            if slot.is_some_and(|at| at <= now) {
                *slot = None;
                due = true;
            }
        }
        if self.first.is_none() && self.second.is_none() {
            self.burst_start = None;
        }
        due
    }
}

/// The watcher of [`GameThread::start_rescan_on_module_load`] and the count
/// of rescans, part of [`GameThread`].
pub(super) struct ModuleLoadWatch {
    #[cfg(windows)]
    watcher: Mutex<Option<Watcher>>,
    rescans: AtomicU64,
}

/// A running notification and the thread that serves it.
#[cfg(windows)]
struct Watcher {
    stop: Arc<AtomicBool>,
    /// `None` once the thread was stopped and joined.
    handle: Option<JoinHandle<()>>,
    registration: sys::Registration,
}

impl ModuleLoadWatch {
    pub(super) fn new() -> Self {
        Self {
            #[cfg(windows)]
            watcher: Mutex::new(None),
            rescans: AtomicU64::new(0),
        }
    }

    /// Counts a finished [`GameThread::rescan`].
    pub(super) fn count_rescan(&self) {
        self.rescans.fetch_add(1, Ordering::AcqRel);
    }

    /// Stops the watcher thread, removes the notification and waits for the
    /// thread. Does nothing when none runs.
    pub(super) fn stop(&self) -> Result<(), HookError> {
        #[cfg(windows)]
        {
            let mut slot = lock(&self.watcher);
            let Some(watcher) = slot.as_mut() else {
                return Ok(());
            };
            watcher.stop.store(true, Ordering::Release);
            watcher.registration.wake();
            if let Some(handle) = watcher.handle.take() {
                let _ = handle.join();
            }
            // After this the loader no longer calls the callback, which
            // lives in this module, and the event can be closed.
            watcher
                .registration
                .unregister()
                .map_err(HookError::ModuleNotification)?;
            if let Some(watcher) = slot.take() {
                watcher.registration.close();
            }
        }
        Ok(())
    }
}

impl GameThread {
    /// Starts rescanning when a module loads: a notification from the loader
    /// wakes a background thread, which calls [`rescan`](Self::rescan) one
    /// second after the load and again five seconds after it
    /// ([`FIRST_RESCAN`], [`SECOND_RESCAN`]), because the classes of a module
    /// are registered some time after it loads. Loads that come close together
    /// share their rescans. This hooks the virtual tables of native classes
    /// that a module loaded after [`install`](Self::install) adds; Blueprint
    /// classes reuse the virtual tables of their native parents.
    ///
    /// Prefer this to [`start_auto_rescan`](Self::start_auto_rescan) on
    /// Windows: nothing runs between loads. Use `start_auto_rescan` as well
    /// when a class can get its first instance with no module load.
    ///
    /// Returns `false` when it is already running, and on platforms with no
    /// module load notification (anything but Windows), where nothing is
    /// started. [`uninstall`](Self::uninstall) stops it; so does
    /// [`stop_rescan_on_module_load`](Self::stop_rescan_on_module_load).
    ///
    /// # Errors
    ///
    /// [`HookError::ModuleNotification`] when the loader notification cannot
    /// be registered, [`HookError::Thread`] when the thread cannot be
    /// started.
    pub fn start_rescan_on_module_load(&'static self) -> Result<bool, HookError> {
        #[cfg(windows)]
        {
            let mut slot = lock(&self.modload.watcher);
            if let Some(watcher) = slot.as_ref() {
                return if watcher.handle.is_some() {
                    Ok(false)
                } else {
                    Err(HookError::ModuleNotification(
                        "the notification of an earlier watcher could not be removed".to_owned(),
                    ))
                };
            }
            let registration = sys::Registration::new().map_err(HookError::ModuleNotification)?;
            let stop = Arc::new(AtomicBool::new(false));
            let event = registration.event();
            let thread_stop = Arc::clone(&stop);
            let spawned = thread::Builder::new()
                .name("ufathom-rescan-on-load".to_owned())
                .spawn(move || {
                    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        watch(self, event, &thread_stop);
                    }));
                });
            let handle = match spawned {
                Ok(handle) => handle,
                Err(error) => {
                    // Nothing runs yet, so the callback can go at once.
                    let undone = registration.unregister();
                    if undone.is_ok() {
                        registration.close();
                    }
                    return Err(HookError::Thread(error.to_string()));
                }
            };
            *slot = Some(Watcher {
                stop,
                handle: Some(handle),
                registration,
            });
            Ok(true)
        }
        #[cfg(not(windows))]
        {
            Ok(false)
        }
    }

    /// Stops [`start_rescan_on_module_load`](Self::start_rescan_on_module_load):
    /// removes the loader notification and waits for the thread. Does nothing
    /// when none runs.
    ///
    /// # Errors
    ///
    /// [`HookError::ModuleNotification`] when the notification cannot be
    /// removed; the module must not be unloaded then.
    pub fn stop_rescan_on_module_load(&self) -> Result<(), HookError> {
        self.modload.stop()
    }

    /// `true` while the thread of
    /// [`start_rescan_on_module_load`](Self::start_rescan_on_module_load) runs.
    #[must_use]
    pub fn rescan_on_module_load_running(&self) -> bool {
        #[cfg(windows)]
        {
            lock(&self.modload.watcher)
                .as_ref()
                .is_some_and(|watcher| watcher.handle.is_some())
        }
        #[cfg(not(windows))]
        {
            false
        }
    }

    /// How many [`rescan`](Self::rescan)s have finished, whoever called them.
    #[must_use]
    pub fn rescans_run(&self) -> u64 {
        self.modload.rescans.load(Ordering::Acquire)
    }

    /// How many module loads the loader notification has reported to this
    /// module's callback. The count only moves while
    /// [`start_rescan_on_module_load`](Self::start_rescan_on_module_load) is
    /// registered, and stays 0 on platforms without the notification.
    #[must_use]
    pub fn module_loads_seen() -> u64 {
        LOADS_SEEN.load(Ordering::Acquire)
    }
}

/// The body of the watcher thread: waits for loads, rescans when due.
#[cfg(windows)]
fn watch(game: &'static GameThread, event: usize, stop: &AtomicBool) {
    let mut schedule = Schedule::default();
    loop {
        let timeout = schedule
            .next_due()
            .map(|due| due.saturating_duration_since(Instant::now()));
        let woke = sys::wait(event, timeout);
        if stop.load(Ordering::Acquire) || woke == sys::Wake::Failed {
            return;
        }
        let now = Instant::now();
        if woke == sys::Wake::Signaled {
            schedule.loaded(now);
        }
        // Nothing is hooked after an uninstall, so there is nothing to keep
        // up to date; `rescan` would hook everything again.
        if schedule.take_due(now) && game.hooked_slots() != 0 {
            let _ = game.rescan();
        }
    }
}

/// The loader notification and the event it signals.
#[cfg(windows)]
mod sys {
    use core::ffi::{CStr, c_void};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
    use windows_sys::Win32::System::Threading::{INFINITE, SetEvent, WaitForSingleObject};

    use super::LOADS_SEEN;

    /// `NTSTATUS`: negative values are errors.
    type NtStatus = i32;

    /// `PLDR_DLL_NOTIFICATION_FUNCTION`: the reason, a pointer to the
    /// `LDR_DLL_NOTIFICATION_DATA` (not read here), the registered context.
    type Notification = unsafe extern "system" fn(u32, *const c_void, *mut c_void);
    /// `LdrRegisterDllNotification(Flags, Function, Context, &Cookie)`.
    type Register =
        unsafe extern "system" fn(u32, Notification, *mut c_void, *mut *mut c_void) -> NtStatus;
    /// `LdrUnregisterDllNotification(Cookie)`.
    type Unregister = unsafe extern "system" fn(*mut c_void) -> NtStatus;

    /// `LDR_DLL_NOTIFICATION_REASON_LOADED`.
    const REASON_LOADED: u32 = 1;
    /// `WAIT_OBJECT_0` and `WAIT_TIMEOUT` of `WaitForSingleObject`.
    const WAIT_OBJECT_0: u32 = 0;
    const WAIT_TIMEOUT: u32 = 0x102;

    // `CreateEventW` takes the optional `SECURITY_ATTRIBUTES` as a null
    // pointer here, which keeps the `Win32_Security` feature of `windows-sys`
    // out of this crate's dependencies.
    #[link(name = "kernel32", kind = "raw-dylib")]
    unsafe extern "system" {
        fn CreateEventW(
            attributes: *const c_void,
            manual_reset: i32,
            initial_state: i32,
            name: *const u16,
        ) -> HANDLE;
    }

    /// The event the callback signals, 0 while no notification is registered.
    static EVENT: AtomicUsize = AtomicUsize::new(0);

    /// Called by the loader, inside the loader lock, for every module load
    /// and unload. It must not allocate, take a lock or load a library: it
    /// counts the load and signals the event, nothing else.
    unsafe extern "system" fn on_notification(
        reason: u32,
        _data: *const c_void,
        _ctx: *mut c_void,
    ) {
        if reason != REASON_LOADED {
            return;
        }
        LOADS_SEEN.fetch_add(1, Ordering::AcqRel);
        let event = EVENT.load(Ordering::Acquire);
        if event != 0 {
            // SAFETY: `event` is an open event handle: it is closed only after
            // the notification was unregistered.
            unsafe { SetEvent(event as HANDLE) };
        }
    }

    /// What the watcher thread woke for.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub(super) enum Wake {
        Signaled,
        TimedOut,
        Failed,
    }

    /// Waits for the event, at most `timeout` (for ever when `None`).
    pub(super) fn wait(event: usize, timeout: Option<Duration>) -> Wake {
        let millis = timeout.map_or(INFINITE, |timeout| {
            // One short of `INFINITE`, which would mean for ever.
            u32::try_from(timeout.as_millis()).map_or(INFINITE - 1, |ms| ms.min(INFINITE - 1))
        });
        // SAFETY: `event` stays open until the thread that waits on it has
        // been joined (`ModuleLoadWatch::stop`).
        match unsafe { WaitForSingleObject(event as HANDLE, millis) } {
            WAIT_OBJECT_0 => Wake::Signaled,
            WAIT_TIMEOUT => Wake::TimedOut,
            _ => Wake::Failed,
        }
    }

    /// A registered notification, with the event it signals.
    pub(super) struct Registration {
        event: usize,
        cookie: usize,
        unregister: Unregister,
    }

    impl Registration {
        /// Creates the event and registers the notification.
        pub(super) fn new() -> Result<Self, String> {
            let register = ntdll_function(c"LdrRegisterDllNotification")?;
            let unregister = ntdll_function(c"LdrUnregisterDllNotification")?;
            // SAFETY: both have the documented signatures of these ntdll
            // exports; the function pointer types have the same size.
            let (register, unregister) = unsafe {
                (
                    core::mem::transmute::<unsafe extern "system" fn() -> isize, Register>(
                        register,
                    ),
                    core::mem::transmute::<unsafe extern "system" fn() -> isize, Unregister>(
                        unregister,
                    ),
                )
            };
            // SAFETY: a null security descriptor and name make a private,
            // auto-reset event, not signaled.
            let event = unsafe { CreateEventW(core::ptr::null(), 0, 0, core::ptr::null()) };
            if event.is_null() {
                return Err("cannot create an event".to_owned());
            }
            if EVENT
                .compare_exchange(0, event as usize, Ordering::AcqRel, Ordering::Acquire)
                .is_err()
            {
                // SAFETY: closing the event created above.
                unsafe { CloseHandle(event) };
                return Err("a notification is already registered".to_owned());
            }
            let mut cookie: *mut c_void = core::ptr::null_mut();
            // SAFETY: `on_notification` has the required signature and lives as
            // long as this module, which stays loaded until
            // `Registration::unregister` succeeded; the context is unused.
            let status =
                unsafe { register(0, on_notification, core::ptr::null_mut(), &mut cookie) };
            if status < 0 || cookie.is_null() {
                EVENT.store(0, Ordering::Release);
                // SAFETY: closing the event created above.
                unsafe { CloseHandle(event) };
                return Err(format!(
                    "LdrRegisterDllNotification failed with status {status:#x}"
                ));
            }
            Ok(Self {
                event: event as usize,
                cookie: cookie as usize,
                unregister,
            })
        }

        /// The event handle, for the watcher thread to wait on.
        pub(super) fn event(&self) -> usize {
            self.event
        }

        /// Wakes the watcher thread without a load having happened.
        pub(super) fn wake(&self) {
            // SAFETY: the event is open until `close`.
            unsafe { SetEvent(self.event as HANDLE) };
        }

        /// Removes the notification. The loader takes its notification lock
        /// to do so, and callbacks run under it, so none is running or will
        /// run once this returns `Ok`.
        pub(super) fn unregister(&self) -> Result<(), String> {
            // SAFETY: `cookie` came from `LdrRegisterDllNotification` and has
            // not been unregistered yet.
            let status = unsafe { (self.unregister)(self.cookie as *mut c_void) };
            if status < 0 {
                return Err(format!(
                    "LdrUnregisterDllNotification failed with status {status:#x}"
                ));
            }
            Ok(())
        }

        /// Closes the event. Only after [`unregister`](Self::unregister)
        /// returned `Ok` and the watcher thread was joined.
        pub(super) fn close(self) {
            EVENT.store(0, Ordering::Release);
            // SAFETY: closing the event created in `new`; nothing uses it
            // any more.
            unsafe { CloseHandle(self.event as HANDLE) };
        }
    }

    /// An export of `ntdll.dll`, which every process has loaded.
    fn ntdll_function(name: &CStr) -> Result<unsafe extern "system" fn() -> isize, String> {
        let module: Vec<u16> = "ntdll.dll\0".encode_utf16().collect();
        // SAFETY: `module` is NUL-terminated and lives through the call.
        let ntdll = unsafe { GetModuleHandleW(module.as_ptr()) };
        if ntdll.is_null() {
            return Err("ntdll.dll is not loaded".to_owned());
        }
        // SAFETY: `ntdll` is a loaded module and `name` is NUL-terminated.
        unsafe { GetProcAddress(ntdll, name.as_ptr().cast()) }
            .ok_or_else(|| format!("ntdll.dll has no {}", name.to_string_lossy()))
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, reason = "tests may panic")]

    use super::*;

    const MS: Duration = Duration::from_millis(1);

    #[test]
    fn a_load_schedules_two_rescans() {
        let t0 = Instant::now();
        let mut schedule = Schedule::default();
        assert_eq!(schedule.next_due(), None);
        assert!(!schedule.take_due(t0));
        schedule.loaded(t0);
        assert_eq!(schedule.next_due(), Some(t0 + FIRST_RESCAN));
        assert!(!schedule.take_due(t0 + FIRST_RESCAN - MS));
        assert!(schedule.take_due(t0 + FIRST_RESCAN));
        assert_eq!(schedule.next_due(), Some(t0 + SECOND_RESCAN));
        assert!(!schedule.take_due(t0 + SECOND_RESCAN - MS));
        assert!(schedule.take_due(t0 + SECOND_RESCAN));
        assert_eq!(schedule.next_due(), None);
        assert!(!schedule.take_due(t0 + SECOND_RESCAN * 2));
    }

    #[test]
    fn loads_close_together_share_their_rescans() {
        let t0 = Instant::now();
        let mut schedule = Schedule::default();
        schedule.loaded(t0);
        let t1 = t0 + FIRST_RESCAN / 2;
        schedule.loaded(t1);
        // The first rescan moved to one second after the last load.
        assert!(!schedule.take_due(t0 + FIRST_RESCAN));
        assert_eq!(schedule.next_due(), Some(t1 + FIRST_RESCAN));
        // A late first rescan and the second one that is also due merge.
        let late = t1 + SECOND_RESCAN;
        assert!(schedule.take_due(late));
        assert!(!schedule.take_due(late));
        assert_eq!(schedule.next_due(), None);
    }

    #[test]
    fn a_stream_of_loads_cannot_postpone_the_rescans_for_ever() {
        let t0 = Instant::now();
        let mut schedule = Schedule::default();
        let step = FIRST_RESCAN / 2;
        let mut now = t0;
        let mut first_due = None;
        // A load every half second, for a minute.
        for _ in 0..120 {
            schedule.loaded(now);
            if schedule.take_due(now) {
                first_due = Some(now);
                break;
            }
            now += step;
        }
        let first_due = first_due.expect("a rescan runs during the burst");
        assert!(first_due <= t0 + FIRST_RESCAN + MAX_POSTPONE);
    }
}
