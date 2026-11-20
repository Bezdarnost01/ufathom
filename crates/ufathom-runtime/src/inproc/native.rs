//! Hooks on a function's `UFunction::Func` pointer.
//!
//! Every invocation of a `UFunction` goes through its `Func` pointer: blueprint
//! bytecode calling a native `BlueprintCallable`, a script function calling
//! another, and `ProcessEvent` itself (`UFunction::Invoke`). Only a direct C++
//! call of the native implementation skips it. [`GameThread::before_native`]
//! replaces that pointer, in the `UFunction` object (heap data, not code), with
//! [`native_thunk`], which runs callbacks and forwards to the original.
//!
//! The offset of `Func` is not part of the dump. It is solved here, in the
//! process, from an invariant: it is the first pointer-aligned offset past
//! `PropertiesSize` at which nearly every `UFunction` holds a pointer into
//! executable memory. No other field of a `UFunction` points at code.

use core::ffi::c_void;
use std::collections::HashMap;
use std::ops::ControlFlow;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::{Arc, OnceLock, RwLock};

use ufathom_mem::{MemoryReader, MemoryReaderExt, SelfMemory};

use super::InProcessExecutor;
use super::hook::{
    Active, Busy, Callbacks, GameThread, HookError, HookId, Registered, lock, with_executor,
};
use super::patch;
use crate::object::UObjectBase;
use crate::ptr::Ptr;
use crate::session::Session;

/// `void (*FNativeFuncPtr)(UObject* Context, FFrame& Stack, void* Result)`.
type NativeFn = unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void);

/// Offset of `FFrame::Node`, the function being run: after the
/// `FOutputDevice` base (a virtual table pointer and two flags) in every
/// UE4 and UE5 build.
const FRAME_NODE: usize = 0x10;
/// Other `FFrame` slots looked at when [`FRAME_NODE`] does not hold a hooked
/// function, in this order.
const FRAME_FALLBACK: [usize; 6] = [0x08, 0x18, 0x20, 0x28, 0x30, 0x38];

/// Bytes of each `UFunction` searched for `Func`, from its start.
const FUNC_WINDOW: usize = 0x180;
/// Share of functions, in percent, that must hold a code pointer at `Func`.
const FUNC_AGREEMENT_PERCENT: usize = 95;
/// Objects looked at, at most, while solving `Func`.
const MAX_OBJECTS: u32 = 4_000_000;

/// A native callback, as registered.
type NativeCallback = dyn Fn(&mut NativeContext<'_>) + Send + Sync;

/// One invocation of a hooked function, as a native callback sees it.
pub struct NativeContext<'a> {
    object: Ptr<UObjectBase>,
    function: Ptr<UObjectBase>,
    frame: *mut c_void,
    result: *mut c_void,
    exec: &'a InProcessExecutor,
    after: bool,
}

impl NativeContext<'_> {
    /// The object the function runs on (`Context`).
    #[must_use]
    pub fn object(&self) -> Ptr<UObjectBase> {
        self.object
    }

    /// The `UFunction`.
    #[must_use]
    pub fn function(&self) -> Ptr<UObjectBase> {
        self.function
    }

    /// The engine's `FFrame` for this invocation. Where the arguments are
    /// depends on the caller (bytecode or a parameter block), so they are not
    /// decoded here.
    #[must_use]
    pub fn frame(&self) -> *mut c_void {
        self.frame
    }

    /// Where the return value is written (`RESULT_PARAM`); null for a
    /// function without one. In an [`after`](GameThread::after_native)
    /// callback it holds the value the function returned.
    #[must_use]
    pub fn result(&self) -> *mut c_void {
        self.result
    }

    /// `true` in an `after` callback.
    #[must_use]
    pub fn is_after(&self) -> bool {
        self.after
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

impl core::fmt::Debug for NativeContext<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("NativeContext")
            .field("object", &self.object)
            .field("function", &self.function)
            .field("after", &self.after)
            .finish_non_exhaustive()
    }
}

