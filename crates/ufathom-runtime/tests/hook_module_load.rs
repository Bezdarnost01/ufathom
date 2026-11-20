//! Rescanning when a module loads: a class that appears in a module loaded
//! after `install` is hooked by the rescans the loader notification
//! schedules, with no timer running.
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

use common::{desc, image};
use ufathom_runtime::EngineDesc;
use ufathom_runtime::inproc::GameThread;

unsafe extern "C" fn fake_process_event(_: *mut c_void, _: *mut c_void, _: *mut c_void) {}

/// The fake engine in live memory, hooked. Returns the memory, which must
/// outlive the hook, and its base address.
fn hooked_engine() -> (Vec<u64>, u64, &'static GameThread) {
    let mut memory = vec![0u64; common::SIZE / 8];
    let base = memory.as_ptr() as u64;
    let bytes = image(base, fake_process_event as *const () as u64);
    for (word, chunk) in memory.iter_mut().zip(bytes.chunks_exact(8)) {
        *word = u64::from_le_bytes(chunk.try_into().unwrap());
    }
    let d: &'static EngineDesc = Box::leak(Box::new(desc(base)));
    // SAFETY: the description matches the fake engine, which outlives the
    // hook; the test uninstalls the hook before the memory is freed.
    let game = unsafe { GameThread::install(d) }.unwrap();
    assert_eq!(game.hooked_slots(), 1);
    (memory, base, game)
}

/// Without module load notifications nothing is started.
#[cfg(not(windows))]
#[test]
fn starting_the_module_load_rescan_is_a_no_op_here() {
    let (memory, _base, game) = hooked_engine();
    assert!(!game.start_rescan_on_module_load().unwrap());
    assert!(!game.rescan_on_module_load_running());
    assert_eq!(GameThread::module_loads_seen(), 0);
    game.stop_rescan_on_module_load().unwrap();
    assert!(
        game.uninstall_and_wait(std::time::Duration::from_secs(5))
            .unwrap()
    );
    drop(memory);
}

#[cfg(windows)]
mod windows_only {
    use std::thread;
    use std::time::{Duration, Instant};

    use super::{fake_process_event, hooked_engine};
    use crate::common::{PROCESS_EVENT_INDEX, object_addr, slot};
    use ufathom_runtime::inproc::{FIRST_RESCAN, GameThread, SECOND_RESCAN};

    const WAIT: Duration = Duration::from_secs(20);

    mod win {
        use core::ffi::c_void;

        #[link(name = "kernel32", kind = "raw-dylib")]
        unsafe extern "system" {
            pub fn LoadLibraryW(name: *const u16) -> *mut c_void;
            pub fn FreeLibrary(module: *mut c_void) -> i32;
            pub fn GetModuleHandleW(name: *const u16) -> *mut c_void;
        }

        /// Small system DLLs a test process does not load by itself.
        const CANDIDATES: [&str; 4] = ["msimg32.dll", "dwmapi.dll", "winmm.dll", "wsock32.dll"];

        fn wide(name: &str) -> Vec<u16> {
            name.encode_utf16().chain(Some(0)).collect()
        }

        /// A candidate that is not loaded in this process.
        pub fn absent_dll() -> Vec<u16> {
            CANDIDATES
                .iter()
                .map(|name| wide(name))
                // SAFETY: `name` is NUL-terminated.
                .find(|name| unsafe { GetModuleHandleW(name.as_ptr()) }.is_null())
                .expect("one of the candidate system DLLs is not loaded")
        }
    }

    fn wait_for(what: &str, mut done: impl FnMut() -> bool) {
        let start = Instant::now();
        while !done() {
            assert!(start.elapsed() < WAIT, "timed out waiting for {what}");
            thread::sleep(Duration::from_millis(5));
        }
    }

    fn word(addr: u64) -> u64 {
        // SAFETY: test addresses inside live test allocations.
        unsafe { (addr as *const u64).read_volatile() }
    }

    #[test]
    fn a_module_load_triggers_two_rescans_that_hook_new_classes() {
        assert_eq!(FIRST_RESCAN, Duration::from_secs(1));
        assert_eq!(SECOND_RESCAN, Duration::from_secs(5));
        let (mut memory, base, game) = hooked_engine();
        let engine_pe = fake_process_event as *const () as u64;
        // A second class whose virtual table no object uses yet.
        let mut second_vtable = vec![0u64; 8];
        second_vtable[PROCESS_EVENT_INDEX as usize] = engine_pe;
        let second = second_vtable.as_ptr() as u64;
        let second_slot = second + u64::from(PROCESS_EVENT_INDEX) * 8;

        assert!(!game.rescan_on_module_load_running());
        assert!(game.start_rescan_on_module_load().unwrap());
        assert!(
            !game.start_rescan_on_module_load().unwrap(),
            "already running"
        );
        assert!(game.rescan_on_module_load_running());

        // An object of a class with a new virtual table appears, then a module
        // loads: the loader notification schedules the rescans that find it.
        let actor_1 = object_addr(base, slot::ACTOR_1);
        memory[((actor_1 - base) / 8) as usize] = second;
        let dll = win::absent_dll();
        let loads = GameThread::module_loads_seen();
        let rescans = game.rescans_run();
        let loaded_at = Instant::now();
        // SAFETY: `dll` is NUL-terminated; the library is a system DLL.
        let module = unsafe { win::LoadLibraryW(dll.as_ptr()) };
        assert!(!module.is_null(), "LoadLibraryW failed");
        assert!(
            GameThread::module_loads_seen() > loads,
            "the loader notification reported the load"
        );

        // Not at once: the module's classes are registered after it loads.
        thread::sleep(FIRST_RESCAN / 4);
        assert_eq!(game.rescans_run(), rescans);
        assert_eq!(word(second_slot), engine_pe);

        wait_for("the first rescan to hook the new class", || {
            word(second_slot) != engine_pe
        });
        assert!(loaded_at.elapsed() >= FIRST_RESCAN, "not before the delay");
        assert_eq!(game.hooked_slots(), 2);
        wait_for("both rescans", || game.rescans_run() >= rescans + 2);
        assert!(loaded_at.elapsed() >= SECOND_RESCAN, "not before the delay");

        // Nothing is scheduled while no module loads.
        let settled = game.rescans_run();
        thread::sleep(Duration::from_millis(1500));
        assert_eq!(game.rescans_run(), settled);

        // SAFETY: `module` was returned by `LoadLibraryW` above.
        unsafe { win::FreeLibrary(module) };

        // Uninstalling stops the thread and removes the notification: a
        // load afterwards is not reported.
        assert!(game.uninstall_and_wait(WAIT).unwrap());
        assert!(!game.rescan_on_module_load_running());
        game.stop_rescan_on_module_load().unwrap();
        assert_eq!(game.hooked_slots(), 0);
        assert_eq!(word(second_slot), engine_pe, "restored");
        // SAFETY: `dll` is NUL-terminated.
        if unsafe { win::GetModuleHandleW(dll.as_ptr()) }.is_null() {
            let loads = GameThread::module_loads_seen();
            // SAFETY: as above.
            let again = unsafe { win::LoadLibraryW(dll.as_ptr()) };
            assert!(!again.is_null());
            assert_eq!(
                GameThread::module_loads_seen(),
                loads,
                "the notification is gone"
            );
            // SAFETY: returned by `LoadLibraryW` above.
            unsafe { win::FreeLibrary(again) };
        }
        drop(second_vtable);
        drop(memory);
    }
}
