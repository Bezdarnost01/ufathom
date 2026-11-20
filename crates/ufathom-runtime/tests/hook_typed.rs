//! Typed hooks (`before_fn`, `after_fn`, `before_native_fn`) against a fake
//! engine in live memory, with hand-written `GameFunction` impls of the kind
//! a generated SDK provides.
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
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use common::{PROCESS_EVENT_INDEX, VTABLE, desc, image, object_addr, slot};
use ufathom_runtime::inproc::{GameFunction, GameThread, HookError, Native, Script};
use ufathom_runtime::{EngineDesc, Pod};

type ProcessEventFn = unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void);
type NativeFn = unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void);

const DESTROY: &str = "/Script/Engine.Actor:K2_DestroyActor";
const WAIT: Duration = Duration::from_secs(10);
/// Where the fake `UFunction` keeps `Func`: past `PropertiesSize` (0x50).
const FUNC: u64 = 0x98;

/// What a generated parameter struct looks like: 16 bytes, an input and an
/// output with explicit padding.
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

/// The function as a script function, by its parameter struct.
impl GameFunction for DestroyParams {
    const PATH: &'static str = DESTROY;
    type Kind = Script;
    type Params = Self;
}

/// The same function with a layout larger than the live block.
struct TooLarge;
impl GameFunction for TooLarge {
    const PATH: &'static str = DESTROY;
    type Kind = Script;
    type Params = [u64; 4];
}

/// The same function seen as one without parameters.
struct NoParams;
impl GameFunction for NoParams {
    const PATH: &'static str = DESTROY;
    type Kind = Script;
    type Params = [u8; 0];
}

/// The same function as a native one.
struct NativeDestroy;
impl GameFunction for NativeDestroy {
    const PATH: &'static str = DESTROY;
    type Kind = Native;
    type Params = DestroyParams;
}

/// A path that exists nowhere.
struct Missing;
impl GameFunction for Missing {
    const PATH: &'static str = "/Script/Engine.Actor:Nope";
    type Kind = Script;
    type Params = DestroyParams;
}

/// Doubles the `i32` at offset 0 of the parameters into offset 8.
unsafe extern "C" fn fake_process_event(_: *mut c_void, _: *mut c_void, parms: *mut c_void) {
    // SAFETY: every caller passes a 16-byte, 8-aligned block.
    unsafe {
        let input = parms.cast::<i32>().read();
        parms.cast::<u8>().add(8).cast::<i32>().write(input * 2);
    }
}

/// Writes 42 to the result, as a native function returning an `i32` would.
unsafe extern "C" fn fake_native(_: *mut c_void, _: *mut c_void, result: *mut c_void) {
    // SAFETY: every caller passes a writable `i32`.
    unsafe { result.cast::<i32>().write(42) };
}

/// Calls whatever the `ProcessEvent` slot holds, as the engine would.
fn engine_call(slot_addr: usize, this: u64, function: u64, input: i32) -> i32 {
    let mut params = [0u64; 2];
    let block = params.as_mut_ptr().cast::<u8>();
    // SAFETY: `params` is 16 bytes; the slot holds a function with the
    // `ProcessEvent` signature (the fake, or the hook forwarding to it).
    unsafe {
        block.cast::<i32>().write(input);
        let entry = (slot_addr as *const usize).read_volatile();
        let f = core::mem::transmute::<usize, ProcessEventFn>(entry);
        f(this as *mut c_void, function as *mut c_void, block.cast());
        block.add(8).cast::<i32>().read()
    }
}

/// Invokes a function through its `Func`, as the blueprint VM does.
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

fn wait_for(what: &str, mut done: impl FnMut() -> bool) {
    let start = Instant::now();
    while !done() {
        assert!(start.elapsed() < WAIT, "timed out waiting for {what}");
        thread::sleep(Duration::from_millis(1));
    }
}

