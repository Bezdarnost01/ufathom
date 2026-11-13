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

use core::ffi::c_void;
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet, VecDeque};
use std::ops::ControlFlow;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError, RwLock, mpsc};
use std::thread::{self, ThreadId};
use std::time::Duration;

use ufathom_mem::{MemoryReaderExt, Pod, SelfMemory};

use super::{InProcessExecutor, ProcessEventFn, patch};
use crate::desc::EngineDesc;
use crate::object::UObjectBase;
use crate::ptr::Ptr;
use crate::session::{Session, SessionError};

/// `ProcessEvent` calls a thread must make before it is taken as the game
/// thread.
const GAME_THREAD_VOTES: u32 = 64;

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
type Callback = Arc<dyn Fn(&mut CallContext<'_>) + Send + Sync>;

struct Registered {
    id: HookId,
    after: bool,
    callback: Callback,
}

/// The installed `ProcessEvent` hook. There is at most one per process; get
/// it from [`GameThread::install`] or [`GameThread::get`].
pub struct GameThread {
    desc: &'static EngineDesc,
    session: Session<SelfMemory>,
    process_event: u32,
    patched: Mutex<Vec<usize>>,
    tasks: Mutex<VecDeque<Task>>,
    has_tasks: AtomicBool,
    callbacks: RwLock<HashMap<u64, Vec<Registered>>>,
    has_callbacks: AtomicBool,
    next_id: AtomicU64,
    game_thread: OnceLock<ThreadId>,
    votes: Mutex<Vec<(ThreadId, u32)>>,
}

static STATE: OnceLock<GameThread> = OnceLock::new();
/// The `ProcessEvent` the hook forwards to.
static ORIGINAL: AtomicUsize = AtomicUsize::new(0);

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
    ///   [`uninstall`](Self::uninstall).
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
            patched: Mutex::new(Vec::new()),
            tasks: Mutex::new(VecDeque::new()),
            has_tasks: AtomicBool::new(false),
            callbacks: RwLock::new(HashMap::new()),
            has_callbacks: AtomicBool::new(false),
            next_id: AtomicU64::new(1),
            game_thread: OnceLock::new(),
            votes: Mutex::new(Vec::new()),
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
    /// from a module loaded later, and re-hooks after
    /// [`uninstall`](Self::uninstall). Returns the number of slots patched.
    ///
    /// # Errors
    ///
    /// As [`install`](Self::install).
    pub fn rescan(&self) -> Result<usize, HookError> {
        let mem = self.session.memory();
        let mut vtables: HashSet<u64> = HashSet::new();
        self.session.for_each_object(|_, object| {
            if let Ok(vtable) = mem.read_u64(object.addr())
                && vtable != 0
            {
                vtables.insert(vtable);
            }
            ControlFlow::Continue(())
        })?;
        let hook = hooked_process_event as *const () as usize;
        let mut slots: Vec<(usize, usize)> = Vec::new();
        let mut counts: HashMap<usize, usize> = HashMap::new();
        for vtable in vtables {
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
        Ok(count)
    }

    /// Puts every patched slot back. A thread already inside the hook
    /// finishes its call normally. Queued tasks and callbacks are kept and
    /// run again after [`rescan`](Self::rescan).
    ///
    /// # Errors
    ///
    /// [`HookError::Patch`] for a slot that cannot be written; the remaining
    /// slots are still restored.
    pub fn uninstall(&self) -> Result<(), HookError> {
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
        first_error.map_or(Ok(()), Err)
    }

    /// Number of virtual table slots currently patched.
    #[must_use]
    pub fn hooked_slots(&self) -> usize {
        lock(&self.patched).len()
    }

    /// `true` once the game thread has been identified: the first thread to
    /// make 64 hooked `ProcessEvent` calls.
    #[must_use]
    pub fn game_thread_known(&self) -> bool {
        self.game_thread.get().is_some()
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

    /// Removes a callback. Returns `false` when it was not registered.
    pub fn remove(&self, id: HookId) -> bool {
        let mut callbacks = write(&self.callbacks);
        let mut found = false;
        callbacks.retain(|_, list| {
            let before = list.len();
            list.retain(|r| r.id != id);
            found |= list.len() != before;
            !list.is_empty()
        });
        self.has_callbacks
            .store(!callbacks.is_empty(), Ordering::Release);
        found
    }

    fn register(&self, path: &str, after: bool, callback: Callback) -> Result<HookId, HookError> {
        let not_found = || HookError::FunctionNotFound(path.to_owned());
        let function = self.session.find_object(path)?.ok_or_else(not_found)?;
        let class = self.session.class_of(function)?;
        if !self.session.name_of(class)?.ends_with("Function") {
            return Err(not_found());
        }
        let id = HookId(self.next_id.fetch_add(1, Ordering::Relaxed));
        write(&self.callbacks)
            .entry(function.addr())
            .or_default()
            .push(Registered {
                id,
                after,
                callback,
            });
        self.has_callbacks.store(true, Ordering::Release);
        Ok(id)
    }

    /// Whether the current thread is the game thread, electing it on the
    /// first thread to make enough calls.
    fn on_game_thread(&self) -> bool {
        if let Some(known) = IS_GAME.get() {
            return known;
        }
        let me = thread::current().id();
        if let Some(&game) = self.game_thread.get() {
            let known = game == me;
            IS_GAME.set(Some(known));
            return known;
        }
        let mut votes = lock(&self.votes);
        let count = match votes.iter_mut().find(|(id, _)| *id == me) {
            Some((_, count)) => {
                *count += 1;
                *count
            }
            None => {
                votes.push((me, 1));
                1
            }
        };
        if count >= GAME_THREAD_VOTES {
            let elected = *self.game_thread.get_or_init(|| me) == me;
            votes.clear();
            return elected;
        }
        false
    }

    /// Runs the queued tasks.
    fn drain(&self, exec: &InProcessExecutor) {
        if !self.has_tasks.swap(false, Ordering::AcqRel) {
            return;
        }
        let tasks: Vec<Task> = lock(&self.tasks).drain(..).collect();
        let context = GameContext { exec };
        for task in tasks {
            let _ = catch_unwind(AssertUnwindSafe(|| task(&context)));
        }
    }

    /// The callbacks for `function` of one kind, copied out of the lock so a
    /// callback may register or remove others.
    fn callbacks_for(&self, function: u64, after: bool) -> Vec<Callback> {
        if !self.has_callbacks.load(Ordering::Acquire) {
            return Vec::new();
        }
        read(&self.callbacks)
            .get(&function)
            .map(|list| {
                list.iter()
                    .filter(|r| r.after == after)
                    .map(|r| Arc::clone(&r.callback))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Runs the callbacks of one kind. Returns `true` to skip the original.
    fn notify(
        &self,
        exec: &InProcessExecutor,
        callbacks: &[Callback],
        object: Ptr<UObjectBase>,
        function: Ptr<UObjectBase>,
        parms: *mut c_void,
        after: bool,
    ) -> bool {
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
        for callback in callbacks {
            let _ = catch_unwind(AssertUnwindSafe(|| callback(&mut context)));
        }
        context.skip
    }
}

impl core::fmt::Debug for GameThread {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("GameThread")
            .field("process_event", &self.process_event)
            .field("hooked_slots", &self.hooked_slots())
            .field("game_thread_known", &self.game_thread_known())
            .finish_non_exhaustive()
    }
}

/// Marks user code as running on this thread until dropped.
struct Busy {
    was: bool,
}

impl Busy {
    fn enter() -> Self {
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

/// Runs `f` with this thread's executor, creating it on first use.
fn with_executor<R>(
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
    let original = ORIGINAL.load(Ordering::Acquire);
    // SAFETY: `original` is the `ProcessEvent` the patched slots held, set
    // before any slot was patched.
    let forward = || unsafe {
        core::mem::transmute::<usize, ProcessEventFn>(original)(this, function, parms);
    };
    let state = match STATE.get() {
        Some(state) if !BUSY.get() => state,
        _ => return forward(),
    };
    let quiet =
        !state.has_tasks.load(Ordering::Acquire) && !state.has_callbacks.load(Ordering::Acquire);
    if (quiet && state.game_thread.get().is_some()) || !state.on_game_thread() {
        return forward();
    }

    let object = Ptr::<UObjectBase>::new(this as usize as u64);
    let function_ptr = Ptr::<UObjectBase>::new(function as usize as u64);
    let before = state.callbacks_for(function_ptr.addr(), false);
    let after = state.callbacks_for(function_ptr.addr(), true);
    let skip = {
        let _busy = Busy::enter();
        with_executor(state.desc, |exec| {
            state.drain(exec);
            !before.is_empty() && state.notify(exec, &before, object, function_ptr, parms, false)
        })
        .unwrap_or(false)
    };
    if !skip {
        forward();
    }
    if !after.is_empty() {
        let _busy = Busy::enter();
        with_executor(state.desc, |exec| {
            state.notify(exec, &after, object, function_ptr, parms, true);
        });
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn read<T>(lock: &RwLock<T>) -> std::sync::RwLockReadGuard<'_, T> {
    lock.read().unwrap_or_else(PoisonError::into_inner)
}

fn write<T>(lock: &RwLock<T>) -> std::sync::RwLockWriteGuard<'_, T> {
    lock.write().unwrap_or_else(PoisonError::into_inner)
}
