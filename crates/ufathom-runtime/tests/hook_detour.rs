//! `HookMode::Detour`: `ProcessEvent` hooked once, in the engine's own code.
//!
//! The fake engine's `ProcessEvent` is machine code in an executable page, so
//! its prologue is exactly what this file says and the detour analysis accepts
//! or refuses it deterministically: `push rbx; sub rsp, 0x20` (5 bytes), then
//! a call into a Rust function, then the matching epilogue. Nothing here has
//! been run against a real game.
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

/// What a build without the `detour` feature (or off Windows x86-64) does.
#[cfg(not(all(feature = "detour", windows, target_arch = "x86_64")))]
mod unavailable {
    use super::common::desc;
    use ufathom_runtime::EngineDesc;
    use ufathom_runtime::inproc::{GameThread, HookError, HookMode};

    #[test]
    fn the_detour_mode_is_refused_without_the_feature() {
        let d: &'static EngineDesc = Box::leak(Box::new(desc(0)));
        // SAFETY: the mode is refused before the description is used.
        let error = unsafe { GameThread::install_with(d, HookMode::Detour) }.unwrap_err();
        assert!(matches!(error, HookError::DetourUnavailable), "{error}");
        assert!(GameThread::get().is_none(), "nothing was installed");
    }
}

#[cfg(all(feature = "detour", windows, target_arch = "x86_64"))]
mod enabled {
    use core::ffi::c_void;
    use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
    use std::sync::{Arc, mpsc};
    use std::thread::{self, JoinHandle};
    use std::time::{Duration, Instant};

    use super::common::{PROCESS_EVENT_INDEX, VTABLE, desc, image, object_addr, slot};
    use ufathom_runtime::detour::in_flight;
    use ufathom_runtime::inproc::{GameFunction, GameThread, HookError, HookMode, Script, When};
    use ufathom_runtime::{EngineDesc, Pod};
    use windows_sys::Win32::System::Memory::{
        MEM_COMMIT, MEM_RELEASE, MEM_RESERVE, PAGE_EXECUTE_READWRITE, VirtualAlloc, VirtualFree,
    };

