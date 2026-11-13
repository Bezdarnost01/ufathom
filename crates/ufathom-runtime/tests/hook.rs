//! `GameThread` against a fake engine in live memory. A "game" thread calls
//! `ProcessEvent` through the patched virtual table in a loop, as the engine
//! does every frame, while the test thread queues tasks and callbacks.
//!
//! The hook is process-wide, so everything runs in one test.
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
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use common::{PROCESS_EVENT_INDEX, VTABLE, desc, image, object_addr, slot};
use ufathom_runtime::inproc::{GameThread, HookError};
use ufathom_runtime::{EngineDesc, Executor, Ptr, UObjectBase};

type ProcessEventFn = unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void);

/// Doubles the `i32` at offset 0 of the parameters into offset 8.
unsafe extern "C" fn fake_process_event(
    _this: *mut c_void,
    _function: *mut c_void,
    parms: *mut c_void,
) {
    // SAFETY: every caller passes a 16-byte, 8-aligned block.
    unsafe {
        let input = parms.cast::<i32>().read();
        parms.cast::<u8>().add(8).cast::<i32>().write(input * 2);
    }
}

const DESTROY: &str = "/Script/Engine.Actor:K2_DestroyActor";
const WAIT: Duration = Duration::from_secs(10);

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

fn wait_for(what: &str, mut done: impl FnMut() -> bool) {
    let start = Instant::now();
    while !done() {
        assert!(start.elapsed() < WAIT, "timed out waiting for {what}");
        thread::sleep(Duration::from_millis(1));
    }
}

