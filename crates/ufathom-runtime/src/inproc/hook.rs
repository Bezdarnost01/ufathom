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
//! In the default [`HookMode::Vtable`] no machine code is patched: only data
//! pointers in virtual tables change, and [`GameThread::uninstall`] puts them
//! back. [`GameThread::install_with`] can instead detour the engine's
//! `ProcessEvent` itself, once ([`HookMode::Detour`]); the same entry point,
//! [`hooked_process_event`], serves both.
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
//! In [`HookMode::Detour`] there is nothing to catch up on: every call of the
//! engine's implementation passes through the one detour. The rest of this
//! section is about [`HookMode::Vtable`].
//!
//! A virtual table is found through a live object of its class, so a native
//! class with no instance at install time is not hooked yet.
//! [`GameThread::rescan`] hooks what appeared since. Two background threads
//! can call it for you: [`GameThread::start_rescan_on_module_load`] does so
//! shortly after the loader maps a module, which is when new native classes
//! appear; [`GameThread::start_auto_rescan`] does so on a fixed interval, for
//! classes that get their first instance with no module load, and on
//! platforms without load notifications.
//!
//! # Owning a hook
//!
//! [`HookId::into_guard`] turns an id into a [`HookGuard`](super::HookGuard)
//! that removes the callback when dropped. Every callback and queued task also
//! records the module that holds its code, so
//! [`GameThread::remove_owned_by`] can remove everything one module
//! registered before that module is unloaded.
//!
//! # Unloading
//!
//! [`GameThread::uninstall_and_wait`] restores every slot (or the detoured
//! bytes) and then waits until no thread runs inside the hook, so the module
//! that holds it can be unloaded.
//!
//! Mods that each link their own copy of this crate can hook the same slot:
//! the one that hooked second saved the first one's hook as its original. The
//! first one then cannot restore the slot, and unloading it would leave the
//! second one forwarding into unmapped code. Such a slot is left as it is and
//! reported as [`HookError::Chained`]; the hook keeps forwarding, and
//! [`uninstall`](GameThread::uninstall) can be called again once the other
//! module has removed its hook. The same holds for the detour: bytes patched
//! over by another tool are left alone and reported as [`HookError::Chained`].

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

use super::guard::owner_of_val;
use super::mode::EngineDetour;
use super::modload::ModuleLoadWatch;
use super::native::NativeHooks;
use super::patch::Restored;
use super::{HookMode, InProcessExecutor, ProcessEventFn, os, patch};
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
    /// A slot could not be restored because another module patched it after
    /// this one, and its hook forwards into this one. The slot is left as it
    /// is and this module's hook keeps forwarding to the original, but this
    /// module must stay loaded until the other one has removed its hook; then
    /// [`GameThread::uninstall`] can be called again.
    #[error(
        "{blocked} slot(s) cannot be restored because another module hooked them after this one \
         (the first, {slot:#x}, holds {holder:#x}); keep this module loaded"
    )]
    Chained {
        /// Address of the first slot left unrestored.
        slot: u64,
        /// What that slot holds: the other module's hook.
        holder: u64,
        /// Slots left unrestored, `ProcessEvent` slots and `UFunction::Func`
        /// fields together. For a detoured `ProcessEvent`, `slot` is the
        /// function, `holder` is where the foreign jump there leads (0 when
        /// the bytes are not a `jmp rel32`) and this counts the detour as one.
        blocked: usize,
    },
    /// The notification that reports module loads could not be registered or
    /// removed.
    #[error("module load notification: {0}")]
    ModuleNotification(String),
    /// [`HookMode::Detour`] was asked for in a build that cannot patch code:
    /// it needs the `detour` feature of `ufathom-runtime`, on Windows x86-64.
    #[error("HookMode::Detour needs the `detour` feature, on Windows x86-64")]
    DetourUnavailable,
    /// The engine's `ProcessEvent` could not be detoured, or its detour could
    /// not be removed. The reason is the detour module's own error.
    #[error("cannot detour ProcessEvent at {target:#x}: {reason}")]
    Detour {
        /// Address of the function.
        target: u64,
        /// What the safety analysis or the operating system said.
        reason: String,
    },
    /// A hook is installed already, in another mode than the one asked for.
    #[error("the hook is installed in {installed} mode, not {requested} mode")]
    ModeConflict {
        /// The mode of the installed hook.
        installed: HookMode,
        /// The mode asked for.
        requested: HookMode,
    },
}

