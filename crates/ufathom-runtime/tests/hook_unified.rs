//! The unified hooks `hook` and `hook_native` against a fake engine in live
//! memory: the mechanism follows the function's kind, and each call fires
//! once.
//!
//! The fake `ProcessEvent` does what `UFunction::Invoke` does for a native
//! function: after the parameter block is processed it goes through the
//! function's `Func`. That is the property the unified hook relies on, and
//! the one that would make a registration on both mechanisms fire twice.
//!
//! The hook is process-wide, so everything runs in one test, in its own test
//! binary.
#![cfg(feature = "inproc")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "tests may panic"
)]

mod common;

use core::ffi::c_void;
use std::sync::atomic::{AtomicI32, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use common::{PROCESS_EVENT_INDEX, VTABLE, desc, image, object_addr, slot};
use ufathom_runtime::inproc::{GameFunction, GameThread, HookError, Native, Script, When};
use ufathom_runtime::{EngineDesc, Pod};

type ProcessEventFn = unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void);
type NativeFn = unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void);

const DESTROY: &str = "/Script/Engine.Actor:K2_DestroyActor";
/// Where the fake `UFunction` keeps `Func`: past `PropertiesSize` (0x50).
const FUNC: u64 = 0x98;

/// Calls of the fake native implementation.
static NATIVE_CALLS: AtomicUsize = AtomicUsize::new(0);

/// A generated parameter struct: an input and an output.
#[repr(C)]
#[derive(Clone, Copy)]
struct DestroyParams {
    input: i32,
    pad_0: i32,
    output: i32,
    pad_1: i32,
}
// SAFETY: integers only, no implicit padding.
unsafe impl Pod for DestroyParams {}

/// The function as a script function.
impl GameFunction for DestroyParams {
    const PATH: &'static str = DESTROY;
    type Kind = Script;
    type Params = Self;
}

/// The same function as a native one.
struct NativeDestroy;
impl GameFunction for NativeDestroy {
    const PATH: &'static str = DESTROY;
    type Kind = Native;
    type Params = DestroyParams;
}

/// A path that exists nowhere.
struct MissingScript;
impl GameFunction for MissingScript {
    const PATH: &'static str = "/Script/Engine.Actor:Nope";
    type Kind = Script;
    type Params = DestroyParams;
}

/// As [`MissingScript`], native.
struct MissingNative;
impl GameFunction for MissingNative {
    const PATH: &'static str = "/Script/Engine.Actor:Nope";
    type Kind = Native;
    type Params = DestroyParams;
}

/// `ProcessEvent`: doubles the input at offset 0 into offset 8, then invokes
/// the function through its `Func` with an `FFrame` whose `Node` is the
/// function, as `UFunction::Invoke` does.
unsafe extern "C" fn fake_process_event(
    this: *mut c_void,
    function: *mut c_void,
    parms: *mut c_void,
) {
    // SAFETY: every caller passes a 16-byte, 8-aligned block and a function
    // object whose `Func` holds the fake native or the thunk forwarding to it.
    unsafe {
        let input = parms.cast::<i32>().read();
        parms.cast::<u8>().add(8).cast::<i32>().write(input * 2);
        let func = function
            .cast::<u8>()
            .add(FUNC as usize)
            .cast::<usize>()
            .read_volatile();
        let mut frame = [0u64; 16];
        frame[2] = function as u64;
        let mut result = 0i32;
        core::mem::transmute::<usize, NativeFn>(func)(
            this,
            frame.as_mut_ptr().cast(),
            (&raw mut result).cast(),
        );
    }
}

/// Writes 42 to the result, as a native function returning an `i32` would.
unsafe extern "C" fn fake_native(_: *mut c_void, _: *mut c_void, result: *mut c_void) {
    NATIVE_CALLS.fetch_add(1, Ordering::AcqRel);
    // SAFETY: every caller passes a writable `i32`.
    unsafe { result.cast::<i32>().write(42) };
}