#[test]
fn hook_runs_tasks_and_callbacks_on_the_game_thread() {
    // The fake engine in live memory, with the fake `ProcessEvent`.
    let mut memory = vec![0u64; common::SIZE / 8];
    let base = memory.as_ptr() as u64;
    let bytes = image(base, fake_process_event as *const () as u64);
    for (word, chunk) in memory.iter_mut().zip(bytes.chunks_exact(8)) {
        *word = u64::from_le_bytes(chunk.try_into().unwrap());
    }
    let slot_addr = (base + VTABLE + u64::from(PROCESS_EVENT_INDEX) * 8) as usize;
    let pawn = object_addr(base, slot::MY_PAWN);
    let destroy = object_addr(base, slot::DESTROY);

    let d: &'static EngineDesc = Box::leak(Box::new(desc(base)));
    // SAFETY: the description matches the fake engine, which outlives the
    // hook; the hook is uninstalled before the memory is freed.
    let game = unsafe { GameThread::install(d) }.unwrap();
    assert_eq!(
        game.hooked_slots(),
        1,
        "all objects share one virtual table"
    );
    assert_ne!(
        // SAFETY: reading the patched slot.
        unsafe { (slot_addr as *const usize).read_volatile() },
        fake_process_event as *const () as usize
    );
    // Installing again returns the same hook.
    // SAFETY: as above.
    assert!(core::ptr::eq(
        unsafe { GameThread::install(d) }.unwrap(),
        game
    ));

    // The "engine": calls `ProcessEvent` with input 7 until stopped and
    // records each output.
    let stop = Arc::new(AtomicBool::new(false));
    let outputs = Arc::new(Mutex::new(Vec::<i32>::new()));
    let engine = {
        let stop = Arc::clone(&stop);
        let outputs = Arc::clone(&outputs);
        thread::spawn(move || {
            while !stop.load(Ordering::Acquire) {
                let out = engine_call(slot_addr, pawn, destroy, 7);
                outputs.lock().unwrap().push(out);
                thread::sleep(Duration::from_micros(200));
            }
        })
    };
    wait_for("the game thread election", || game.game_thread_known());
    let latest = |outputs: &Mutex<Vec<i32>>| outputs.lock().unwrap().last().copied();
    let wait_output = |value: i32| {
        outputs.lock().unwrap().clear();
        wait_for(&format!("output {value}"), || {
            latest(&outputs) == Some(value)
        });
    };
    wait_output(14);

    // A task runs on the game thread, and may call game functions.
    let engine_thread = engine.thread().id();
    let (path, doubled, on_game) = game
        .call(WAIT, move |ctx| {
            let path = ctx
                .session()
                .path_of(Ptr::<UObjectBase>::new(pawn))
                .unwrap();
            let mut params = [0u64; 2];
            let bytes =
                unsafe { core::slice::from_raw_parts_mut(params.as_mut_ptr().cast::<u8>(), 16) };
            bytes[..4].copy_from_slice(&5i32.to_le_bytes());
            ctx.executor()
                .process_event(Ptr::new(pawn), DESTROY, bytes)
                .unwrap();
            let doubled = i32::from_le_bytes(bytes[8..12].try_into().unwrap());
            (path, doubled, thread::current().id() == engine_thread)
        })
        .unwrap();
    assert_eq!(path, "/Script/Engine.MyPawn");
    assert_eq!(doubled, 10);
    assert!(
        on_game,
        "the task ran on the thread that calls ProcessEvent"
    );

    // A panicking task is contained; the game keeps running.
    assert!(matches!(
        game.call(WAIT, |_| -> () { panic!("task panic") }),
        Err(HookError::Dropped)
    ));
    wait_output(14);

    // `before` can change the inputs; `after` sees the outputs.
    let seen_after = Arc::new(AtomicUsize::new(0));
    let change = game
        .before(DESTROY, |ctx| {
            ctx.params_mut()[..4].copy_from_slice(&100i32.to_le_bytes());
        })
        .unwrap();
    let watch = {
        let seen_after = Arc::clone(&seen_after);
        game.after(DESTROY, move |ctx| {
            assert!(ctx.is_after());
            let out = i32::from_le_bytes(ctx.params()[8..12].try_into().unwrap());
            seen_after.store(out as usize, Ordering::Release);
        })
        .unwrap()
    };
    wait_output(200);
    wait_for("the after callback", || {
        seen_after.load(Ordering::Acquire) == 200
    });
    assert!(game.remove(change));
    assert!(!game.remove(change), "already removed");

    // `before` can skip the game function.
    let skip = game.before(DESTROY, |ctx| ctx.skip_original()).unwrap();
    wait_output(0);
    assert!(game.remove(skip));
    assert!(game.remove(watch));
    wait_output(14);

    // Calls from another thread pass straight through, without callbacks.
    let hits = Arc::new(AtomicUsize::new(0));
    let count = {
        let hits = Arc::clone(&hits);
        game.before(DESTROY, move |_| {
            hits.fetch_add(1, Ordering::AcqRel);
        })
        .unwrap()
    };
    wait_for("a callback on the game thread", || {
        hits.load(Ordering::Acquire) > 0
    });
    stop.store(true, Ordering::Release);
    engine.join().unwrap();
    let before_side_call = hits.load(Ordering::Acquire);
    assert_eq!(engine_call(slot_addr, pawn, destroy, 3), 6);
    assert_eq!(hits.load(Ordering::Acquire), before_side_call);
    assert!(game.remove(count));

    // Unknown functions are refused.
    assert!(matches!(
        game.before("/Script/Engine.Actor:Nope", |_| {}),
        Err(HookError::FunctionNotFound(_))
    ));
    assert!(matches!(
        game.before("/Script/Engine.Actor", |_| {}),
        Err(HookError::FunctionNotFound(_))
    ));

    // Uninstall puts the original back; rescan hooks again.
    game.uninstall().unwrap();
    assert_eq!(game.hooked_slots(), 0);
    // SAFETY: reading the restored slot.
    assert_eq!(
        unsafe { (slot_addr as *const usize).read_volatile() },
        fake_process_event as *const () as usize
    );
    assert_eq!(game.rescan().unwrap(), 1);
    game.uninstall().unwrap();
    drop(memory);
}
