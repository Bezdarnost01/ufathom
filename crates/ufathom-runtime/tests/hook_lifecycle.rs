//! Owning and releasing hooks: `HookGuard`, removal by module, and two
//! modules that hook the same slots.
//!
//! The hook is a process-wide singleton, so two copies of the runtime (two
//! mods, each linking its own) cannot both exist in one test process. The
//! second module is therefore played by hand: it writes its own hook into the
//! slots, with the first module's hook as its "original", exactly as a second
//! `GameThread::install` in another DLL would. The slot-level logic is also
//! covered by unit tests in `patch.rs`.
//!
//! Everything runs in one test, in its own test binary.
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
use std::sync::atomic::{AtomicUsize, Ordering};

use common::{PROCESS_EVENT_INDEX, VTABLE, desc, image, object_addr, slot};
use ufathom_runtime::EngineDesc;
use ufathom_runtime::inproc::{GameThread, HookError};

type EngineFn = unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void);

const DESTROY: &str = "/Script/Engine.Actor:K2_DestroyActor";
/// Where the fake `UFunction` keeps `Func`: past `PropertiesSize` (0x50).
const FUNC: u64 = 0x98;
/// A module base no module has.
const NO_SUCH_MODULE: usize = 0x10;

/// Calls of the fake engine's `ProcessEvent`.
static PROCESS_EVENT_CALLS: AtomicUsize = AtomicUsize::new(0);
/// Calls of the fake native function.
static NATIVE_CALLS: AtomicUsize = AtomicUsize::new(0);
/// Calls of the second module's `ProcessEvent` hook and its native thunk.
static OTHER_PROCESS_EVENT_CALLS: AtomicUsize = AtomicUsize::new(0);
static OTHER_NATIVE_CALLS: AtomicUsize = AtomicUsize::new(0);
/// What the second module saved as the originals: the first module's hooks.
static OTHER_PROCESS_EVENT_ORIGINAL: AtomicUsize = AtomicUsize::new(0);
static OTHER_NATIVE_ORIGINAL: AtomicUsize = AtomicUsize::new(0);

unsafe extern "C" fn fake_process_event(_: *mut c_void, _: *mut c_void, _: *mut c_void) {
    PROCESS_EVENT_CALLS.fetch_add(1, Ordering::AcqRel);
}

unsafe extern "C" fn fake_native(_: *mut c_void, _: *mut c_void, result: *mut c_void) {
    NATIVE_CALLS.fetch_add(1, Ordering::AcqRel);
    // SAFETY: every caller passes a writable `i32`.
    unsafe { result.cast::<i32>().write(42) };
}

/// The second module's `ProcessEvent` hook: counts and forwards to what it
/// found in the slot.
unsafe extern "C" fn other_process_event(this: *mut c_void, f: *mut c_void, p: *mut c_void) {
    OTHER_PROCESS_EVENT_CALLS.fetch_add(1, Ordering::AcqRel);
    let original = OTHER_PROCESS_EVENT_ORIGINAL.load(Ordering::Acquire);
    // SAFETY: the test stores the first module's hook, a function with this
    // signature, before it installs this one.
    unsafe { core::mem::transmute::<usize, EngineFn>(original)(this, f, p) };
}

/// The second module's thunk for `Func`.
unsafe extern "C" fn other_native(this: *mut c_void, frame: *mut c_void, result: *mut c_void) {
    OTHER_NATIVE_CALLS.fetch_add(1, Ordering::AcqRel);
    let original = OTHER_NATIVE_ORIGINAL.load(Ordering::Acquire);
    // SAFETY: as for `other_process_event`.
    unsafe { core::mem::transmute::<usize, EngineFn>(original)(this, frame, result) };
}

fn word(addr: u64) -> u64 {
    // SAFETY: test addresses inside live test allocations.
    unsafe { (addr as *const u64).read_volatile() }
}

fn set_word(addr: u64, value: u64) {
    // SAFETY: test addresses inside live, writable test allocations.
    unsafe { (addr as *mut u64).write_volatile(value) };
}