/// Slots that could not be put back by an uninstall.
#[derive(Debug, Default)]
pub(super) struct RestoreReport {
    /// (slot, what it holds) of each slot another module patched afterwards.
    pub(super) chained: Vec<(u64, u64)>,
    /// The first slot that could not be written at all.
    pub(super) error: Option<HookError>,
}

impl RestoreReport {
    /// Adds what `other` found.
    pub(super) fn merge(&mut self, other: Self) {
        self.chained.extend(other.chained);
        if self.error.is_none() {
            self.error = other.error;
        }
    }

    /// `Ok` when every slot was restored; a write error first, else
    /// [`HookError::Chained`].
    pub(super) fn into_result(self) -> Result<(), HookError> {
        if let Some(error) = self.error {
            return Err(error);
        }
        match self.chained.first() {
            Some(&(slot, holder)) => Err(HookError::Chained {
                slot,
                holder,
                blocked: self.chained.len(),
            }),
            None => Ok(()),
        }
    }
}

/// The `ProcessEvent` slots of the virtual tables looked at.
#[derive(Default)]
struct Vote {
    /// (address of the slot, what it holds) for each virtual table.
    slots: Vec<(usize, usize)>,
    /// The virtual tables those slots belong to.
    looked_at: Vec<u64>,
}