#[test]
fn typed_hooks_see_checked_parameters_on_a_fake_engine() {
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

    // The "engine": calls `ProcessEvent` with input 7 and invokes the native
    // implementation, until stopped; records each output.
    let stop = Arc::new(AtomicBool::new(false));
    let output = Arc::new(AtomicI32::new(0));
    let engine = {
        let stop = Arc::clone(&stop);
        let output = Arc::clone(&output);
        thread::spawn(move || {
            while !stop.load(Ordering::Acquire) {
                output.store(engine_call(slot_addr, pawn, destroy, 7), Ordering::Release);
                invoke(destroy, pawn);
                thread::sleep(Duration::from_micros(200));
            }
        })
    };
    wait_for("the game thread election", || game.game_thread_known());
    wait_for("an unhooked call", || output.load(Ordering::Acquire) == 14);

    // `before_fn` gets the block as the right type and may change it.
    let seen_input = Arc::new(AtomicI32::new(0));
    let change = {
        let seen_input = Arc::clone(&seen_input);
        game.before_fn::<DestroyParams>(move |ctx, params| {
            assert!(!ctx.is_after());
            assert_eq!(ctx.object().addr(), pawn);
            assert!(ctx.params().is_empty(), "the block is lent as `params`");
            seen_input.store(params.input, Ordering::Release);
            params.input = 100;
        })
        .unwrap()
    };
    wait_for("the modified input to reach the engine", || {
        output.load(Ordering::Acquire) == 200
    });
    assert_eq!(seen_input.load(Ordering::Acquire), 7);

    // A later untyped callback of the same call sees the whole block again.
    let untyped_len = Arc::new(AtomicUsize::new(0));
    let untyped = {
        let untyped_len = Arc::clone(&untyped_len);
        game.before(DESTROY, move |ctx| {
            untyped_len.store(ctx.params().len(), Ordering::Release);
        })
        .unwrap()
    };
    wait_for("the untyped callback", || {
        untyped_len.load(Ordering::Acquire) != 0
    });
    assert_eq!(untyped_len.load(Ordering::Acquire), 16);
    assert!(game.remove(untyped));

    // `after_fn` sees the output the game function wrote, and may change it.
    let seen_output = Arc::new(AtomicI32::new(0));
    let watch = {
        let seen_output = Arc::clone(&seen_output);
        game.after_fn::<DestroyParams>(move |ctx, params| {
            assert!(ctx.is_after());
            seen_output.store(params.output, Ordering::Release);
            params.output += 1;
        })
        .unwrap()
    };
    wait_for("the after callback", || {
        seen_output.load(Ordering::Acquire) == 200
    });
    wait_for("the changed output to reach the engine", || {
        output.load(Ordering::Acquire) == 201
    });
    assert!(game.remove(change));
    assert!(game.remove(watch));
    wait_for("the unhooked output", || {
        output.load(Ordering::Acquire) == 14
    });

    // A layout larger than the live block: the callback is never called,
    // and the callbacks that do fit keep running.
    let too_large = Arc::new(AtomicUsize::new(0));
    let fitting = Arc::new(AtomicUsize::new(0));
    let big = {
        let too_large = Arc::clone(&too_large);
        game.before_fn::<TooLarge>(move |_, _| {
            too_large.fetch_add(1, Ordering::AcqRel);
        })
        .unwrap()
    };
    let small = {
        let fitting = Arc::clone(&fitting);
        game.before_fn::<DestroyParams>(move |_, _| {
            fitting.fetch_add(1, Ordering::AcqRel);
        })
        .unwrap()
    };
    wait_for("calls to the fitting callback", || {
        fitting.load(Ordering::Acquire) >= 20
    });
    assert_eq!(too_large.load(Ordering::Acquire), 0);
    assert!(game.remove(big));
    assert!(game.remove(small));

    // A function without parameters always fits, and a typed callback may
    // skip the original.
    let skip = game
        .before_fn::<NoParams>(|ctx, _| ctx.skip_original())
        .unwrap();
    wait_for("the skipped call", || output.load(Ordering::Acquire) == 0);
    assert!(game.remove(skip));
    wait_for("the unhooked output", || {
        output.load(Ordering::Acquire) == 14
    });

    // A panicking typed callback is contained and gives the block back.
    let panicking = game
        .before_fn::<DestroyParams>(|_, _| panic!("typed callback panic"))
        .unwrap();
    let after_panic = Arc::new(AtomicUsize::new(0));
    let check = {
        let after_panic = Arc::clone(&after_panic);
        game.before(DESTROY, move |ctx| {
            after_panic.store(ctx.params().len(), Ordering::Release);
        })
        .unwrap()
    };
    wait_for("the callback after the panic", || {
        after_panic.load(Ordering::Acquire) != 0
    });
    assert_eq!(after_panic.load(Ordering::Acquire), 16);
    assert!(game.remove(panicking));
    assert!(game.remove(check));

    // Native hooks by type: callbacks see the invocation and the result.
    let native_before = Arc::new(AtomicUsize::new(0));
    let native_result = Arc::new(Mutex::new(Vec::<i32>::new()));
    let hook_before = {
        let native_before = Arc::clone(&native_before);
        game.before_native_fn::<NativeDestroy>(move |ctx| {
            assert!(!ctx.is_after());
            assert_eq!(ctx.object().addr(), pawn);
            native_before.fetch_add(1, Ordering::AcqRel);
        })
        .unwrap()
    };
    let hook_after = {
        let native_result = Arc::clone(&native_result);
        game.after_native_fn::<NativeDestroy>(move |ctx| {
            assert!(ctx.is_after());
            // SAFETY: the fake passes an `i32` result.
            let value = unsafe { ctx.result().cast::<i32>().read() };
            native_result.lock().unwrap().push(value);
        })
        .unwrap()
    };
    wait_for("native callbacks", || {
        native_before.load(Ordering::Acquire) > 0 && !native_result.lock().unwrap().is_empty()
    });
    assert!(native_result.lock().unwrap().iter().all(|&v| v == 42));
    assert!(game.remove(hook_before));
    assert!(game.remove(hook_after));

    // An unknown path is refused, as with the string API.
    assert!(matches!(
        game.before_fn::<Missing>(|_, _| {}),
        Err(HookError::FunctionNotFound(_))
    ));

    stop.store(true, Ordering::Release);
    engine.join().unwrap();
    assert!(game.uninstall_and_wait(WAIT).unwrap());
    drop(memory);
}