/// Calls whatever the `ProcessEvent` slot of `vtable` holds, as the engine does.
fn engine_call(vtable: u64, this: u64, function: u64) {
    let entry = word(vtable + u64::from(PROCESS_EVENT_INDEX) * 8) as usize;
    let mut params = [0u64; 2];
    // SAFETY: the slot holds the fake or a hook forwarding to it.
    unsafe {
        core::mem::transmute::<usize, EngineFn>(entry)(
            this as *mut c_void,
            function as *mut c_void,
            params.as_mut_ptr().cast(),
        );
    }
}

/// Invokes a function through its `Func`, as the blueprint VM does. Returns
/// the result.
fn invoke(function: u64, context: u64) -> i32 {
    let mut frame = [0u64; 16];
    frame[2] = function;
    let mut result = 0i32;
    let func = word(function + FUNC) as usize;
    // SAFETY: `Func` holds the fake native or a thunk forwarding to it.
    unsafe {
        core::mem::transmute::<usize, EngineFn>(func)(
            context as *mut c_void,
            frame.as_mut_ptr().cast(),
            (&raw mut result).cast(),
        );
    }
    result
}

/// The base address of the test executable, which holds the closures below
/// and, statically linked, the runtime.
#[cfg(windows)]
fn this_module() -> usize {
    #[link(name = "kernel32", kind = "raw-dylib")]
    unsafe extern "system" {
        fn GetModuleHandleW(name: *const u16) -> *mut c_void;
    }
    // SAFETY: a null name asks for the module of the running executable.
    unsafe { GetModuleHandleW(core::ptr::null()) as usize }
}

/// There are no modules to tell apart on other platforms.
#[cfg(not(windows))]
fn this_module() -> usize {
    0
}

