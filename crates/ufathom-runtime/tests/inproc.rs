//! `InProcessExecutor` against fake objects in this process's own memory,
//! with a fake virtual table whose `ProcessEvent` records the call.
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
use std::cell::RefCell;

use common::{PARAMS_SIZE, desc, image, object_addr, slot};
use ufathom_runtime::inproc::InProcessExecutor;
use ufathom_runtime::{CallError, EngineDesc, Executor, Ptr, SessionError, UObjectBase};

thread_local! {
    static CALLS: RefCell<Vec<(usize, usize)>> = const { RefCell::new(Vec::new()) };
}

/// Doubles the `i32` at offset 0 of the parameters into offset 8.
unsafe extern "C" fn fake_process_event(
    this: *mut c_void,
    function: *mut c_void,
    parms: *mut c_void,
) {
    CALLS.with(|calls| calls.borrow_mut().push((this as usize, function as usize)));
    // SAFETY: the test passes a 16-byte, 8-aligned block.
    unsafe {
        let input = parms.cast::<i32>().read();
        parms.cast::<u8>().add(8).cast::<i32>().write(input * 2);
    }
}

/// The fake engine in live memory. The buffer must outlive the executor.
struct Live {
    memory: Vec<u64>,
    base: u64,
}

fn live() -> Live {
    let mut memory = vec![0u64; common::SIZE / 8];
    let base = memory.as_ptr() as u64;
    let bytes = image(base, fake_process_event as *const () as u64);
    for (word, chunk) in memory.iter_mut().zip(bytes.chunks_exact(8)) {
        *word = u64::from_le_bytes(chunk.try_into().unwrap());
    }
    Live { memory, base }
}

fn attach(live: &Live) -> InProcessExecutor {
    let d: &'static EngineDesc = Box::leak(Box::new(desc(live.base)));
    // SAFETY: the description matches the fake engine in `live`, which stays
    // alive for the whole test, and every call is made on this thread.
    unsafe { InProcessExecutor::attach(d) }.unwrap()
}

const DESTROY: &str = "/Script/Engine.Actor:K2_DestroyActor";

#[test]
fn calls_process_event_through_the_vtable() {
    let live = live();
    let exec = attach(&live);
    let pawn = Ptr::<UObjectBase>::new(object_addr(live.base, slot::MY_PAWN));
    let mut params = [0u64; 2];
    let bytes: &mut [u8] = bytemuck_bytes(&mut params);
    bytes[..4].copy_from_slice(&21i32.to_le_bytes());

    CALLS.with(|calls| calls.borrow_mut().clear());
    exec.process_event(pawn, DESTROY, bytes).unwrap();
    // A second call uses the cached function.
    exec.process_event(pawn, DESTROY, bytes).unwrap();

    assert_eq!(i32::from_le_bytes(bytes[8..12].try_into().unwrap()), 42);
    let function = object_addr(live.base, slot::DESTROY) as usize;
    CALLS.with(|calls| {
        assert_eq!(
            *calls.borrow(),
            vec![(pawn.addr() as usize, function); 2],
            "this and the UFunction are passed as found"
        );
    });
    // The session is usable through the executor.
    assert_eq!(
        exec.session().path_of(pawn).unwrap(),
        "/Script/Engine.MyPawn"
    );
    drop(live.memory);
}

#[test]
fn refuses_bad_calls_without_running_anything() {
    let live = live();
    let exec = attach(&live);
    let pawn = Ptr::<UObjectBase>::new(object_addr(live.base, slot::MY_PAWN));
    let mut params = [0u8; PARAMS_SIZE as usize];
    CALLS.with(|calls| calls.borrow_mut().clear());

    let stale = Ptr::<UObjectBase>::new(object_addr(live.base, slot::MY_PAWN) + 8);
    assert!(matches!(
        exec.process_event(stale, DESTROY, &mut params),
        Err(CallError::InvalidObject { .. })
    ));
    assert!(matches!(
        exec.process_event(Ptr::NULL, DESTROY, &mut params),
        Err(CallError::InvalidObject { addr: 0 })
    ));
    assert!(matches!(
        exec.process_event(pawn, "/Script/Engine.Actor:Nope", &mut params),
        Err(CallError::FunctionNotFound(_))
    ));
    // A class is not a function.
    assert!(matches!(
        exec.process_event(pawn, "/Script/Engine.Actor", &mut params),
        Err(CallError::FunctionNotFound(_))
    ));
    assert!(matches!(
        exec.process_event(pawn, DESTROY, &mut params[..8]),
        Err(CallError::ParamsSize {
            expected: 16,
            actual: 8
        })
    ));
    CALLS.with(|calls| assert!(calls.borrow().is_empty()));
    drop(live.memory);
}

#[test]
fn attach_needs_the_process_event_index() {
    let live = live();
    let mut d = desc(live.base);
    d.process_event_index = None;
    let d: &'static EngineDesc = Box::leak(Box::new(d));
    // SAFETY: attach fails before anything is called.
    let result = unsafe { InProcessExecutor::attach(d) };
    assert!(matches!(result, Err(SessionError::Unknown(_))));
}

fn bytemuck_bytes(words: &mut [u64; 2]) -> &mut [u8] {
    // SAFETY: 16 initialised bytes, uniquely borrowed.
    unsafe { core::slice::from_raw_parts_mut(words.as_mut_ptr().cast::<u8>(), 16) }
}
