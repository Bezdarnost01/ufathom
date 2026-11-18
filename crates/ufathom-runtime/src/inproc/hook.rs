//! [`GameThread`]: a `ProcessEvent` hook that runs code on the game thread.
//!
//! The engine calls `ProcessEvent` on the game thread many times per frame.
//! [`GameThread::install`] replaces the `ProcessEvent` slot in the virtual
//! table of every native class that uses the common implementation, so each
//! of those calls passes through [`hooked_process_event`] first. There:
//!
//! - queued tasks ([`GameThread::run`], [`GameThread::call`]) run, with an
//!   [`InProcessExecutor`] that may call game functions;
//! - callbacks registered for one function ([`GameThread::before`],
//!   [`GameThread::after`]) see its object and parameters and may skip it.
//!
//! No machine code is patched: only data pointers in virtual tables change,
//! and [`GameThread::uninstall`] puts them back.
//!
//! # The game thread
//!
//! Tasks and callbacks run only on the game thread. It is, in order:
//!
//! 1. the thread named by [`GameThread::set_game_thread`], when the caller
//!    knows it;
//! 2. the process's main thread, which is the game thread of a standard
//!    Unreal Engine build, as soon as it makes a hooked call;
//! 3. otherwise, the first thread to make 64 hooked calls.
//!
//! # Cost on the game thread
//!
//! With nothing queued and no callback registered, a hooked call costs two
//! atomic loads and a thread-local read before it forwards. With callbacks
//! registered, a call of a function that has none is rejected by a 1024-bit
//! filter; only a filter hit takes a read lock to clone an `Arc` of the
//! callback table. No call allocates.
//!
//! # Classes loaded later
//!
//! A virtual table is found through a live object of its class, so a native
//! class with no instance at install time is not hooked yet.
//! [`GameThread::rescan`] hooks what appeared since, and
//! [`GameThread::start_auto_rescan`] does that periodically on a background
//! thread of its own.
//!
//! # Unloading
//!
//! [`GameThread::uninstall_and_wait`] restores every slot and then waits
//! until no thread runs inside the hook, so the module that holds it can be
//! unloaded.

use core::ffi::c_void;
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet, VecDeque};
use std::ops::ControlFlow;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError, RwLock, mpsc};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use ufathom_mem::{MemoryReaderExt, Pod, SelfMemory};

use super::native::NativeHooks;
use super::{InProcessExecutor, ProcessEventFn, os, patch};
use crate::desc::EngineDesc;
use crate::object::UObjectBase;
use crate::ptr::Ptr;
use crate::session::{Session, SessionError};

/// `ProcessEvent` calls a thread must make before it is taken as the game
/// thread, when it is not the main thread and none was named.
const GAME_THREAD_VOTES: u32 = 64;

/// How long [`GameThread::uninstall_and_wait`] keeps waiting after the last
/// thread left the hook, for a thread that read a patched slot just before
/// it was restored and has not entered yet.
const UNLOAD_GRACE: Duration = Duration::from_millis(50);

/// Shortest interval [`GameThread::start_auto_rescan`] accepts: a rescan
/// walks the whole object array.
pub const MIN_RESCAN_INTERVAL: Duration = Duration::from_millis(250);

/// Why the hook could not do what was asked.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum HookError {
    /// Opening the session failed, or the build has no `ProcessEvent` index.
    #[error(transparent)]
    Session(#[from] SessionError),
    /// No virtual table with a `ProcessEvent` slot was found.
    #[error("no virtual table to hook: {0}")]
    NothingToHook(String),
    /// A virtual table slot could not be written.
    #[error("cannot patch the virtual table slot at {slot:#x}: {reason}")]
    Patch {
        /// Address of the slot.
        slot: u64,
        /// What the operating system said.
        reason: String,
    },
    /// No function with this path exists, or the object there is not a
    /// function.
    #[error("function not found: {0}")]
    FunctionNotFound(String),
    /// The game thread did not run the task in time. The task stays queued
    /// and still runs later.
    #[error("the game thread did not run the task within {0:?}")]
    Timeout(Duration),
    /// The task never produced a result: it panicked, or the executor could
    /// not be created on the game thread.
    #[error("the task did not complete")]
    Dropped,
    /// A background thread could not be started.
    #[error("cannot start a thread: {0}")]
    Thread(String),
    /// A native function hook is not possible in this build, for example
    /// because the offset of `UFunction::Func` could not be solved.
    #[error("native function hook: {0}")]
    Native(String),
}