    type ProcessEventFn = unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void);

    const DESTROY: &str = "/Script/Engine.Actor:K2_DestroyActor";
    const WAIT: Duration = Duration::from_secs(10);

    /// Where each piece of machine code lives in the page.
    const ENGINE_AT: usize = 0x000;
    const OVERRIDE_AT: usize = 0x100;
    const RET_AT: usize = 0x200;
    const FOREIGN_AT: usize = 0x300;

    /// Calls of the engine's `ProcessEvent` body, and of an override of it.
    static ENGINE_CALLS: AtomicUsize = AtomicUsize::new(0);
    static OVERRIDE_CALLS: AtomicUsize = AtomicUsize::new(0);
    /// The engine's `ProcessEvent`, which the override calls as its base.
    static BASE: AtomicUsize = AtomicUsize::new(0);
    /// Makes the `before` callback of the blocking test wait.
    static BLOCK: AtomicBool = AtomicBool::new(false);
    static BLOCKED: AtomicBool = AtomicBool::new(false);

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

    impl GameFunction for DestroyParams {
        const PATH: &'static str = DESTROY;
        type Kind = Script;
        type Params = Self;
    }

    /// The engine's `ProcessEvent`: doubles the input into the output.
    unsafe extern "C" fn engine_body(_: *mut c_void, _: *mut c_void, parms: *mut c_void) {
        ENGINE_CALLS.fetch_add(1, Ordering::AcqRel);
        if !parms.is_null() {
            // SAFETY: every caller passes a 16-byte, 8-aligned block.
            unsafe {
                let input = parms.cast::<i32>().read();
                parms.cast::<u8>().add(8).cast::<i32>().write(input * 2);
            }
        }
    }

    /// A class's own `ProcessEvent`: counts, then calls the base one.
    unsafe extern "C" fn override_body(this: *mut c_void, f: *mut c_void, parms: *mut c_void) {
        OVERRIDE_CALLS.fetch_add(1, Ordering::AcqRel);
        let base = BASE.load(Ordering::Acquire);
        // SAFETY: the test stores the engine's stub, a function with this
        // signature, before any override runs.
        unsafe { core::mem::transmute::<usize, ProcessEventFn>(base)(this, f, parms) };
    }

    /// `push rbx; sub rsp,0x20; mov rax,<body>; call rax; add rsp,0x20; pop rbx; ret`.
    /// The arguments (rcx, rdx, r8) pass through untouched. The detour steals
    /// the first two instructions, 5 bytes.
    fn stub(body: usize) -> Vec<u8> {
        let mut code = vec![0x53, 0x48, 0x83, 0xEC, 0x20, 0x48, 0xB8];
        code.extend_from_slice(&body.to_le_bytes());
        code.extend_from_slice(&[0xFF, 0xD0, 0x48, 0x83, 0xC4, 0x20, 0x5B, 0xC3]);
        code
    }

    /// An executable, writable page filled with `int3`.
    struct Page {
        base: *mut u8,
    }

    // SAFETY: the page is plain memory; tests hand its address to other threads.
    unsafe impl Send for Page {}
    // SAFETY: as above.
    unsafe impl Sync for Page {}

    impl Page {
        fn executable() -> Self {
            // SAFETY: reserves and commits a fresh page.
            let base = unsafe {
                VirtualAlloc(
                    core::ptr::null(),
                    0x1000,
                    MEM_COMMIT | MEM_RESERVE,
                    PAGE_EXECUTE_READWRITE,
                )
            }
            .cast::<u8>();
            assert!(!base.is_null());
            // SAFETY: the page is committed and writable.
            unsafe { core::ptr::write_bytes(base, 0xCC, 0x1000) };
            Self { base }
        }

        fn write(&self, offset: usize, bytes: &[u8]) {
            assert!(offset + bytes.len() <= 0x1000);
            // SAFETY: inside the committed, writable page.
            unsafe {
                core::ptr::copy_nonoverlapping(bytes.as_ptr(), self.base.add(offset), bytes.len())
            };
        }

        fn read(&self, offset: usize, len: usize) -> Vec<u8> {
            assert!(offset + len <= 0x1000);
            // SAFETY: inside the committed page.
            unsafe { core::slice::from_raw_parts(self.base.add(offset), len) }.to_vec()
        }

        fn address(&self, offset: usize) -> usize {
            self.base as usize + offset
        }
    }

    impl Drop for Page {
        fn drop(&mut self) {
            // SAFETY: releases the page allocated in `executable`.
            unsafe { VirtualFree(self.base.cast(), 0, MEM_RELEASE) };
        }
    }

    /// The thread that plays the engine: it runs jobs one after another.
    struct Engine {
        jobs: Option<mpsc::Sender<Box<dyn FnOnce() + Send>>>,
        handle: Option<JoinHandle<()>>,
        id: u64,
    }

    impl Engine {
        fn start() -> Self {
            let (jobs, queue) = mpsc::channel::<Box<dyn FnOnce() + Send>>();
            let (id_tx, id_rx) = mpsc::channel();
            let handle = thread::spawn(move || {
                id_tx.send(GameThread::current_thread_id()).unwrap();
                while let Ok(job) = queue.recv() {
                    job();
                }
            });
            Self {
                jobs: Some(jobs),
                handle: Some(handle),
                id: id_rx.recv().unwrap(),
            }
        }

        /// Queues a job without waiting for it.
        fn post(&self, job: impl FnOnce() + Send + 'static) {
            self.jobs.as_ref().unwrap().send(Box::new(job)).unwrap();
        }

        /// Runs a job on the engine thread and returns its result.
        fn run<R: Send + 'static>(&self, job: impl FnOnce() -> R + Send + 'static) -> R {
            let (result, done) = mpsc::channel();
            self.post(move || result.send(job()).unwrap());
            done.recv_timeout(WAIT).unwrap()
        }

        fn stop(mut self) {
            drop(self.jobs.take());
            self.handle.take().unwrap().join().unwrap();
        }
    }

    /// Calls whatever the `ProcessEvent` slot of `vtable` holds, as the engine
    /// does, and returns the output.
    fn engine_call(vtable: u64, this: u64, function: u64, input: i32) -> i32 {
        let mut params = [0u64; 2];
        let block = params.as_mut_ptr().cast::<u8>();
        let slot = (vtable + u64::from(PROCESS_EVENT_INDEX) * 8) as *const usize;
        // SAFETY: `params` is 16 bytes; the slot holds a function with the
        // `ProcessEvent` signature.
        unsafe {
            block.cast::<i32>().write(input);
            let entry = slot.read_volatile();
            let f = core::mem::transmute::<usize, ProcessEventFn>(entry);
            f(this as *mut c_void, function as *mut c_void, block.cast());
            block.add(8).cast::<i32>().read()
        }
    }

    fn word(addr: u64) -> u64 {
        // SAFETY: test addresses inside live test allocations.
        unsafe { (addr as *const u64).read_volatile() }
    }

    fn wait_for(what: &str, mut done: impl FnMut() -> bool) {
        let start = Instant::now();
        while !done() {
            assert!(start.elapsed() < WAIT, "timed out waiting for {what}");
            thread::sleep(Duration::from_millis(1));
        }
    }

    /// Where the `jmp rel32` at the start of `page` leads.
    fn jump_destination(page: &Page, offset: usize) -> usize {
        let bytes = page.read(offset, 5);
        assert_eq!(bytes[0], 0xE9, "a jmp rel32");
        let displacement = i32::from_le_bytes(bytes[1..5].try_into().unwrap());
        page.address(offset + 5)
            .wrapping_add_signed(displacement as isize)
    }

    /// `E9 rel32` from `from` to `to`.
    fn jump(from: usize, to: usize) -> [u8; 5] {
        let displacement = i32::try_from(to as i64 - (from as i64 + 5)).unwrap();
        let [a, b, c, d] = displacement.to_le_bytes();
        [0xE9, a, b, c, d]
    }

    /// Counts calls of one kind and remembers the object of the last.
    #[derive(Default)]
    struct Seen {
        calls: AtomicUsize,
        object: AtomicU64,
    }

    fn counting(
        seen: &Arc<Seen>,
    ) -> impl Fn(&mut ufathom_runtime::inproc::CallContext<'_>) + use<> {
        let seen = Arc::clone(seen);
        move |ctx| {
            seen.object.store(ctx.object().addr(), Ordering::Release);
            seen.calls.fetch_add(1, Ordering::AcqRel);
        }
    }

    #[test]
    fn process_event_is_hooked_once_in_the_engines_own_code() {
        let page = Page::executable();
        let engine_stub = page.address(ENGINE_AT);
        BASE.store(engine_stub, Ordering::Release);
        page.write(ENGINE_AT, &stub(engine_body as *const () as usize));
        page.write(OVERRIDE_AT, &stub(override_body as *const () as usize));
        // A function that cannot be detoured: it returns at once.
        page.write(RET_AT, &[0xC3, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90]);
        let ret_stub = page.address(RET_AT);
        let override_stub = page.address(OVERRIDE_AT);
        let original_code = page.read(ENGINE_AT, 0x40);

        // The vote first elects the function that cannot be moved.
        let mut memory = vec![0u64; super::common::SIZE / 8];
        let base = memory.as_ptr() as u64;
        let bytes = image(base, ret_stub as u64);
        for (word, chunk) in memory.iter_mut().zip(bytes.chunks_exact(8)) {
            *word = u64::from_le_bytes(chunk.try_into().unwrap());
        }
        let vtable = base + VTABLE;
        let pe_slot = ((VTABLE + u64::from(PROCESS_EVENT_INDEX) * 8) / 8) as usize;
        let pawn = object_addr(base, slot::MY_PAWN);
        let actor_1 = object_addr(base, slot::ACTOR_1);
        let destroy = object_addr(base, slot::DESTROY);
        let d: &'static EngineDesc = Box::leak(Box::new(desc(base)));

        // SAFETY: the description matches the fake engine, which outlives the
        // hook; the hook is uninstalled before the memory is freed. The
        // elected function is the fake's own `ProcessEvent`.
        let error = unsafe { GameThread::install_with(d, HookMode::Detour) }.unwrap_err();
        assert!(
            matches!(&error, HookError::Detour { target, reason }
                if *target == ret_stub as u64 && reason.contains("ret")),
            "{error}"
        );
        assert_eq!(
            page.read(RET_AT, 8)[0],
            0xC3,
            "a refused function is untouched"
        );
        let game = GameThread::get().expect("the state exists after a failed attempt");
        assert_eq!(game.mode(), HookMode::Detour);
        assert_eq!(game.detour_target(), None);

        // The engine's real function wins the vote; a second attempt works.
        memory[pe_slot] = engine_stub as u64;
        // SAFETY: as above.
        let installed = unsafe { GameThread::install_with(d, HookMode::Detour) }.unwrap();
        assert!(core::ptr::eq(installed, game));
        assert_eq!(game.detour_target(), Some(engine_stub));
        assert_eq!(
            page.read(ENGINE_AT, 1),
            [0xE9],
            "a jmp rel32 over the prologue"
        );
        assert_eq!(
            page.read(ENGINE_AT + 5, 0x3B),
            original_code[5..0x40],
            "only the stolen bytes changed"
        );
        assert_eq!(
            word(vtable + u64::from(PROCESS_EVENT_INDEX) * 8),
            engine_stub as u64,
            "no virtual table slot is touched"
        );
        assert_eq!(game.hooked_slots(), 0);

        // The vtable machinery has nothing to do in this mode.
        assert_eq!(game.rescan().unwrap(), 0);
        assert!(!game.start_auto_rescan(Duration::from_secs(1)).unwrap());
        assert!(!game.auto_rescan_running());
        assert!(!game.start_rescan_on_module_load().unwrap());
        assert!(!game.rescan_on_module_load_running());
        // SAFETY: as above.
        let conflict = unsafe { GameThread::install(d) }.unwrap_err();
        assert!(
            matches!(
                conflict,
                HookError::ModeConflict {
                    installed: HookMode::Detour,
                    requested: HookMode::Vtable
                }
            ),
            "{conflict}"
        );

        // The thread that plays the engine is the game thread.
        let engine = Engine::start();
        assert!(game.set_game_thread(engine.id));

        let before = Arc::new(Seen::default());
        let after = Arc::new(Seen::default());
        game.before(DESTROY, counting(&before)).unwrap();
        game.after(DESTROY, counting(&after)).unwrap();
        let typed_inputs = Arc::new(AtomicUsize::new(0));
        {
            let typed_inputs = Arc::clone(&typed_inputs);
            game.hook::<DestroyParams>(When::Before, move |_ctx, params| {
                typed_inputs.fetch_add(params.input as usize, Ordering::AcqRel);
            })
            .unwrap();
        }

        // A call through the shared virtual table: once each, forwarded.
        let engine_calls = ENGINE_CALLS.load(Ordering::Acquire);
        let output = engine.run(move || engine_call(vtable, pawn, destroy, 21));
        assert_eq!(output, 42, "the original ran");
        assert_eq!(ENGINE_CALLS.load(Ordering::Acquire), engine_calls + 1);
        assert_eq!(before.calls.load(Ordering::Acquire), 1);
        assert_eq!(after.calls.load(Ordering::Acquire), 1);
        assert_eq!(before.object.load(Ordering::Acquire), pawn);
        assert_eq!(typed_inputs.load(Ordering::Acquire), 21);

        // A class whose virtual table appears after install: no rescan.
        let mut second_vtable = vec![0u64; 8];
        second_vtable[PROCESS_EVENT_INDEX as usize] = engine_stub as u64;
        let second = second_vtable.as_ptr() as u64;
        memory[((actor_1 - base) / 8) as usize] = second;
        assert_eq!(game.hooked_slots(), 0);
        let output = engine.run(move || engine_call(second, actor_1, destroy, 5));
        assert_eq!(output, 10);
        assert_eq!(before.calls.load(Ordering::Acquire), 2);
        assert_eq!(
            before.object.load(Ordering::Acquire),
            actor_1,
            "the object of the new class"
        );

        // A class that overrides ProcessEvent and calls the base one.
        let mut third_vtable = vec![0u64; 8];
        third_vtable[PROCESS_EVENT_INDEX as usize] = override_stub as u64;
        let third = third_vtable.as_ptr() as u64;
        let output = engine.run(move || engine_call(third, pawn, destroy, 4));
        assert_eq!(output, 8);
        assert_eq!(OVERRIDE_CALLS.load(Ordering::Acquire), 1);
        assert_eq!(
            before.calls.load(Ordering::Acquire),
            3,
            "fired through the base"
        );
        assert_eq!(after.calls.load(Ordering::Acquire), 3);

        // Another thread passes straight through.
        assert_eq!(engine_call(vtable, pawn, destroy, 3), 6);
        assert_eq!(before.calls.load(Ordering::Acquire), 3);

        // A callback may skip the original.
        let skip = game
            .before(DESTROY, |ctx| {
                if ctx
                    .params_as::<DestroyParams>()
                    .is_some_and(|p| p.input == 7)
                {
                    ctx.skip_original();
                }
            })
            .unwrap();
        let engine_calls = ENGINE_CALLS.load(Ordering::Acquire);
        assert_eq!(engine.run(move || engine_call(vtable, pawn, destroy, 7)), 0);
        assert_eq!(
            ENGINE_CALLS.load(Ordering::Acquire),
            engine_calls,
            "skipped"
        );
        assert!(game.remove(skip));

        // Queued tasks run at the next call on the game thread.
        let ran = Arc::new(AtomicBool::new(false));
        {
            let ran = Arc::clone(&ran);
            game.run(move |ctx| {
                assert!(ctx.session().num_objects().is_ok());
                ran.store(true, Ordering::Release);
            });
        }
        assert!(!ran.load(Ordering::Acquire));
        engine.run(move || engine_call(vtable, pawn, destroy, 1));
        assert!(ran.load(Ordering::Acquire));

        // Another tool patched over the detour and forwards to it: the bytes
        // are not ours to restore, the hook keeps working.
        let ours = page.read(ENGINE_AT, 5);
        let relay = jump_destination(&page, ENGINE_AT);
        let mut chained = vec![0xFF, 0x25, 0, 0, 0, 0];
        chained.extend_from_slice(&(relay as u64).to_le_bytes());
        page.write(FOREIGN_AT, &chained);
        page.write(ENGINE_AT, &jump(engine_stub, page.address(FOREIGN_AT)));
        let calls = before.calls.load(Ordering::Acquire);
        assert_eq!(engine.run(move || engine_call(vtable, pawn, destroy, 2)), 4);
        assert_eq!(
            before.calls.load(Ordering::Acquire),
            calls + 1,
            "still hooked"
        );
        let error = game.uninstall().unwrap_err();
        assert!(
            matches!(
                error,
                HookError::Chained { slot, holder, blocked: 1 }
                    if slot == engine_stub as u64 && holder == page.address(FOREIGN_AT) as u64
            ),
            "{error}"
        );
        assert!(game.uninstall_and_wait(Duration::from_millis(200)).is_err());
        assert_eq!(game.detour_target(), Some(engine_stub), "kept for a retry");
        // The other tool lets go; the hook is whole again.
        page.write(ENGINE_AT, &ours);

        // A callback is still inside the hook when the module "unloads": the
        // bytes are restored at once, the wait reports the thread, and it
        // succeeds after the callback returns.
        game.before(DESTROY, |_| {
            if BLOCK.load(Ordering::Acquire) {
                BLOCKED.store(true, Ordering::Release);
                while BLOCK.load(Ordering::Acquire) {
                    thread::sleep(Duration::from_millis(1));
                }
            }
        })
        .unwrap();
        BLOCK.store(true, Ordering::Release);
        engine.post(move || {
            engine_call(vtable, pawn, destroy, 6);
        });
        wait_for("the callback to block", || BLOCKED.load(Ordering::Acquire));
        assert!(!game.uninstall_and_wait(Duration::from_millis(300)).unwrap());
        assert_eq!(
            page.read(ENGINE_AT, 0x40),
            original_code,
            "the bytes are back while a thread is still inside"
        );
        assert_eq!(game.detour_target(), None);
        assert!(in_flight() >= 1, "the detour counts the blocked thread");
        BLOCK.store(false, Ordering::Release);
        engine.run(|| ());
        assert!(game.uninstall_and_wait(WAIT).unwrap());
        assert_eq!(in_flight(), 0);
        assert_eq!(page.read(ENGINE_AT, 0x40), original_code);

        // An uninstalled detour is not installed again, and calls reach the
        // engine without the hook.
        let unused: &'static EngineDesc = Box::leak(Box::new(desc(0)));
        // SAFETY: the hook exists, so nothing of `unused` is read.
        let again = unsafe { GameThread::install_with(unused, HookMode::Detour) }.unwrap();
        assert!(core::ptr::eq(again, game));
        assert_eq!(game.detour_target(), None);
        assert_eq!(game.rescan().unwrap(), 0);
        let calls = before.calls.load(Ordering::Acquire);
        let engine_calls = ENGINE_CALLS.load(Ordering::Acquire);
        assert_eq!(
            engine.run(move || engine_call(vtable, pawn, destroy, 9)),
            18
        );
        assert_eq!(before.calls.load(Ordering::Acquire), calls, "unhooked");
        assert_eq!(ENGINE_CALLS.load(Ordering::Acquire), engine_calls + 1);

        engine.stop();
        drop((second_vtable, third_vtable, memory));
    }
}