impl Vote {
    /// The value most slots hold, not counting `exclude` (the hook itself),
    /// the larger one on a tie.
    fn majority(&self, exclude: usize) -> Option<usize> {
        let mut counts: HashMap<usize, usize> = HashMap::new();
        for &(_, value) in &self.slots {
            if value != exclude {
                *counts.entry(value).or_default() += 1;
            }
        }
        counts
            .into_iter()
            .max_by_key(|&(value, count)| (count, value))
            .map(|(value, _)| value)
    }
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
    /// Taken out and put back by the typed hooks, which lend it as a `P`.
    pub(super) params: &'a mut [u8],
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
        super::typed::cast_block(self.params)
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

/// Identifies a registered callback, for [`GameThread::remove`]. Use
/// [`into_guard`](Self::into_guard) to have it removed automatically.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct HookId(u64);

type Task = Box<dyn FnOnce(&GameContext<'_>) + Send>;
type Callback = dyn Fn(&mut CallContext<'_>) + Send + Sync;

/// One registered callback.
pub(super) struct Registered<C: ?Sized> {
    pub(super) id: HookId,
    pub(super) after: bool,
    /// Base address of the module that holds the callback's code, see
    /// [`GameThread::remove_owned_by`].
    pub(super) module: usize,
    pub(super) callback: Arc<C>,
}

impl<C: ?Sized> Clone for Registered<C> {
    fn clone(&self) -> Self {
        Self {
            id: self.id,
            after: self.after,
            module: self.module,
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

    /// Removes every callback registered by code in `module`. Returns how
    /// many were removed and the functions left with no callback.
    pub(super) fn remove_owned_by(&self, module: usize) -> (usize, Vec<u64>) {
        let mut guard = write(&self.table);
        if !guard.values().flatten().any(|r| r.module == module) {
            return (0, Vec::new());
        }
        let mut table = Table::clone(&guard);
        let mut removed = 0usize;
        let mut emptied = Vec::new();
        table.retain(|&function, list| {
            let had = list.len();
            list.retain(|r| r.module != module);
            removed += had - list.len();
            if list.is_empty() {
                emptied.push(function);
            }
            !list.is_empty()
        });
        let any = !table.is_empty();
        let functions: Vec<u64> = table.keys().copied().collect();
        *guard = Arc::new(table);
        self.filter.rebuild(functions.into_iter());
        self.any.store(any, Ordering::Release);
        (removed, emptied)
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
    pub(super) mode: HookMode,
    session: Session<SelfMemory>,
    process_event: u32,
    /// Serializes rescans, so two never patch at once.
    scan: Mutex<()>,
    patched: Mutex<Vec<usize>>,
    /// Virtual tables already looked at since the last uninstall.
    seen: Mutex<HashSet<u64>>,
    /// Queued tasks with the module that holds each one's code.
    pub(super) tasks: Mutex<VecDeque<(usize, Task)>>,
    has_tasks: AtomicBool,
    pub(super) callbacks: Callbacks<Callback>,
    pub(super) next_id: AtomicU64,
    /// Operating system id of the game thread, 0 while unknown.
    game_thread: AtomicU64,
    /// The process's main thread, preferred in the election.
    main_thread: Option<u64>,
    votes: Mutex<Vec<(u64, u32)>>,
    rescanner: Mutex<Option<Rescanner>>,
    pub(super) modload: ModuleLoadWatch,
    pub(super) native: NativeHooks,
    /// The detour of [`HookMode::Detour`].
    detour: EngineDetour,
}

static STATE: OnceLock<GameThread> = OnceLock::new();
/// The `ProcessEvent` the hook forwards to: the engine's implementation, or
/// in [`HookMode::Detour`] the trampoline that runs it.
pub(super) static ORIGINAL: AtomicUsize = AtomicUsize::new(0);
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
    /// Installs the hook in [`HookMode::Vtable`], or returns the one already
    /// installed. See [`install_with`](Self::install_with) to choose the mode.
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
    /// patched before the failure stay patched; call `uninstall` to undo),
    /// [`HookError::ModeConflict`] when the installed hook is a detour.
    pub unsafe fn install(desc: &'static EngineDesc) -> Result<&'static Self, HookError> {
        // SAFETY: the contract is the caller's.
        unsafe { Self::install_with(desc, HookMode::Vtable) }
    }

    /// Installs the hook in `mode`, or returns the one already installed.
    ///
    /// Both modes take the vote [`install`](Self::install) describes to find
    /// the engine's `ProcessEvent`. [`HookMode::Vtable`] then swaps the slot in
    /// every virtual table that holds it and patches no code.
    /// [`HookMode::Detour`] detours the function itself, once: the same
    /// callbacks, tasks and game thread detection run, for every caller of
    /// the engine's implementation, including classes with no live object
    /// yet and classes that override `ProcessEvent` and call the base
    /// implementation. A class whose override never calls the base is not
    /// seen.
    ///
    /// A detour usually has an author's signature to check the target
    /// against. Here the target is the vote's result, so its own current bytes
    /// are the expected ones: the mode trusts the vote, and relies on the
    /// [`detour`](crate::detour) module's analysis to refuse a function it
    /// cannot move safely (`ret`, a trap, a branch into the stolen bytes).
    /// That analysis cannot tell that the vote elected the wrong function, or
    /// that another tool patched the bytes first.
    ///
    /// Asking again for the mode that is installed returns the hook, and for
    /// the detour mode installs the detour if an earlier attempt failed. An
    /// uninstalled detour is not installed again.
    ///
    /// # Safety
    ///
    /// As [`install`](Self::install). For [`HookMode::Detour`] also: the
    /// majority `ProcessEvent` slot value is the start of the engine's
    /// `UObject::ProcessEvent` (true of every Unreal Engine build, where the
    /// one implementation is shared by nearly every class), and no other tool
    /// is patching that function at this moment.
    ///
    /// # Errors
    ///
    /// As [`install`](Self::install), and [`HookError::DetourUnavailable`]
    /// when `mode` is [`HookMode::Detour`] and the build has no `detour`
    /// feature or is not Windows x86-64, [`HookError::Detour`] when the
    /// function cannot be detoured, [`HookError::ModeConflict`] when a hook is
    /// installed in the other mode.
    pub unsafe fn install_with(
        desc: &'static EngineDesc,
        mode: HookMode,
    ) -> Result<&'static Self, HookError> {
        if mode == HookMode::Detour && !EngineDetour::AVAILABLE {
            return Err(HookError::DetourUnavailable);
        }
        let existing = STATE.get();
        let state = match existing {
            Some(state) => state,
            None => {
                let process_event = desc
                    .process_event_index
                    .ok_or(SessionError::Unknown("ProcessEvent vtable index"))?;
                let fresh = Self {
                    desc,
                    mode,
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
                    modload: ModuleLoadWatch::new(),
                    native: NativeHooks::new(),
                    detour: EngineDetour::new(),
                };
                STATE.get_or_init(|| fresh)
            }
        };
        if state.mode != mode {
            return Err(HookError::ModeConflict {
                installed: state.mode,
                requested: mode,
            });
        }
        match mode {
            HookMode::Vtable => {
                if existing.is_none() {
                    state.rescan()?;
                }
            }
            HookMode::Detour => state.install_detour()?,
        }
        Ok(state)
    }

    /// The installed hook, if any.
    #[must_use]
    pub fn get() -> Option<&'static Self> {
        STATE.get()
    }

    /// How the installed hook intercepts `ProcessEvent`.
    #[must_use]
    pub fn mode(&self) -> HookMode {
        self.mode
    }

    /// The function the detour of [`HookMode::Detour`] is patched into, while
    /// it is. `None` in the other mode, before the detour exists and after it
    /// was removed.
    #[must_use]
    pub fn detour_target(&self) -> Option<usize> {
        self.detour.target()
    }

    /// Hooks virtual tables that appeared since the last scan, for example
    /// of a class that had no instance before or from a module loaded later,
    /// and re-hooks after [`uninstall`](Self::uninstall). Returns the number
    /// of slots patched.
    ///
    /// Only virtual tables not seen since the last uninstall are read, but
    /// the whole object array is walked: this takes milliseconds on a large
    /// game, so prefer a background thread
    /// ([`start_rescan_on_module_load`](Self::start_rescan_on_module_load) or
    /// [`start_auto_rescan`](Self::start_auto_rescan)) to the game thread.
    ///
    /// In [`HookMode::Detour`] there is nothing to catch up on, since the
    /// engine's function itself is hooked: it does nothing and returns
    /// `Ok(0)`.
    ///
    /// # Errors
    ///
    /// As [`install`](Self::install).
    pub fn rescan(&self) -> Result<usize, HookError> {
        if self.mode == HookMode::Detour {
            return Ok(0);
        }
        let _scan = lock(&self.scan);
        let vote = self.collect_vote()?;
        let hook = hooked_process_event as *const () as usize;
        let mut original = ORIGINAL.load(Ordering::Acquire);
        if original == 0 {
            original = vote.majority(hook).ok_or_else(|| self.nothing_to_hook())?;
            ORIGINAL.store(original, Ordering::Release);
        }
        let mut patched = lock(&self.patched);
        let mut count = 0;
        for &(slot, value) in &vote.slots {
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
        lock(&self.seen).extend(vote.looked_at);
        self.modload.count_rescan();
        Ok(count)
    }

    /// Reads the `ProcessEvent` slot of every virtual table not seen since
    /// the last uninstall.
    fn collect_vote(&self) -> Result<Vote, HookError> {
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
        let mut vote = Vote::default();
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
            vote.looked_at.push(vtable);
            vote.slots.push((slot, value));
        }
        Ok(vote)
    }

    fn nothing_to_hook(&self) -> HookError {
        HookError::NothingToHook(format!(
            "no object has a non-null virtual table slot {}",
            self.process_event
        ))
    }

    /// Detours the engine's `ProcessEvent` for [`HookMode::Detour`], unless
    /// that was done.
    fn install_detour(&self) -> Result<(), HookError> {
        let _scan = lock(&self.scan);
        // SAFETY: the vote's winner is the engine's `ProcessEvent` and the
        // replacement forwards to the trampoline, which the detour stores in
        // `ORIGINAL` before it patches (contract of `install_with`).
        unsafe {
            self.detour.install(|| {
                self.collect_vote()?
                    .majority(0)
                    .ok_or_else(|| self.nothing_to_hook())
            })
        }
    }

    /// Starts a background thread that calls [`rescan`](Self::rescan) every
    /// `interval` (at least [`MIN_RESCAN_INTERVAL`]), so classes that get
    /// their first instance later are hooked too. Returns `false` when one
    /// is already running, and in [`HookMode::Detour`], where there is
    /// nothing to rescan and no thread is started.
    /// [`stop_auto_rescan`](Self::stop_auto_rescan) and
    /// [`uninstall`](Self::uninstall) stop it.
    ///
    /// Prefer [`start_rescan_on_module_load`](Self::start_rescan_on_module_load)
    /// on Windows: a native class a module adds is registered shortly after
    /// that module loads, and Blueprint classes reuse the virtual tables of
    /// their native parents, so a rescan after each load finds what a timer
    /// finds, at no cost between loads. Use this one on other platforms, or in
    /// addition when a class can get its first instance with no module load,
    /// for example a native class registered lazily by the game itself.
    ///
    /// # Errors
    ///
    /// [`HookError::Thread`] when the thread cannot be started.
    pub fn start_auto_rescan(&'static self, interval: Duration) -> Result<bool, HookError> {
        if self.mode == HookMode::Detour {
            return Ok(false);
        }
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

    /// Stops the [background rescans](Self::start_rescan_on_module_load), then
    /// puts every patched slot (or, in [`HookMode::Detour`], the detoured
    /// bytes) and every [native function](Self::before_native) back. A thread
    /// already inside the hook finishes its call normally.
    /// Queued tasks and callbacks are kept and run again after
    /// [`rescan`](Self::rescan); use [`remove_owned_by`](Self::remove_owned_by)
    /// to drop those of a module that is about to be unloaded.
    ///
    /// A slot that another module hooked after this one is not touched, see
    /// the module docs; nor is a detour whose bytes another tool patched over. It stays registered with this hook, which
    /// keeps forwarding (and running its callbacks) until the other module
    /// removes its own hook; call `uninstall` again after that.
    ///
    /// # Errors
    ///
    /// [`HookError::Patch`] for a slot that cannot be written,
    /// [`HookError::Chained`] for slots another module hooked afterwards,
    /// [`HookError::ModuleNotification`] when the module load notification
    /// cannot be removed. The remaining slots are still restored; the module
    /// must not be unloaded after any of these errors.
    pub fn uninstall(&self) -> Result<(), HookError> {
        self.stop_auto_rescan();
        let notification = self.modload.stop();
        let _scan = lock(&self.scan);
        let original = ORIGINAL.load(Ordering::Acquire);
        let hook = hooked_process_event as *const () as usize;
        let mut report = RestoreReport::default();
        {
            let mut patched = lock(&self.patched);
            let mut kept = Vec::new();
            for slot in patched.drain(..) {
                // SAFETY: the slot was patched by `rescan` and holds the hook
                // unless another module patched it since; the original is what
                // it held before.
                match unsafe { patch::restore_slot(slot, hook, original) } {
                    Ok(Restored::Done) => {}
                    Ok(Restored::Chained { holder }) => {
                        report.chained.push((slot as u64, holder as u64));
                        kept.push(slot);
                    }
                    Err(reason) => {
                        report.error.get_or_insert(HookError::Patch {
                            slot: slot as u64,
                            reason,
                        });
                        kept.push(slot);
                    }
                }
            }
            // A slot not restored is still ours: a later call retries it.
            *patched = kept;
        }
        lock(&self.seen).clear();
        match self.detour.uninstall() {
            Ok(()) => {}
            Err(HookError::Chained { slot, holder, .. }) => report.chained.push((slot, holder)),
            Err(error) => {
                report.error.get_or_insert(error);
            }
        }
        report.merge(self.native.restore_all());
        notification?;
        report.into_result()
    }

    /// [`uninstall`](Self::uninstall)s, then waits up to `timeout` until no
    /// thread runs inside a hook of this module. Returns `true` when none
    /// does, after which the module may be unloaded; `false` when a thread
    /// was still inside (for example, in a long game function called through
    /// the hook) and unloading would be unsafe.
    ///
    /// In [`HookMode::Detour`] it waits for the hook's own count and for the
    /// detour's (threads in its replacement, relay or trampoline), then frees
    /// the detour's memory. The detour's wait may take a little longer than
    /// `timeout` after the hook's own was spent.
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
                    // The detour needs a quiet period of its own.
                    let left = deadline
                        .saturating_duration_since(Instant::now())
                        .max(UNLOAD_GRACE * 4);
                    return self.detour.release(left);
                }
            }
            if Instant::now() >= deadline {
                return Ok(false);
            }
            thread::sleep(Duration::from_millis(1));
        }
    }

    /// Number of virtual table slots currently patched. Always 0 in
    /// [`HookMode::Detour`], which patches none; see
    /// [`detour_target`](Self::detour_target).
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
        let module = owner_of_val(&task);
        lock(&self.tasks).push_back((module, Box::new(task)));
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
    /// The module that holds the callback's code becomes its owner, see
    /// [`remove_owned_by`](Self::remove_owned_by). Turn the id into a
    /// [`HookGuard`](super::HookGuard) with [`HookId::into_guard`] to remove
    /// the callback automatically.
    ///
    /// # Errors
    ///
    /// [`HookError::FunctionNotFound`] or [`HookError::Session`].
    pub fn before(
        &self,
        path: &str,
        callback: impl Fn(&mut CallContext<'_>) + Send + Sync + 'static,
    ) -> Result<HookId, HookError> {
        let module = owner_of_val(&callback);
        self.register(path, false, Arc::new(callback), module)
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
        let module = owner_of_val(&callback);
        self.register(path, true, Arc::new(callback), module)
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
        module: usize,
    ) -> Result<HookId, HookError> {
        let function = self.function(path)?;
        let id = self.new_id();
        self.callbacks.add(
            function.addr(),
            Registered {
                id,
                after,
                module,
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
        let tasks: Vec<Task> = lock(&self.tasks).drain(..).map(|(_, task)| task).collect();
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
            .field("mode", &self.mode)
            .field("process_event", &self.process_event)
            .field("hooked_slots", &self.hooked_slots())
            .field("game_thread", &self.game_thread_id())
            .field("auto_rescan", &self.auto_rescan_running())
            .field(
                "rescan_on_module_load",
                &self.rescan_on_module_load_running(),
            )
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

/// The hook written into virtual tables in place of `ProcessEvent`, and the
/// target of the detour in [`HookMode::Detour`]: both modes run this one body,
/// which forwards to whatever [`ORIGINAL`] holds.
pub(super) unsafe extern "C" fn hooked_process_event(
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

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, reason = "tests may panic")]

    use super::*;

    /// A callback table over plain values, since only the bookkeeping is
    /// under test.
    fn entry(id: u64, module: usize) -> Registered<u32> {
        Registered {
            id: HookId(id),
            after: false,
            module,
            callback: Arc::new(0),
        }
    }

    #[test]
    fn removing_by_module_leaves_other_modules_alone() {
        const A: usize = 0x1000_0000;
        const B: usize = 0x2000_0000;
        let callbacks = Callbacks::<u32>::new();
        callbacks.add(0x10, entry(1, A));
        callbacks.add(0x10, entry(2, B));
        callbacks.add(0x20, entry(3, A));
        callbacks.add(0x30, entry(4, B));

        assert_eq!(callbacks.remove_owned_by(0x3000_0000), (0, Vec::new()));
        let (removed, mut emptied) = callbacks.remove_owned_by(A);
        emptied.sort_unstable();
        assert_eq!(removed, 2);
        assert_eq!(emptied, vec![0x20], "0x10 still has B's callback");
        assert!(callbacks.any());
        let table = callbacks.lookup(0x10).expect("B's callback is left");
        assert_eq!(table.get(&0x10).map(Vec::len), Some(1));
        assert!(callbacks.lookup(0x20).is_none());

        assert_eq!(callbacks.remove_owned_by(B).0, 2);
        assert!(!callbacks.any());
        assert!(callbacks.lookup(0x10).is_none());
        // Removing an id of a removed callback finds nothing.
        assert_eq!(callbacks.remove(HookId(1)), None);
    }
}
