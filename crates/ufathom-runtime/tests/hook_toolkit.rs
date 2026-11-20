//! `GameThread` features beyond `hook.rs`: naming the game thread, hooks on
//! `UFunction::Func`, classes that get their first instance after install,
//! and waiting for the hook to be idle before unloading.
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
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use common::{PROCESS_EVENT_INDEX, VTABLE, desc, image, object_addr, slot};
use ufathom_runtime::EngineDesc;
use ufathom_runtime::inproc::{GameThread, HookError, MIN_RESCAN_INTERVAL};

type ProcessEventFn = unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void);
type NativeFn = unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void);

const DESTROY: &str = "/Script/Engine.Actor:K2_DestroyActor";
const WAIT: Duration = Duration::from_secs(10);
/// Where the fake `UFunction` keeps `Func`: past `PropertiesSize` (0x50).
const FUNC: u64 = 0x98;

/// Counts calls of the fake `ProcessEvent`.
static PROCESS_EVENT_CALLS: AtomicUsize = AtomicUsize::new(0);
/// Counts calls of the fake native implementation.
static NATIVE_CALLS: AtomicUsize = AtomicUsize::new(0);

unsafe extern "C" fn fake_process_event(_: *mut c_void, _: *mut c_void, _: *mut c_void) {
    PROCESS_EVENT_CALLS.fetch_add(1, Ordering::AcqRel);
}

/// Writes 42 to the result, as a native function returning an `i32` would.
unsafe extern "C" fn fake_native(_: *mut c_void, _: *mut c_void, result: *mut c_void) {
    NATIVE_CALLS.fetch_add(1, Ordering::AcqRel);
    // SAFETY: every caller passes a writable `i32`.
    unsafe { result.cast::<i32>().write(42) };
}

/// Calls whatever the `ProcessEvent` slot of `vtable` holds.
fn engine_call(vtable: u64, this: u64, function: u64) {
    let slot = (vtable + u64::from(PROCESS_EVENT_INDEX) * 8) as *const usize;
    let mut params = [0u64; 2];
    // SAFETY: the slot holds the fake or the hook forwarding to it.
    unsafe {
        let f = core::mem::transmute::<usize, ProcessEventFn>(slot.read_volatile());
        f(
            this as *mut c_void,
            function as *mut c_void,
            params.as_mut_ptr().cast(),
        );
    }
}

/// Invokes a function through its `Func`, as the blueprint VM does, with an
/// `FFrame` whose `Node` is the function. Returns the result.
fn invoke(function: u64, context: u64) -> i32 {
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
    result
}

fn wait_for(what: &str, mut done: impl FnMut() -> bool) {
    let start = Instant::now();
    while !done() {
        assert!(start.elapsed() < WAIT, "timed out waiting for {what}");
        thread::sleep(Duration::from_millis(1));
    }
}

/// A thread that plays the engine until `stop`: it calls `ProcessEvent` and
/// invokes the function, and reports its operating system id first.
fn engine_thread(
    stop: Arc<AtomicBool>,
    vtable: Arc<AtomicU64>,
    this: u64,
    function: u64,
) -> (thread::JoinHandle<()>, u64) {
    let (id_tx, id_rx) = mpsc::channel();
    let handle = thread::spawn(move || {
        id_tx.send(GameThread::current_thread_id()).unwrap();
        while !stop.load(Ordering::Acquire) {
            engine_call(vtable.load(Ordering::Acquire), this, function);
            assert_eq!(invoke(function, this), 42);
            thread::sleep(Duration::from_micros(200));
        }
    });
    (handle, id_rx.recv().unwrap())
}