/// What a task receives on the game thread.
pub struct GameContext<'a> {
    exec: &'a InProcessExecutor,
}

impl GameContext<'_> {
    /// An executor for calling game functions, for example through generated
    /// wrappers.
    #[must_use]
    pub fn executor(&self) -> &InProcessExecutor {
        self.exec
    }

    /// Names, objects and lookups.
    #[must_use]
    pub fn session(&self) -> &Session<SelfMemory> {
        self.exec.session()
    }
}

impl core::fmt::Debug for GameContext<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("GameContext").finish_non_exhaustive()
    }
}

/// One intercepted `ProcessEvent` call, as a callback sees it.
pub struct CallContext<'a> {
    object: Ptr<UObjectBase>,
    function: Ptr<UObjectBase>,
    params: &'a mut [u8],
    exec: &'a InProcessExecutor,
    after: bool,
    skip: bool,
}

impl CallContext<'_> {
    /// The object the function runs on.
    #[must_use]
    pub fn object(&self) -> Ptr<UObjectBase> {
        self.object
    }

    /// The `UFunction`.
    #[must_use]
    pub fn function(&self) -> Ptr<UObjectBase> {
        self.function
    }

    /// `true` in an [`after`](GameThread::after) callback.
    #[must_use]
    pub fn is_after(&self) -> bool {
        self.after
    }

    /// The parameter block, `PropertiesSize` bytes long. Empty for a
    /// function without parameters.
    #[must_use]
    pub fn params(&self) -> &[u8] {
        self.params
    }

    /// The parameter block, writable: a `before` callback may change the
    /// inputs, an `after` callback the outputs and the return value.
    pub fn params_mut(&mut self) -> &mut [u8] {
        self.params
    }

    /// The parameter block as the generated parameter struct `P`. `None`
    /// when the block is smaller than `P` or not aligned for it.
    pub fn params_as<P: Pod>(&mut self) -> Option<&mut P> {
        if self.params.len() < size_of::<P>()
            || self.params.as_ptr().align_offset(align_of::<P>()) != 0
        {
            return None;
        }
        // SAFETY: the block is large enough and aligned for `P`, uniquely
        // borrowed through `self`, and `P: Pod` accepts every byte pattern.
        Some(unsafe { &mut *self.params.as_mut_ptr().cast::<P>() })
    }

    /// In a `before` callback: do not run the game function. The parameter
    /// block is left as the callbacks wrote it. Has no effect in `after`.
    pub fn skip_original(&mut self) {
        if !self.after {
            self.skip = true;
        }
    }

    /// An executor for calling other game functions. Calls made through it
    /// do not trigger callbacks.
    #[must_use]
    pub fn executor(&self) -> &InProcessExecutor {
        self.exec
    }

    /// Names, objects and lookups.
    #[must_use]
    pub fn session(&self) -> &Session<SelfMemory> {
        self.exec.session()
    }
}

impl core::fmt::Debug for CallContext<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("CallContext")
            .field("object", &self.object)
            .field("function", &self.function)
            .field("params", &self.params.len())
            .field("after", &self.after)
            .finish_non_exhaustive()
    }
}

/// Identifies a registered callback, for [`GameThread::remove`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct HookId(u64);