/// Calls `ProcessEvent` through the virtual table slot, as the engine does.
fn engine_call(slot_addr: usize, this: u64, function: u64, input: i32) -> i32 {
    let mut params = [0u64; 2];
    let block = params.as_mut_ptr().cast::<u8>();
    // SAFETY: `params` is 16 bytes; the slot holds the fake or the hook
    // forwarding to it.
    unsafe {
        block.cast::<i32>().write(input);
        let entry = (slot_addr as *const usize).read_volatile();
        let f = core::mem::transmute::<usize, ProcessEventFn>(entry);
        f(this as *mut c_void, function as *mut c_void, block.cast());
        block.add(8).cast::<i32>().read()
    }
}

/// Invokes a function through its `Func` directly, as bytecode does.
fn invoke(function: u64, context: u64) {
    let mut frame = [0u64; 16];
    frame[2] = function;
    let mut result = 0i32;
    // SAFETY: `Func` holds the fake native or the thunk forwarding to it.
    unsafe {
        let func = ((function + FUNC) as *const usize).read_volatile();
        let f = core::mem::transmute::<usize, NativeFn>(func);
        f(
            context as *mut c_void,
            frame.as_mut_ptr().cast(),
            (&raw mut result).cast(),
        );
    }
}

fn count(counter: &Arc<AtomicUsize>) -> usize {
    counter.load(Ordering::Acquire)
}