/// State of the native hooks, part of [`GameThread`].
pub(super) struct NativeHooks {
    callbacks: Callbacks<NativeCallback>,
    /// The original `Func` of every function ever hooked. Entries are never
    /// removed, so a thread inside the thunk can always forward.
    originals: RwLock<HashMap<u64, usize>>,
    /// `Func` slots currently holding the thunk: (slot address, original).
    patched: std::sync::Mutex<HashMap<u64, (usize, usize)>>,
    func_offset: OnceLock<Result<u32, String>>,
}

impl NativeHooks {
    pub(super) fn new() -> Self {
        Self {
            callbacks: Callbacks::new(),
            originals: RwLock::new(HashMap::new()),
            patched: std::sync::Mutex::new(HashMap::new()),
            func_offset: OnceLock::new(),
        }
    }

    /// Removes a native callback, and unhooks its function when it was the
    /// last one. Returns `false` when `id` is not a native callback.
    pub(super) fn remove(&self, id: HookId) -> bool {
        let Some((function, left)) = self.callbacks.remove(id) else {
            return false;
        };
        if !left {
            let _ = self.restore(function);
        }
        true
    }

    /// Puts the original `Func` back into `function`.
    fn restore(&self, function: u64) -> Result<(), HookError> {
        let Some((slot, original)) = lock(&self.patched).remove(&function) else {
            return Ok(());
        };
        let thunk = native_thunk as *const () as usize;
        // SAFETY: `slot` is the `Func` field of a live `UFunction` this module
        // wrote the thunk into; `original` is what it held before.
        unsafe { patch::swap_slot(slot, thunk, original) }
            .map(|_| ())
            .map_err(|reason| HookError::Patch {
                slot: slot as u64,
                reason,
            })
    }

    /// Puts every original `Func` back. Callbacks stay registered; hooking
    /// the same function again re-patches it.
    pub(super) fn restore_all(&self) -> Result<(), HookError> {
        let functions: Vec<u64> = lock(&self.patched).keys().copied().collect();
        let mut first_error = None;
        for function in functions {
            if let Err(error) = self.restore(function) {
                first_error.get_or_insert(error);
            }
        }
        first_error.map_or(Ok(()), Err)
    }

    /// The function a frame runs and its original `Func`.
    fn resolve(&self, frame: *mut c_void) -> Option<(u64, usize)> {
        let originals = self
            .originals
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let base = frame as usize;
        core::iter::once(FRAME_NODE)
            .chain(FRAME_FALLBACK)
            .find_map(|offset| {
                // SAFETY: `frame` is the engine's live `FFrame` for this call,
                // larger than 0x40 bytes in every build; reading a word of it
                // has no side effect.
                let value = unsafe { ((base + offset) as *const u64).read_unaligned() };
                originals.get(&value).map(|&original| (value, original))
            })
    }
}