#[test]
fn hooks_are_released_by_guard_by_module_and_in_order() {
    let mut memory = vec![0u64; common::SIZE / 8];
    let base = memory.as_ptr() as u64;
    let bytes = image(base, fake_process_event as *const () as u64);
    for (word, chunk) in memory.iter_mut().zip(bytes.chunks_exact(8)) {
        *word = u64::from_le_bytes(chunk.try_into().unwrap());
    }
    let pawn = object_addr(base, slot::MY_PAWN);
    let destroy = object_addr(base, slot::DESTROY);
    set_word(destroy + FUNC, fake_native as *const () as u64);
    let vtable = base + VTABLE;
    let pe_slot = vtable + u64::from(PROCESS_EVENT_INDEX) * 8;
    let engine_pe = fake_process_event as *const () as u64;
    let engine_native = fake_native as *const () as u64;

    let d: &'static EngineDesc = Box::leak(Box::new(desc(base)));
    // SAFETY: the description matches the fake engine, which outlives the
    // hook; the hook is uninstalled before the memory is freed.
    let game = unsafe { GameThread::install(d) }.unwrap();
    assert_eq!(game.hooked_slots(), 1);
    let hook_a = word(pe_slot);
    assert_ne!(hook_a, engine_pe);

    // A guard removes its callback when dropped; `forget` keeps it.
    let id = {
        let guard = game.before(DESTROY, |_| {}).unwrap().into_guard(game);
        let id = guard.id();
        assert!(format!("{guard:?}").contains("HookGuard"));
        id
    };
    assert!(!game.remove(id), "the guard removed it on drop");
    let kept = game
        .before(DESTROY, |_| {})
        .unwrap()
        .into_guard(game)
        .forget();
    assert!(game.remove(kept), "forget left the callback registered");

    // A native guard restores `Func` when it drops.
    {
        let _guard = game
            .before_native(DESTROY, |_| {})
            .unwrap()
            .into_guard(game);
        assert_ne!(word(destroy + FUNC), engine_native, "hooked while guarded");
    }
    assert_eq!(word(destroy + FUNC), engine_native, "dropped: restored");

    // Removal by module takes callbacks, native callbacks and queued tasks of
    // that module, and nothing of another.
    let _a = game.before(DESTROY, |_| {}).unwrap();
    let _b = game.after(DESTROY, |_| {}).unwrap();
    let _c = game.before_native(DESTROY, |_| {}).unwrap();
    game.run(|_| {});
    assert_ne!(word(destroy + FUNC), engine_native);
    assert_eq!(game.remove_owned_by(NO_SUCH_MODULE), 0);
    assert_ne!(word(destroy + FUNC), engine_native, "nothing was removed");
    // Two ProcessEvent callbacks, one native callback and one task.
    assert_eq!(game.remove_owned_by(this_module()), 4);
    assert_eq!(
        word(destroy + FUNC),
        engine_native,
        "the last native callback of the module restored Func"
    );
    assert_eq!(game.remove_owned_by_current_module(), 0, "nothing is left");
    assert_eq!(game.hooked_slots(), 1, "removing callbacks keeps the hook");
    game.run(|_| {});
    assert_eq!(
        game.remove_owned_by_current_module(),
        1,
        "the runtime and the caller are one module here"
    );

    // Another module hooks the same `Func` and `ProcessEvent` slot after this
    // one and saves this one's hooks as its originals.
    let native_id = game.before_native(DESTROY, |_| {}).unwrap();
    let thunk = word(destroy + FUNC);
    assert_ne!(thunk, engine_native);
    OTHER_NATIVE_ORIGINAL.store(thunk as usize, Ordering::Release);
    set_word(destroy + FUNC, other_native as *const () as u64);
    OTHER_PROCESS_EVENT_ORIGINAL.store(hook_a as usize, Ordering::Release);
    set_word(pe_slot, other_process_event as *const () as u64);

    // Removing the last native callback cannot give `Func` back, but the
    // call still reaches the original through both thunks.
    assert!(game.remove(native_id));
    assert_eq!(word(destroy + FUNC), other_native as *const () as u64);
    let native_calls = NATIVE_CALLS.load(Ordering::Acquire);
    assert_eq!(invoke(destroy, pawn), 42);
    assert_eq!(NATIVE_CALLS.load(Ordering::Acquire), native_calls + 1);
    assert_eq!(OTHER_NATIVE_CALLS.load(Ordering::Acquire), 1);

    // Uninstalling cannot restore either slot: it says so, leaves both alone,
    // and does not report the module as unloadable.
    let error = game
        .uninstall_and_wait(std::time::Duration::from_millis(200))
        .unwrap_err();
    match error {
        HookError::Chained { blocked, .. } => assert_eq!(blocked, 2),
        other => panic!("expected Chained, got {other}"),
    }
    assert_eq!(word(pe_slot), other_process_event as *const () as u64);
    assert_eq!(word(destroy + FUNC), other_native as *const () as u64);
    assert_eq!(game.hooked_slots(), 1, "the slot is still tracked");
    assert!(matches!(
        game.uninstall(),
        Err(HookError::Chained { blocked: 2, .. })
    ));

    // The first module's hook keeps forwarding, for good.
    let calls = PROCESS_EVENT_CALLS.load(Ordering::Acquire);
    for _ in 0..8 {
        engine_call(vtable, pawn, destroy);
    }
    assert_eq!(PROCESS_EVENT_CALLS.load(Ordering::Acquire), calls + 8);
    assert_eq!(OTHER_PROCESS_EVENT_CALLS.load(Ordering::Acquire), 8);
    assert_eq!(invoke(destroy, pawn), 42);

    // A rescan does not patch what another module holds.
    assert_eq!(game.rescan().unwrap(), 0);
    assert_eq!(word(pe_slot), other_process_event as *const () as u64);

    // The other module unloads and gives the slots back; now this one can.
    set_word(pe_slot, hook_a);
    set_word(destroy + FUNC, thunk);
    assert!(
        game.uninstall_and_wait(std::time::Duration::from_secs(5))
            .unwrap()
    );
    assert_eq!(word(pe_slot), engine_pe);
    assert_eq!(word(destroy + FUNC), engine_native);
    assert_eq!(game.hooked_slots(), 0);
    engine_call(vtable, pawn, destroy);
    assert_eq!(invoke(destroy, pawn), 42);

    drop(memory);
}