#[test]
fn toolkit_features_work_on_a_fake_engine() {
    let mut memory = vec![0u64; common::SIZE / 8];
    let base = memory.as_ptr() as u64;
    let bytes = image(base, fake_process_event as *const () as u64);
    for (word, chunk) in memory.iter_mut().zip(bytes.chunks_exact(8)) {
        *word = u64::from_le_bytes(chunk.try_into().unwrap());
    }
    let pawn = object_addr(base, slot::MY_PAWN);
    let destroy = object_addr(base, slot::DESTROY);
    let destroy_word = |offset: u64| ((destroy - base + offset) / 8) as usize;
    memory[destroy_word(FUNC)] = fake_native as *const () as u64;
    // A second class whose virtual table no object uses yet.
    let mut second_vtable = vec![0u64; 8];
    second_vtable[PROCESS_EVENT_INDEX as usize] = fake_process_event as *const () as u64;
    let second = second_vtable.as_ptr() as u64;

    let d: &'static EngineDesc = Box::leak(Box::new(desc(base)));
    // SAFETY: the description matches the fake engine, which outlives the
    // hook; the hook is uninstalled before the memory is freed.
    let game = unsafe { GameThread::install(d) }.unwrap();
    assert_eq!(game.hooked_slots(), 1);
    assert!(!game.game_thread_known());

    // Two engine threads; the second is named as the game thread before
    // either has made enough calls to be elected.
    let stop = Arc::new(AtomicBool::new(false));
    let vtable = Arc::new(AtomicU64::new(base + VTABLE));
    let (a, _a_id) = engine_thread(Arc::clone(&stop), Arc::clone(&vtable), pawn, destroy);
    let (b, b_id) = engine_thread(Arc::clone(&stop), Arc::clone(&vtable), pawn, destroy);
    if !game.set_game_thread(b_id) {
        // Thread `a` was elected first; naming is refused once known.
        assert_ne!(game.game_thread_id(), None);
        panic!("thread a won the election before it could be named");
    }
    assert!(!game.set_game_thread(b_id), "already known");
    assert_eq!(game.game_thread_id(), Some(b_id));
    let ran_on = game
        .call(WAIT, |_| GameThread::current_thread_id())
        .unwrap();
    assert_eq!(ran_on, b_id, "tasks run on the named thread only");

    // `Func` is found by its invariant, and native callbacks see the call.
    assert_eq!(game.func_offset().unwrap(), FUNC as u32);
    let before = Arc::new(AtomicUsize::new(0));
    let after_result = Arc::new(AtomicUsize::new(0));
    let hook_before = {
        let before = Arc::clone(&before);
        game.before_native(DESTROY, move |ctx| {
            assert!(!ctx.is_after());
            assert_eq!(ctx.object().addr(), pawn);
            before.fetch_add(1, Ordering::AcqRel);
        })
        .unwrap()
    };
    let hook_after = {
        let after_result = Arc::clone(&after_result);
        game.after_native(DESTROY, move |ctx| {
            assert!(ctx.is_after());
            assert!(ctx.session().path_of(ctx.function()).is_ok());
            // SAFETY: the fake passes an `i32` result.
            let value = unsafe { ctx.result().cast::<i32>().read() };
            after_result.store(value as usize, Ordering::Release);
        })
        .unwrap()
    };
    assert_ne!(memory_word(destroy + FUNC), fake_native as *const () as u64);
    wait_for("native callbacks", || {
        before.load(Ordering::Acquire) > 0 && after_result.load(Ordering::Acquire) == 42
    });
    // The game thread's calls run the callbacks; other threads only forward,
    // and every call still reaches the original.
    let native_calls = NATIVE_CALLS.load(Ordering::Acquire);
    assert!(native_calls >= before.load(Ordering::Acquire));
    assert!(game.remove(hook_before));
    assert_ne!(
        memory_word(destroy + FUNC),
        fake_native as *const () as u64,
        "still hooked: one callback left"
    );
    assert!(game.remove(hook_after));
    assert_eq!(
        memory_word(destroy + FUNC),
        fake_native as *const () as u64,
        "the last removal restores Func"
    );
    assert!(matches!(
        game.before_native("/Script/Engine.Actor:Nope", |_| {}),
        Err(HookError::FunctionNotFound(_))
    ));

    // A callback on one function does not run for calls of another, and
    // the original keeps being called.
    let other_hits = Arc::new(AtomicUsize::new(0));
    let unrelated = {
        let other_hits = Arc::clone(&other_hits);
        game.before(DESTROY, move |_| {
            other_hits.fetch_add(1, Ordering::AcqRel);
        })
        .unwrap()
    };
    wait_for("a ProcessEvent callback", || {
        other_hits.load(Ordering::Acquire) > 0
    });
    assert!(game.remove(unrelated));

    // An object of a class with a new virtual table appears after install:
    // the background rescan hooks it.
    assert!(game.start_auto_rescan(Duration::ZERO).unwrap());
    assert!(
        !game.start_auto_rescan(MIN_RESCAN_INTERVAL).unwrap(),
        "already running"
    );
    assert!(game.auto_rescan_running());
    let actor_1 = object_addr(base, slot::ACTOR_1);
    memory[((actor_1 - base) / 8) as usize] = second;
    let second_slot = second + u64::from(PROCESS_EVENT_INDEX) * 8;
    wait_for("the rescan to hook the new class", || {
        memory_word(second_slot) != fake_process_event as *const () as u64
    });
    assert_eq!(game.hooked_slots(), 2);
    vtable.store(second, Ordering::Release);
    let calls = PROCESS_EVENT_CALLS.load(Ordering::Acquire);
    wait_for("calls through the new class", || {
        PROCESS_EVENT_CALLS.load(Ordering::Acquire) > calls + 10
    });

    // Re-hook a native function, then uninstall and wait: every slot and
    // `Func` is restored and no thread is left in the hook.
    let rehook = game.before_native(DESTROY, |_| {}).unwrap();
    assert!(game.uninstall_and_wait(WAIT).unwrap());
    assert!(!game.auto_rescan_running());
    assert_eq!(game.hooked_slots(), 0);
    assert_eq!(
        memory_word(base + VTABLE + u64::from(PROCESS_EVENT_INDEX) * 8),
        fake_process_event as *const () as u64
    );
    assert_eq!(
        memory_word(second_slot),
        fake_process_event as *const () as u64
    );
    assert_eq!(memory_word(destroy + FUNC), fake_native as *const () as u64);
    assert!(game.remove(rehook));

    stop.store(true, Ordering::Release);
    a.join().unwrap();
    b.join().unwrap();
    drop(second_vtable);
    drop(memory);
}

fn memory_word(addr: u64) -> u64 {
    // SAFETY: test addresses inside live test allocations.
    unsafe { (addr as *const u64).read_volatile() }
}