impl GameThread {
    /// Calls `callback` on the game thread before each invocation of the
    /// function at `path` through its `Func` pointer: from blueprint
    /// bytecode, from another script function, or from `ProcessEvent`. This
    /// reaches native `BlueprintCallable` functions that
    /// [`before`](Self::before) never sees, because blueprints call them
    /// without `ProcessEvent`.
    ///
    /// The callback observes the call. It cannot skip it: a blueprint caller
    /// expects the function to read its arguments from the bytecode.
    ///
    /// # Errors
    ///
    /// [`HookError::FunctionNotFound`], [`HookError::Session`],
    /// [`HookError::Native`] when the `Func` offset cannot be solved, or
    /// [`HookError::Patch`].
    pub fn before_native(
        &self,
        path: &str,
        callback: impl Fn(&mut NativeContext<'_>) + Send + Sync + 'static,
    ) -> Result<HookId, HookError> {
        self.register_native(path, false, Arc::new(callback))
    }

    /// Like [`before_native`](Self::before_native), after the function
    /// returned, with its return value at [`NativeContext::result`].
    ///
    /// # Errors
    ///
    /// As [`before_native`](Self::before_native).
    pub fn after_native(
        &self,
        path: &str,
        callback: impl Fn(&mut NativeContext<'_>) + Send + Sync + 'static,
    ) -> Result<HookId, HookError> {
        self.register_native(path, true, Arc::new(callback))
    }

    /// The offset of `UFunction::Func` in this build, solved on first use.
    ///
    /// # Errors
    ///
    /// [`HookError::Native`] when no offset satisfies the invariant.
    pub fn func_offset(&self) -> Result<u32, HookError> {
        self.native
            .func_offset
            .get_or_init(|| solve_func_offset(self.session()))
            .clone()
            .map_err(HookError::Native)
    }

    fn register_native(
        &self,
        path: &str,
        after: bool,
        callback: Arc<NativeCallback>,
    ) -> Result<HookId, HookError> {
        let function = self.function(path)?;
        let offset = self.func_offset()?;
        self.patch_native(function, offset)?;
        let id = self.new_id();
        self.native.callbacks.add(
            function.addr(),
            Registered {
                id,
                after,
                callback,
            },
        );
        Ok(id)
    }

    /// Writes the thunk into the `Func` of `function`, unless it is there.
    fn patch_native(&self, function: Ptr<UObjectBase>, offset: u32) -> Result<(), HookError> {
        let mut patched = lock(&self.native.patched);
        if patched.contains_key(&function.addr()) {
            return Ok(());
        }
        let bad = |reason: &str| HookError::Native(format!("{function:?}: {reason}"));
        let slot = function
            .addr()
            .checked_add(u64::from(offset))
            .and_then(|slot| usize::try_from(slot).ok())
            .filter(|slot| slot % 8 == 0)
            .ok_or_else(|| bad("the Func field is not an aligned address"))?;
        let original = self
            .session()
            .memory()
            .read_u64(slot as u64)
            .ok()
            .and_then(|value| usize::try_from(value).ok())
            .filter(|&value| value != 0)
            .ok_or_else(|| bad("Func is null or unreadable"))?;
        let thunk = native_thunk as *const () as usize;
        if original == thunk {
            return Ok(());
        }
        // Published before the thunk can run for this function.
        self.native
            .originals
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(function.addr(), original);
        // SAFETY: `slot` is the aligned `Func` field of a live `UFunction`,
        // read just above; the thunk forwards to `original` with the same
        // arguments.
        let changed = unsafe { patch::swap_slot(slot, original, thunk) }.map_err(|reason| {
            HookError::Patch {
                slot: slot as u64,
                reason,
            }
        })?;
        if !changed {
            return Err(bad("Func changed while it was being hooked"));
        }
        patched.insert(function.addr(), (slot, original));
        Ok(())
    }

    /// Runs the native callbacks of one kind.
    fn notify_native(
        exec: &InProcessExecutor,
        list: &[Registered<NativeCallback>],
        call: (Ptr<UObjectBase>, Ptr<UObjectBase>, *mut c_void, *mut c_void),
        after: bool,
    ) {
        let (object, function, frame, result) = call;
        let mut context = NativeContext {
            object,
            function,
            frame,
            result,
            exec,
            after,
        };
        for entry in list.iter().filter(|r| r.after == after) {
            let _ = catch_unwind(AssertUnwindSafe(|| (entry.callback)(&mut context)));
        }
    }
}

/// Written into a hooked function's `Func`.
unsafe extern "C" fn native_thunk(context: *mut c_void, frame: *mut c_void, result: *mut c_void) {
    let _active = Active::enter();
    let Some(state) = GameThread::get() else {
        return;
    };
    // The thunk is only ever written into functions recorded in
    // `originals`, so resolving fails only on an `FFrame` layout this code
    // does not know. There is nothing safe to forward to then.
    let Some((function, original)) = state.native.resolve(frame) else {
        return;
    };
    // SAFETY: `original` is the `Func` the function held, with this signature.
    let forward =
        || unsafe { core::mem::transmute::<usize, NativeFn>(original)(context, frame, result) };
    let table = state.native.callbacks.lookup(function);
    if table.is_none() || !state.wants(true) {
        return forward();
    }
    let list = table.as_ref().and_then(|t| t.get(&function));
    let call = (
        Ptr::<UObjectBase>::new(context as usize as u64),
        Ptr::<UObjectBase>::new(function),
        frame,
        result,
    );
    if let Some(list) = list {
        let _busy = Busy::enter();
        with_executor(state.desc, |exec| {
            state.drain(exec);
            GameThread::notify_native(exec, list, call, false);
        });
    }
    forward();
    if let Some(list) = list
        && list.iter().any(|r| r.after)
    {
        let _busy = Busy::enter();
        with_executor(state.desc, |exec| {
            GameThread::notify_native(exec, list, call, true);
        });
    }
}

/// Solves the offset of `UFunction::Func`: the lowest pointer-aligned
/// offset past `PropertiesSize` where at least
/// [`FUNC_AGREEMENT_PERCENT`] of all functions hold a pointer into
/// executable memory.
pub(super) fn solve_func_offset(session: &Session<SelfMemory>) -> Result<u32, String> {
    let mem = session.memory();
    let mut code: Vec<(u64, u64)> = mem
        .regions()
        .map_err(|error| format!("cannot list memory regions: {error}"))?
        .into_iter()
        .filter(|region| region.perms.execute)
        .map(|region| (region.base, region.base.saturating_add(region.size)))
        .collect();
    code.sort_unstable();
    let is_code = |value: u64| {
        let at = code.partition_point(|&(start, _)| start <= value);
        at.checked_sub(1)
            .and_then(|i| code.get(i))
            .is_some_and(|&(start, end)| value >= start && value < end)
    };

    let props = usize::try_from(session.desc().object.properties_size).unwrap_or(usize::MAX);
    let first = props.saturating_add(4).div_ceil(8) * 8;
    if first >= FUNC_WINDOW {
        return Err(format!(
            "PropertiesSize at {props:#x} leaves no room to search"
        ));
    }
    let mut is_function: HashMap<u64, bool> = HashMap::new();
    let mut hits = vec![0usize; FUNC_WINDOW / 8];
    let mut functions = 0usize;
    let mut window = vec![0u8; FUNC_WINDOW];
    let mut visited = 0u32;
    session
        .for_each_object(|_, object| {
            visited += 1;
            if visited > MAX_OBJECTS {
                return ControlFlow::Break(());
            }
            let Ok(class) = session.class_of(object) else {
                return ControlFlow::Continue(());
            };
            let function = *is_function.entry(class.addr()).or_insert_with(|| {
                session
                    .name_of(class)
                    .is_ok_and(|name| name.ends_with("Function"))
            });
            if !function || mem.read_exact(object.addr(), &mut window).is_err() {
                return ControlFlow::Continue(());
            }
            functions += 1;
            for (index, word) in window.chunks_exact(8).enumerate().skip(first / 8) {
                let mut bytes = [0u8; 8];
                bytes.copy_from_slice(word);
                if is_code(u64::from_le_bytes(bytes))
                    && let Some(hit) = hits.get_mut(index)
                {
                    *hit += 1;
                }
            }
            ControlFlow::Continue(())
        })
        .map_err(|error| format!("cannot walk the object array: {error}"))?;
    if functions == 0 {
        return Err("no UFunction is loaded".to_owned());
    }
    let needed = (functions * FUNC_AGREEMENT_PERCENT).div_ceil(100);
    hits.iter()
        .enumerate()
        .skip(first / 8)
        .find(|&(_, &count)| count >= needed)
        .and_then(|(index, _)| u32::try_from(index * 8).ok())
        .ok_or_else(|| {
            format!(
                "no offset from {first:#x} to {FUNC_WINDOW:#x} holds a code pointer in \
                 {FUNC_AGREEMENT_PERCENT}% of {functions} functions"
            )
        })
}