#[test]
fn hook_follows_the_kind_and_each_call_fires_once() {
    let mut memory = vec![0u64; common::SIZE / 8];
    let base = memory.as_ptr() as u64;
    let bytes = image(base, fake_process_event as *const () as u64);
    for (word, chunk) in memory.iter_mut().zip(bytes.chunks_exact(8)) {
        *word = u64::from_le_bytes(chunk.try_into().unwrap());
    }
    let slot_addr = (base + VTABLE + u64::from(PROCESS_EVENT_INDEX) * 8) as usize;
    let pawn = object_addr(base, slot::MY_PAWN);
    let destroy = object_addr(base, slot::DESTROY);
    memory[((destroy - base + FUNC) / 8) as usize] = fake_native as *const () as u64;

    let d: &'static EngineDesc = Box::leak(Box::new(desc(base)));
    // SAFETY: the description matches the fake engine, which outlives the
    // hook; the hook is uninstalled before the memory is freed.
    let game = unsafe { GameThread::install(d) }.unwrap();
    // This thread plays the engine and the game thread, so every call runs
    // its callbacks at once.
    assert!(game.set_game_thread(GameThread::current_thread_id()));

    let script_before = Arc::new(AtomicUsize::new(0));
    let script_after = Arc::new(AtomicUsize::new(0));
    let native_before = Arc::new(AtomicUsize::new(0));
    let native_after = Arc::new(AtomicUsize::new(0));
    let seen_input = Arc::new(AtomicI32::new(0));
    let seen_output = Arc::new(AtomicI32::new(0));
    let results = Arc::new(Mutex::new(Vec::<i32>::new()));

    // A script function: typed parameters, before and after.
    let hooks = [
        {
            let (calls, seen_input) = (Arc::clone(&script_before), Arc::clone(&seen_input));
            game.hook::<DestroyParams>(When::Before, move |ctx, params| {
                assert!(!ctx.is_after());
                assert_eq!(ctx.object().addr(), pawn);
                seen_input.store(params.input, Ordering::Release);
                calls.fetch_add(1, Ordering::AcqRel);
            })
            .unwrap()
        },
        {
            let (calls, seen_output) = (Arc::clone(&script_after), Arc::clone(&seen_output));
            game.hook::<DestroyParams>(When::After, move |ctx, params| {
                assert!(ctx.is_after());
                seen_output.store(params.output, Ordering::Release);
                calls.fetch_add(1, Ordering::AcqRel);
            })
            .unwrap()
        },
        // A native function: the invocation and its result.
        {
            let calls = Arc::clone(&native_before);
            game.hook_native::<NativeDestroy>(When::Before, move |ctx| {
                assert!(!ctx.is_after());
                assert_eq!(ctx.object().addr(), pawn);
                calls.fetch_add(1, Ordering::AcqRel);
            })
            .unwrap()
        },
        {
            let (calls, results) = (Arc::clone(&native_after), Arc::clone(&results));
            game.hook_native::<NativeDestroy>(When::After, move |ctx| {
                assert!(ctx.is_after());
                // SAFETY: the fake passes an `i32` result.
                results
                    .lock()
                    .unwrap()
                    .push(unsafe { ctx.result().cast::<i32>().read() });
                calls.fetch_add(1, Ordering::AcqRel);
            })
            .unwrap()
        },
    ];

    // One `ProcessEvent` call reaches the function through both mechanisms,
    // and every hook fires exactly once.
    assert_eq!(engine_call(slot_addr, pawn, destroy, 21), 42);
    assert_eq!(NATIVE_CALLS.load(Ordering::Acquire), 1);
    assert_eq!(count(&script_before), 1);
    assert_eq!(count(&script_after), 1);
    assert_eq!(count(&native_before), 1, "once, not once per mechanism");
    assert_eq!(count(&native_after), 1);
    assert_eq!(seen_input.load(Ordering::Acquire), 21);
    assert_eq!(seen_output.load(Ordering::Acquire), 42);
    assert_eq!(*results.lock().unwrap(), [42]);

    // A direct invocation (bytecode) reaches the native hook only.
    invoke(destroy, pawn);
    assert_eq!(count(&native_before), 2);
    assert_eq!(count(&native_after), 2);
    assert_eq!(count(&script_before), 1, "ProcessEvent was not called");
    assert_eq!(count(&script_after), 1);

    // Why a native function is registered on `Func` alone: a second hook on
    // the `ProcessEvent` path fires for every `ProcessEvent` call as well,
    // so the same call would be reported twice.
    let on_process_event = Arc::new(AtomicUsize::new(0));
    let both = {
        let calls = Arc::clone(&on_process_event);
        game.before_fn::<NativeDestroy>(move |_, _| {
            calls.fetch_add(1, Ordering::AcqRel);
        })
        .unwrap()
    };
    let (func_before, pe_before) = (count(&native_before), count(&on_process_event));
    engine_call(slot_addr, pawn, destroy, 1);
    assert_eq!(count(&native_before) - func_before, 1);
    assert_eq!(count(&on_process_event) - pe_before, 1);
    assert!(game.remove(both));

    // A function that does not exist is refused by both.
    assert!(matches!(
        game.hook::<MissingScript>(When::Before, |_, _| {}),
        Err(HookError::FunctionNotFound(_))
    ));
    assert!(matches!(
        game.hook_native::<MissingNative>(When::After, |_| {}),
        Err(HookError::FunctionNotFound(_))
    ));

    // The returned ids are the usual ones: a guard removes its hook.
    let guarded = Arc::new(AtomicUsize::new(0));
    let guard = {
        let calls = Arc::clone(&guarded);
        game.hook::<DestroyParams>(When::Before, move |_, _| {
            calls.fetch_add(1, Ordering::AcqRel);
        })
        .unwrap()
        .into_guard(game)
    };
    engine_call(slot_addr, pawn, destroy, 2);
    assert_eq!(count(&guarded), 1);
    drop(guard);
    engine_call(slot_addr, pawn, destroy, 2);
    assert_eq!(count(&guarded), 1, "removed by the guard");

    // Their module is recorded as for the other hooks: removing everything
    // the current module registered removes them, and `Func` is restored.
    assert_eq!(game.remove_owned_by_current_module(), hooks.len());
    let func_word = |addr: u64| {
        // SAFETY: test address inside a live test allocation.
        unsafe { (addr as *const u64).read_volatile() }
    };
    assert_eq!(func_word(destroy + FUNC), fake_native as *const () as u64);
    let before = count(&script_before);
    engine_call(slot_addr, pawn, destroy, 3);
    assert_eq!(count(&script_before), before, "no hook is left");
    assert!(!game.remove(hooks[0]));

    assert!(
        game.uninstall_and_wait(std::time::Duration::from_secs(10))
            .unwrap()
    );
    drop(memory);
}