type Task = Box<dyn FnOnce(&GameContext<'_>) + Send>;
type Callback = dyn Fn(&mut CallContext<'_>) + Send + Sync;

/// One registered callback.
pub(super) struct Registered<C: ?Sized> {
    pub(super) id: HookId,
    pub(super) after: bool,
    pub(super) callback: Arc<C>,
}

impl<C: ?Sized> Clone for Registered<C> {
    fn clone(&self) -> Self {
        Self {
            id: self.id,
            after: self.after,
            callback: Arc::clone(&self.callback),
        }
    }
}

/// Callbacks by function address.
pub(super) type Table<C> = HashMap<u64, Vec<Registered<C>>>;

/// A 1024-bit filter over function addresses: a clear bit proves a function
/// has no callback, without a lock.
struct Filter([AtomicU64; 16]);

impl Filter {
    const fn new() -> Self {
        Self([const { AtomicU64::new(0) }; 16])
    }

    fn bit(function: u64) -> (usize, u64) {
        let hash = (function >> 4) ^ (function >> 14) ^ (function >> 24);
        let bit = (hash & 1023) as usize;
        (bit / 64, 1u64 << (bit % 64))
    }

    fn may_contain(&self, function: u64) -> bool {
        let (word, mask) = Self::bit(function);
        self.0
            .get(word)
            .is_some_and(|w| w.load(Ordering::Acquire) & mask != 0)
    }

    fn insert(&self, function: u64) {
        let (word, mask) = Self::bit(function);
        if let Some(w) = self.0.get(word) {
            w.fetch_or(mask, Ordering::AcqRel);
        }
    }

    /// Sets exactly the bits of `functions`. A bit of a function still in
    /// the set is never cleared, even for a moment.
    fn rebuild(&self, functions: impl Iterator<Item = u64>) {
        let mut words = [0u64; 16];
        for function in functions {
            let (word, mask) = Self::bit(function);
            if let Some(w) = words.get_mut(word) {
                *w |= mask;
            }
        }
        for (w, value) in self.0.iter().zip(words) {
            w.store(value, Ordering::Release);
        }
    }
}

/// Callbacks keyed by function, read without allocating.
///
/// Writers copy the table, change the copy and publish it; a reader clones
/// the current `Arc` under a short read lock, so a callback may register or
/// remove callbacks while the table it runs from stays intact.
pub(super) struct Callbacks<C: ?Sized> {
    table: RwLock<Arc<Table<C>>>,
    filter: Filter,
    any: AtomicBool,
}

impl<C: ?Sized> Callbacks<C> {
    pub(super) fn new() -> Self {
        Self {
            table: RwLock::new(Arc::new(HashMap::new())),
            filter: Filter::new(),
            any: AtomicBool::new(false),
        }
    }

    /// `true` when any callback is registered.
    pub(super) fn any(&self) -> bool {
        self.any.load(Ordering::Acquire)
    }

    pub(super) fn add(&self, function: u64, entry: Registered<C>) {
        let mut guard = write(&self.table);
        let mut table = Table::clone(&guard);
        table.entry(function).or_default().push(entry);
        // The bit is set before the table that needs it is published.
        self.filter.insert(function);
        *guard = Arc::new(table);
        self.any.store(true, Ordering::Release);
    }

    /// Removes the callback `id`. Returns the function it was registered
    /// for, and whether that function has callbacks left.
    pub(super) fn remove(&self, id: HookId) -> Option<(u64, bool)> {
        let mut guard = write(&self.table);
        let function = guard
            .iter()
            .find(|(_, list)| list.iter().any(|r| r.id == id))
            .map(|(&function, _)| function)?;
        let mut table = Table::clone(&guard);
        let left = table.get_mut(&function).is_some_and(|list| {
            list.retain(|r| r.id != id);
            !list.is_empty()
        });
        if !left {
            table.remove(&function);
        }
        let any = !table.is_empty();
        let functions: Vec<u64> = table.keys().copied().collect();
        *guard = Arc::new(table);
        self.filter.rebuild(functions.into_iter());
        self.any.store(any, Ordering::Release);
        Some((function, left))
    }

    /// The table, when `function` may have callbacks. `None` is certain.
    pub(super) fn lookup(&self, function: u64) -> Option<Arc<Table<C>>> {
        if !self.any() || !self.filter.may_contain(function) {
            return None;
        }
        let table = Arc::clone(&read(&self.table));
        table.contains_key(&function).then_some(table)
    }
}

/// The background thread of [`GameThread::start_auto_rescan`].
struct Rescanner {
    stop: mpsc::Sender<()>,
    handle: JoinHandle<()>,
}

/// The installed `ProcessEvent` hook. There is at most one per process; get
/// it from [`GameThread::install`] or [`GameThread::get`].
pub struct GameThread {
    pub(super) desc: &'static EngineDesc,
    session: Session<SelfMemory>,
    process_event: u32,
    /// Serializes rescans, so two never patch at once.
    scan: Mutex<()>,
    patched: Mutex<Vec<usize>>,
    /// Virtual tables already looked at since the last uninstall.
    seen: Mutex<HashSet<u64>>,
    tasks: Mutex<VecDeque<Task>>,
    has_tasks: AtomicBool,
    callbacks: Callbacks<Callback>,
    pub(super) next_id: AtomicU64,
    /// Operating system id of the game thread, 0 while unknown.
    game_thread: AtomicU64,
    /// The process's main thread, preferred in the election.
    main_thread: Option<u64>,
    votes: Mutex<Vec<(u64, u32)>>,
    rescanner: Mutex<Option<Rescanner>>,
    pub(super) native: NativeHooks,
}

static STATE: OnceLock<GameThread> = OnceLock::new();
/// The `ProcessEvent` the hook forwards to.
static ORIGINAL: AtomicUsize = AtomicUsize::new(0);
/// Threads currently inside a hook of this module.
pub(super) static ACTIVE: AtomicUsize = AtomicUsize::new(0);

thread_local! {
    /// Set while user code runs, so its own calls bypass the hook.
    static BUSY: Cell<bool> = const { Cell::new(false) };
    /// Whether this thread is the game thread, once known.
    static IS_GAME: Cell<Option<bool>> = const { Cell::new(None) };
    /// The game thread's executor, created on first use there.
    static EXECUTOR: RefCell<Option<InProcessExecutor>> = const { RefCell::new(None) };
}

impl GameThread {
    /// Installs the hook, or returns the one already installed.
    ///
    /// Every live object's virtual table is read. The `ProcessEvent` slot
    /// value most of them share is taken as the engine's implementation, and
    /// that slot is redirected in each virtual table that holds it.
    ///
    /// # Safety
    ///
    /// - This code runs inside the game process described by `desc`, which
    ///   describes this exact build, including its `ProcessEvent` index.
    /// - This module stays loaded while the hook is installed and for as long
    ///   as a thread may still be running the hook after
    ///   [`uninstall`](Self::uninstall); [`uninstall_and_wait`](Self::uninstall_and_wait)
    ///   tells when that is over.
    ///
    /// # Errors
    ///
    /// [`HookError::Session`] when the session cannot be opened or the index
    /// is unknown, [`HookError::NothingToHook`] when no virtual table was
    /// found, [`HookError::Patch`] when a slot cannot be written (slots
    /// patched before the failure stay patched; call `uninstall` to undo).
    pub unsafe fn install(desc: &'static EngineDesc) -> Result<&'static Self, HookError> {
        if let Some(state) = STATE.get() {
            return Ok(state);
        }
        let process_event = desc
            .process_event_index
            .ok_or(SessionError::Unknown("ProcessEvent vtable index"))?;
        let fresh = Self {
            desc,
            session: Session::new(SelfMemory::new(), desc)?,
            process_event,
            scan: Mutex::new(()),
            patched: Mutex::new(Vec::new()),
            seen: Mutex::new(HashSet::new()),
            tasks: Mutex::new(VecDeque::new()),
            has_tasks: AtomicBool::new(false),
            callbacks: Callbacks::new(),
            next_id: AtomicU64::new(1),
            game_thread: AtomicU64::new(0),
            main_thread: os::main_thread_id(),
            votes: Mutex::new(Vec::new()),
            rescanner: Mutex::new(None),
            native: NativeHooks::new(),
        };
        let state = STATE.get_or_init(|| fresh);
        state.rescan()?;
        Ok(state)
    }

    /// The installed hook, if any.
    #[must_use]
    pub fn get() -> Option<&'static Self> {
        STATE.get()
    }

    /// Hooks virtual tables that appeared since the last scan, for example
    /// of a class that had no instance before or from a module loaded later,
    /// and re-hooks after [`uninstall`](Self::uninstall). Returns the number
    /// of slots patched.
    ///
    /// Only virtual tables not seen since the last uninstall are read, but
    /// the whole object array is walked: this takes milliseconds on a large
    /// game, so prefer a background thread
    /// ([`start_auto_rescan`](Self::start_auto_rescan)) to the game thread.
    ///
    /// # Errors
    ///
    /// As [`install`](Self::install).
    pub fn rescan(&self) -> Result<usize, HookError> {
        let _scan = lock(&self.scan);
        let mem = self.session.memory();
        let mut fresh: HashSet<u64> = HashSet::new();
        {
            let seen = lock(&self.seen);
            self.session.for_each_object(|_, object| {
                if let Ok(vtable) = mem.read_u64(object.addr())
                    && vtable != 0
                    && !seen.contains(&vtable)
                {
                    fresh.insert(vtable);
                }
                ControlFlow::Continue(())
            })?;
        }
        let hook = hooked_process_event as *const () as usize;
        let mut slots: Vec<(usize, usize)> = Vec::new();
        let mut counts: HashMap<usize, usize> = HashMap::new();
        let mut looked_at: Vec<u64> = Vec::new();
        for vtable in fresh {
            let Some(slot) = u64::from(self.process_event)
                .checked_mul(8)
                .and_then(|offset| vtable.checked_add(offset))
                .and_then(|slot| usize::try_from(slot).ok())
            else {
                continue;
            };
            let Ok(value) = mem.read_u64(slot as u64) else {
                continue;
            };
            let Ok(value) = usize::try_from(value) else {
                continue;
            };
            if value == 0 {
                continue;
            }
            looked_at.push(vtable);
            if value != hook {
                *counts.entry(value).or_default() += 1;
            }
            slots.push((slot, value));
        }
        let mut original = ORIGINAL.load(Ordering::Acquire);
        if original == 0 {
            original = counts
                .iter()
                .max_by_key(|&(&value, &count)| (count, value))
                .map(|(&value, _)| value)
                .ok_or_else(|| {
                    HookError::NothingToHook(format!(
                        "no object has a non-null virtual table slot {}",
                        self.process_event
                    ))
                })?;
            ORIGINAL.store(original, Ordering::Release);
        }
        let mut patched = lock(&self.patched);
        let mut count = 0;
        for (slot, value) in slots {
            if value != original {
                continue;
            }
            // SAFETY: `slot` is the `ProcessEvent` slot of a virtual table in
            // use, read just above; the hook forwards to `original`, which it
            // holds, with the same arguments (contract of `install`).
            let changed = unsafe { patch::swap_slot(slot, original, hook) }.map_err(|reason| {
                HookError::Patch {
                    slot: slot as u64,
                    reason,
                }
            })?;
            if changed {
                patched.push(slot);
                count += 1;
            }
        }
        lock(&self.seen).extend(looked_at);
        Ok(count)
    }

    /// Starts a background thread that calls [`rescan`](Self::rescan) every
    /// `interval` (at least [`MIN_RESCAN_INTERVAL`]), so classes that get
    /// their first instance later are hooked too. Returns `false` when one
    /// is already running. [`stop_auto_rescan`](Self::stop_auto_rescan) and
    /// [`uninstall`](Self::uninstall) stop it.
    ///
    /// # Errors
    ///
    /// [`HookError::Thread`] when the thread cannot be started.
    pub fn start_auto_rescan(&'static self, interval: Duration) -> Result<bool, HookError> {
        let interval = interval.max(MIN_RESCAN_INTERVAL);
        let mut slot = lock(&self.rescanner);
        if slot.is_some() {
            return Ok(false);
        }
        let (stop, wake) = mpsc::channel::<()>();
        let handle = thread::Builder::new()
            .name("ufathom-rescan".to_owned())
            .spawn(move || {
                let _ = catch_unwind(AssertUnwindSafe(|| {
                    while let Err(mpsc::RecvTimeoutError::Timeout) = wake.recv_timeout(interval) {
                        if lock(&self.patched).is_empty() {
                            // Uninstalled: nothing to keep up to date.
                            continue;
                        }
                        let _ = self.rescan();
                    }
                }));
            })
            .map_err(|error| HookError::Thread(error.to_string()))?;
        *slot = Some(Rescanner { stop, handle });
        Ok(true)
    }

    /// Stops the thread of [`start_auto_rescan`](Self::start_auto_rescan)
    /// and waits for it. Does nothing when none runs.
    pub fn stop_auto_rescan(&self) {
        let rescanner = lock(&self.rescanner).take();
        if let Some(Rescanner { stop, handle }) = rescanner {
            drop(stop);
            let _ = handle.join();
        }
    }

    /// `true` while the thread of [`start_auto_rescan`](Self::start_auto_rescan) runs.
    #[must_use]
    pub fn auto_rescan_running(&self) -> bool {
        lock(&self.rescanner).is_some()
    }

    /// Stops [`auto rescans`](Self::start_auto_rescan), then puts every
    /// patched slot and every [native function](Self::before_native) back. A
    /// thread already inside the hook finishes its call normally. Queued
    /// tasks and callbacks are kept and run again after
    /// [`rescan`](Self::rescan).
    ///
    /// # Errors
    ///
    /// [`HookError::Patch`] for a slot that cannot be written; the remaining
    /// slots are still restored.
    pub fn uninstall(&self) -> Result<(), HookError> {
        self.stop_auto_rescan();
        let _scan = lock(&self.scan);
        let original = ORIGINAL.load(Ordering::Acquire);
        let hook = hooked_process_event as *const () as usize;
        let mut first_error = None;
        for slot in lock(&self.patched).drain(..) {
            // SAFETY: the slot was patched by `rescan` and holds the hook;
            // the original is what it held before.
            if let Err(reason) = unsafe { patch::swap_slot(slot, hook, original) } {
                first_error.get_or_insert(HookError::Patch {
                    slot: slot as u64,
                    reason,
                });
            }
        }
        lock(&self.seen).clear();
        if let Err(error) = self.native.restore_all() {
            first_error.get_or_insert(error);
        }
        first_error.map_or(Ok(()), Err)
    }

    /// [`uninstall`](Self::uninstall)s, then waits up to `timeout` until no
    /// thread runs inside a hook of this module. Returns `true` when none
    /// does, after which the module may be unloaded; `false` when a thread
    /// was still inside (for example, in a long game function called through
    /// the hook) and unloading would be unsafe.
    ///
    /// # Errors
    ///
    /// As [`uninstall`](Self::uninstall). Nothing is waited for then.
    pub fn uninstall_and_wait(&self, timeout: Duration) -> Result<bool, HookError> {
        self.uninstall()?;
        let deadline = Instant::now() + timeout;
        loop {
            if ACTIVE.load(Ordering::Acquire) == 0 {
                // A thread may have read a patched slot just before it was
                // restored and not entered yet.
                thread::sleep(UNLOAD_GRACE);
                if ACTIVE.load(Ordering::Acquire) == 0 {
                    return Ok(true);
                }
            }
            if Instant::now() >= deadline {
                return Ok(false);
            }
            thread::sleep(Duration::from_millis(1));
        }
    }

    /// Number of virtual table slots currently patched.
    #[must_use]
    pub fn hooked_slots(&self) -> usize {
        lock(&self.patched).len()
    }

    /// `true` once the game thread has been identified.
    #[must_use]
    pub fn game_thread_known(&self) -> bool {
        self.game_thread_id().is_some()
    }

    /// The operating system id of the game thread, once identified.
    #[must_use]
    pub fn game_thread_id(&self) -> Option<u64> {
        match self.game_thread.load(Ordering::Acquire) {
            0 => None,
            id => Some(id),
        }
    }

    /// Names the game thread by its operating system id, for example one the
    /// engine's `GGameThreadId` holds, instead of letting the hook pick it.
    /// Returns `false` (and changes nothing) when the game thread is already
    /// known or `os_thread_id` is 0.
    pub fn set_game_thread(&self, os_thread_id: u64) -> bool {
        os_thread_id != 0
            && self
                .game_thread
                .compare_exchange(0, os_thread_id, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
    }

    /// The operating system id of the calling thread, as
    /// [`set_game_thread`](Self::set_game_thread) and
    /// [`game_thread_id`](Self::game_thread_id) use it.
    #[must_use]
    pub fn current_thread_id() -> u64 {
        os::current_thread_id()
    }

    /// Names, objects and lookups, usable from any thread.
    #[must_use]
    pub fn session(&self) -> &Session<SelfMemory> {
        &self.session
    }

    /// Queues `task` to run on the game thread at its next hooked
    /// `ProcessEvent` call. A panic inside the task is caught.
    pub fn run(&self, task: impl FnOnce(&GameContext<'_>) + Send + 'static) {
        lock(&self.tasks).push_back(Box::new(task));
        self.has_tasks.store(true, Ordering::Release);
    }

    /// Runs `task` on the game thread and waits up to `timeout` for its
    /// result. On the game thread itself the task runs at once.
    ///
    /// # Errors
    ///
    /// [`HookError::Timeout`] when the game thread did not get to it in time
    /// (it stays queued), [`HookError::Dropped`] when it panicked or no
    /// executor could be created.
    pub fn call<R: Send + 'static>(
        &self,
        timeout: Duration,
        task: impl FnOnce(&GameContext<'_>) -> R + Send + 'static,
    ) -> Result<R, HookError> {
        if IS_GAME.get() == Some(true) {
            return with_executor(self.desc, |exec| {
                let _busy = Busy::enter();
                catch_unwind(AssertUnwindSafe(|| task(&GameContext { exec }))).ok()
            })
            .flatten()
            .ok_or(HookError::Dropped);
        }
        let (sender, receiver) = mpsc::sync_channel(1);
        self.run(move |context| {
            let _ = sender.send(task(context));
        });
        receiver.recv_timeout(timeout).map_err(|error| match error {
            mpsc::RecvTimeoutError::Timeout => HookError::Timeout(timeout),
            mpsc::RecvTimeoutError::Disconnected => HookError::Dropped,
        })
    }

    /// Calls `callback` on the game thread before each call of the function
    /// at `path`, such as `/Script/Engine.Actor:ReceiveTick`. The callback
    /// may read and change the parameters and skip the function.
    ///
    /// # Errors
    ///
    /// [`HookError::FunctionNotFound`] or [`HookError::Session`].
    pub fn before(
        &self,
        path: &str,
        callback: impl Fn(&mut CallContext<'_>) + Send + Sync + 'static,
    ) -> Result<HookId, HookError> {
        self.register(path, false, Arc::new(callback))
    }

    /// Calls `callback` on the game thread after each call of the function at
    /// `path`, with its outputs and return value in the parameters.
    ///
    /// # Errors
    ///
    /// As [`before`](Self::before).
    pub fn after(
        &self,
        path: &str,
        callback: impl Fn(&mut CallContext<'_>) + Send + Sync + 'static,
    ) -> Result<HookId, HookError> {
        self.register(path, true, Arc::new(callback))
    }

    /// Removes a callback, of `ProcessEvent` or of a native function.
    /// Returns `false` when it was not registered.
    pub fn remove(&self, id: HookId) -> bool {
        self.callbacks.remove(id).is_some() || self.native.remove(id)
    }

    /// The `UFunction` at `path`, checked to be a function.
    pub(super) fn function(&self, path: &str) -> Result<Ptr<UObjectBase>, HookError> {
        let not_found = || HookError::FunctionNotFound(path.to_owned());
        let function = self.session.find_object(path)?.ok_or_else(not_found)?;
        let class = self.session.class_of(function)?;
        if !self.session.name_of(class)?.ends_with("Function") {
            return Err(not_found());
        }
        Ok(function)
    }

    pub(super) fn new_id(&self) -> HookId {
        HookId(self.next_id.fetch_add(1, Ordering::Relaxed))
    }

    fn register(
        &self,
        path: &str,
        after: bool,
        callback: Arc<Callback>,
    ) -> Result<HookId, HookError> {
        let function = self.function(path)?;
        let id = self.new_id();
        self.callbacks.add(
            function.addr(),
            Registered {
                id,
                after,
                callback,
            },
        );
        Ok(id)
    }

    /// Whether the current thread is the game thread, electing it when it
    /// is the main thread or the first to make enough calls.
    pub(super) fn on_game_thread(&self) -> bool {
        if let Some(known) = IS_GAME.get() {
            return known;
        }
        let me = os::current_thread_id();
        let known = self.game_thread.load(Ordering::Acquire);
        if known != 0 {
            let is_game = known == me;
            IS_GAME.set(Some(is_game));
            return is_game;
        }
        if self.main_thread == Some(me) {
            return self.elect(me);
        }
        let count = {
            let mut votes = lock(&self.votes);
            match votes.iter_mut().find(|(id, _)| *id == me) {
                Some((_, count)) => {
                    *count += 1;
                    *count
                }
                None => {
                    votes.push((me, 1));
                    1
                }
            }
        };
        count >= GAME_THREAD_VOTES && self.elect(me)
    }

    /// Makes `me` the game thread unless another was chosen first.
    fn elect(&self, me: u64) -> bool {
        let winner =
            match self
                .game_thread
                .compare_exchange(0, me, Ordering::AcqRel, Ordering::Acquire)
            {
                Ok(_) => me,
                Err(current) => current,
            };
        lock(&self.votes).clear();
        let is_game = winner == me;
        IS_GAME.set(Some(is_game));
        is_game
    }

    /// Whether this call should go through tasks and callbacks: `false` when
    /// nothing is pending or this is not the game thread.
    pub(super) fn wants(&self, pending: bool) -> bool {
        if BUSY.get() {
            return false;
        }
        if !pending && self.game_thread.load(Ordering::Acquire) != 0 {
            return false;
        }
        self.on_game_thread()
    }

    /// `true` when tasks are queued.
    pub(super) fn has_tasks(&self) -> bool {
        self.has_tasks.load(Ordering::Acquire)
    }

    /// Runs the queued tasks.
    pub(super) fn drain(&self, exec: &InProcessExecutor) {
        if !self.has_tasks.swap(false, Ordering::AcqRel) {
            return;
        }
        let tasks: Vec<Task> = lock(&self.tasks).drain(..).collect();
        let context = GameContext { exec };
        for task in tasks {
            let _ = catch_unwind(AssertUnwindSafe(|| task(&context)));
        }
    }

    /// Runs the callbacks of one kind. Returns `true` to skip the original.
    fn notify(
        &self,
        exec: &InProcessExecutor,
        list: &[Registered<Callback>],
        call: (Ptr<UObjectBase>, Ptr<UObjectBase>, *mut c_void),
        after: bool,
    ) -> bool {
        let (object, function, parms) = call;
        let len = if parms.is_null() {
            0
        } else {
            self.session.params_size(function).unwrap_or(0)
        };
        let params: &mut [u8] = if len == 0 {
            &mut []
        } else {
            // SAFETY: the engine passes a parameter block of the function's
            // `PropertiesSize` bytes, valid and exclusively ours for the
            // duration of this `ProcessEvent` call on this thread.
            unsafe { core::slice::from_raw_parts_mut(parms.cast::<u8>(), len) }
        };
        let mut context = CallContext {
            object,
            function,
            params,
            exec,
            after,
            skip: false,
        };
        for entry in list.iter().filter(|r| r.after == after) {
            let _ = catch_unwind(AssertUnwindSafe(|| (entry.callback)(&mut context)));
        }
        context.skip
    }
}

impl core::fmt::Debug for GameThread {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("GameThread")
            .field("process_event", &self.process_event)
            .field("hooked_slots", &self.hooked_slots())
            .field("game_thread", &self.game_thread_id())
            .field("auto_rescan", &self.auto_rescan_running())
            .finish_non_exhaustive()
    }
}

/// Marks user code as running on this thread until dropped.
pub(super) struct Busy {
    was: bool,
}

impl Busy {
    pub(super) fn enter() -> Self {
        Self {
            was: BUSY.replace(true),
        }
    }
}

impl Drop for Busy {
    fn drop(&mut self) {
        BUSY.set(self.was);
    }
}

/// Counts a thread inside a hook of this module until dropped.
pub(super) struct Active;

impl Active {
    pub(super) fn enter() -> Self {
        ACTIVE.fetch_add(1, Ordering::AcqRel);
        Self
    }
}

impl Drop for Active {
    fn drop(&mut self) {
        ACTIVE.fetch_sub(1, Ordering::AcqRel);
    }
}

/// Runs `f` with this thread's executor, creating it on first use.
pub(super) fn with_executor<R>(
    desc: &'static EngineDesc,
    f: impl FnOnce(&InProcessExecutor) -> R,
) -> Option<R> {
    EXECUTOR.with(|cell| {
        let mut slot = cell.try_borrow_mut().ok()?;
        if slot.is_none() {
            // SAFETY: only called on the game thread inside the game that
            // `desc` describes (contract of `GameThread::install`).
            *slot = unsafe { InProcessExecutor::attach(desc) }.ok();
        }
        let exec = slot.take()?;
        drop(slot);
        let result = f(&exec);
        // Put it back; a nested use in `f` found the slot empty and made none.
        if let Ok(mut slot) = cell.try_borrow_mut() {
            *slot = Some(exec);
        }
        Some(result)
    })
}

/// The hook written into virtual tables in place of `ProcessEvent`.
unsafe extern "C" fn hooked_process_event(
    this: *mut c_void,
    function: *mut c_void,
    parms: *mut c_void,
) {
    let _active = Active::enter();
    let original = ORIGINAL.load(Ordering::Acquire);
    // SAFETY: `original` is the `ProcessEvent` the patched slots held, set
    // before any slot was patched.
    let forward = || unsafe {
        core::mem::transmute::<usize, ProcessEventFn>(original)(this, function, parms);
    };
    let Some(state) = STATE.get() else {
        return forward();
    };
    let function_addr = function as usize as u64;
    let table = state.callbacks.lookup(function_addr);
    if !state.wants(table.is_some() || state.has_tasks()) {
        return forward();
    }
    let hooks = table.as_ref().and_then(|t| t.get(&function_addr));
    let call = (
        Ptr::<UObjectBase>::new(this as usize as u64),
        Ptr::<UObjectBase>::new(function_addr),
        parms,
    );
    let skip = {
        let _busy = Busy::enter();
        with_executor(state.desc, |exec| {
            state.drain(exec);
            hooks.is_some_and(|list| state.notify(exec, list, call, false))
        })
        .unwrap_or(false)
    };
    if !skip {
        forward();
    }
    if let Some(list) = hooks
        && list.iter().any(|r| r.after)
    {
        let _busy = Busy::enter();
        with_executor(state.desc, |exec| {
            state.notify(exec, list, call, true);
        });
    }
}

pub(super) fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn read<T>(lock: &RwLock<T>) -> std::sync::RwLockReadGuard<'_, T> {
    lock.read().unwrap_or_else(PoisonError::into_inner)
}

fn write<T>(lock: &RwLock<T>) -> std::sync::RwLockWriteGuard<'_, T> {
    lock.write().unwrap_or_else(PoisonError::into_inner)
}
