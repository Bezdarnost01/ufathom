//! Inline detours on hand-written machine code.
//!
//! The functions under test are bytes written into an executable page, so
//! the prologues are exactly what each test says, not whatever the compiler
//! chose. Nothing here has been run against a real game.
#![cfg(all(feature = "detour", windows, target_arch = "x86_64"))]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "tests may panic"
)]

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use ufathom_runtime::detour::{
    Detour, DetourError, Signature, find_in_module, in_flight, module_base, uninstall_all_and_wait,
};
use windows_sys::Win32::System::Memory::{
    MEM_COMMIT, MEM_RELEASE, MEM_RESERVE, PAGE_EXECUTE_READWRITE, PAGE_READWRITE, VirtualAlloc,
    VirtualFree,
};

/// Detours are process-wide state (the in-flight counter, the thread
/// suspension), so the tests of this file run one at a time.
static SERIAL: Mutex<()> = Mutex::new(());

fn serial() -> MutexGuard<'static, ()> {
    SERIAL.lock().unwrap_or_else(PoisonError::into_inner)
}

type IntFn = extern "system" fn(i32) -> i32;

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

fn call(address: usize, argument: i32) -> i32 {
    // SAFETY: tests only call addresses of the machine code they wrote, or
    // of a detour's trampoline, with the signature `int (int)`.
    let function = unsafe { core::mem::transmute::<usize, IntFn>(address) };
    function(argument)
}

/// `push rbx; sub rsp,0x20; mov eax,ecx; add eax,1; add rsp,0x20; pop rbx; ret`
const PLUS_ONE: [u8; 16] = [
    0x53, 0x48, 0x83, 0xEC, 0x20, 0x89, 0xC8, 0x83, 0xC0, 0x01, 0x48, 0x83, 0xC4, 0x20, 0x5B, 0xC3,
];
const PLUS_ONE_SIGNATURE: &str = "53 48 83 EC 20 89 C8 83 C0 01";

fn sig(text: &str) -> Signature {
    Signature::parse(text).unwrap()
}

extern "system" fn plus_1000(x: i32) -> i32 {
    x + 1000
}

fn install_plus_1000(target: usize, signature: &str) -> Result<Detour, DetourError> {
    // SAFETY: the target is test code with the signature `int (int)`;
    // `plus_1000` has the same signature and stays mapped.
    unsafe { Detour::install(target, &sig(signature), plus_1000 as *const ()) }
}

#[test]
fn replaces_calls_runs_the_original_and_restores_the_bytes() {
    let _lock = serial();
    let page = Page::executable();
    page.write(0, &PLUS_ONE);
    let target = page.address(0);
    let before = page.read(0, 16);
    assert_eq!(call(target, 4), 5);

    let detour = install_plus_1000(target, PLUS_ONE_SIGNATURE).unwrap();
    assert_eq!(detour.target(), target);
    assert_eq!(detour.stolen_len(), 5);
    assert_eq!(call(target, 4), 1004);
    // The jump is in place, padded with nothing (5 bytes stolen exactly).
    assert_eq!(page.read(0, 1), [0xE9]);
    // The trampoline runs the original code.
    assert_eq!(call(detour.original() as usize, 4), 5);
    // SAFETY: the target is `int (int)` and so is the type.
    let typed: IntFn = unsafe { detour.original_as() };
    assert_eq!(typed(9), 10);
    // The trampoline is within reach of a rel32 jump.
    assert!((detour.original() as usize).abs_diff(target) < 0x8000_0000);

    detour.uninstall().unwrap();
    assert_eq!(page.read(0, 16), before);
    assert_eq!(call(target, 4), 5);
}

static TEN_ORIGINAL: AtomicUsize = AtomicUsize::new(0);

ufathom_runtime::detour_fn! {
    fn times_ten(x: i32) -> i32 {
        let address = TEN_ORIGINAL.load(Ordering::Acquire);
        // SAFETY: the cell holds the trampoline of the detour that routes
        // here, which has the signature `int (int)`.
        let original = unsafe { core::mem::transmute::<usize, IntFn>(address) };
        original(x) * 10
    }
}

#[test]
fn a_replacement_can_call_the_original() {
    let _lock = serial();
    let page = Page::executable();
    page.write(0, &PLUS_ONE);
    let target = page.address(0);
    // SAFETY: as in `install_plus_1000`; `times_ten` calls the original only
    // through the cell the install fills.
    let detour = unsafe {
        Detour::install_with_original(
            target,
            &sig(PLUS_ONE_SIGNATURE),
            times_ten as *const (),
            &TEN_ORIGINAL,
        )
    }
    .unwrap();
    assert_eq!(call(target, 4), 50);
    assert_eq!(in_flight(), 0);
    assert!(detour.uninstall_and_wait(Duration::from_secs(5)).unwrap());
    assert_eq!(call(target, 4), 5);
}

#[test]
fn a_signature_mismatch_changes_nothing() {
    let _lock = serial();
    let page = Page::executable();
    page.write(0, &PLUS_ONE);
    let before = page.read(0, 16);
    let error = install_plus_1000(page.address(0), "53 48 83 EC 28 89 C8 83 C0 01").unwrap_err();
    assert!(
        matches!(
            error,
            DetourError::SignatureMismatch {
                offset: 4,
                found: 0x20,
                ..
            }
        ),
        "{error}"
    );
    assert_eq!(page.read(0, 16), before);
    assert_eq!(call(page.address(0), 1), 2);

    // Wildcards do not mask a real difference elsewhere.
    let error = install_plus_1000(page.address(0), "53 ?? ?? ?? ?? 89 C8 83 C0 02").unwrap_err();
    assert!(
        matches!(error, DetourError::SignatureMismatch { offset: 9, .. }),
        "{error}"
    );
    assert_eq!(page.read(0, 16), before);
}

#[test]
fn a_signature_shorter_than_the_patch_is_refused() {
    let _lock = serial();
    let page = Page::executable();
    page.write(0, &PLUS_ONE);
    let before = page.read(0, 16);
    // The first two instructions are 5 bytes; 4 are not enough.
    let error = install_plus_1000(page.address(0), "53 48 83 EC").unwrap_err();
    assert!(
        matches!(
            error,
            DetourError::SignatureTooShort {
                needed: 5,
                given: 4,
                ..
            }
        ),
        "{error}"
    );
    assert_eq!(page.read(0, 16), before);
}

#[test]
fn bad_targets_and_replacements_are_refused() {
    let _lock = serial();
    // Committed, readable, but not executable.
    // SAFETY: a fresh read-write page.
    let data = unsafe {
        VirtualAlloc(
            core::ptr::null(),
            0x1000,
            MEM_COMMIT | MEM_RESERVE,
            PAGE_READWRITE,
        )
    };
    assert!(!data.is_null());
    let error = install_plus_1000(data as usize, "00").unwrap_err();
    assert!(
        matches!(error, DetourError::NotExecutable { .. }),
        "{error}"
    );
    // SAFETY: releases the page above.
    unsafe { VirtualFree(data, 0, MEM_RELEASE) };

    let error = install_plus_1000(0, "00").unwrap_err();
    assert!(
        matches!(error, DetourError::NotExecutable { .. }),
        "{error}"
    );

    let page = Page::executable();
    page.write(0, &PLUS_ONE);
    // SAFETY: the null replacement is refused before anything is touched.
    let error =
        unsafe { Detour::install(page.address(0), &sig(PLUS_ONE_SIGNATURE), core::ptr::null()) }
            .unwrap_err();
    assert!(
        matches!(error, DetourError::BadReplacement { .. }),
        "{error}"
    );
    assert_eq!(call(page.address(0), 1), 2);
}

#[test]
fn rip_relative_operands_are_relocated() {
    let _lock = serial();
    let page = Page::executable();
    // lea rax, [rip + slot]; mov eax, [rax]; add eax, ecx; ret
    let slot = 0x800usize;
    let displacement = i32::try_from(slot - 7).unwrap().to_le_bytes();
    let mut code = vec![0x48, 0x8D, 0x05];
    code.extend_from_slice(&displacement);
    code.extend_from_slice(&[0x8B, 0x00, 0x01, 0xC8, 0xC3]);
    page.write(0, &code);
    page.write(slot, &100i32.to_le_bytes());
    let target = page.address(0);
    assert_eq!(call(target, 5), 105);

    let detour = install_plus_1000(target, "48 8D 05 ?? ?? ?? ?? 8B 00 01 C8").unwrap();
    assert_eq!(detour.stolen_len(), 7);
    assert_eq!(call(target, 5), 1005);
    // Read through the moved `lea`: the data slot is still found.
    assert_eq!(call(detour.original() as usize, 5), 105);
    page.write(slot, &200i32.to_le_bytes());
    assert_eq!(call(detour.original() as usize, 5), 205);
    detour.uninstall().unwrap();
    assert_eq!(call(target, 5), 205);
}

#[test]
fn functions_that_cannot_be_moved_are_refused() {
    let _lock = serial();
    let cases: [(&str, &[u8]); 3] = [
        // mov al, 1; ret; the ret is inside the first 5 bytes.
        ("B0 01 C3 90 90", &[0xB0, 0x01, 0xC3, 0x90, 0x90, 0x90]),
        // test ecx, ecx; jz back to the start.
        ("85 C9 74 FC 90", &[0x85, 0xC9, 0x74, 0xFC, 0x90, 0xC3]),
        // nop; int3 in the middle of the first bytes.
        ("90 CC 90 90 90", &[0x90, 0xCC, 0x90, 0x90, 0x90, 0xC3]),
    ];
    for (signature, code) in cases {
        let page = Page::executable();
        page.write(0, code);
        let before = page.read(0, 16);
        let error = install_plus_1000(page.address(0), signature).unwrap_err();
        assert!(
            matches!(error, DetourError::Unrelocatable { .. }),
            "{signature}: {error}"
        );
        assert_eq!(page.read(0, 16), before, "{signature}");
    }
}

#[test]
fn branches_are_relocated() {
    let _lock = serial();
    let page = Page::executable();
    // xor eax,eax; jz +0 (to the next instruction, inside the stolen bytes);
    // nop; add eax,7; ret. Always returns 7.
    page.write(0, &[0x31, 0xC0, 0x74, 0x00, 0x90, 0x83, 0xC0, 0x07, 0xC3]);
    // test ecx,ecx; jz out; nop; mov eax,5; ret; out: mov eax,9; ret.
    // The jz leaves the stolen bytes.
    page.write(
        0x100,
        &[
            0x85, 0xC9, 0x74, 0x07, 0x90, 0xB8, 0x05, 0, 0, 0, 0xC3, 0xB8, 0x09, 0, 0, 0, 0xC3,
        ],
    );
    // An import-thunk style function: jmp rel32 to a function at 0x300
    // that returns x + 2.
    let rel = i32::try_from(0x300usize - 0x205).unwrap().to_le_bytes();
    page.write(0x200, &[0xE9, rel[0], rel[1], rel[2], rel[3]]);
    page.write(0x300, &[0x8D, 0x41, 0x02, 0xC3]);

    let inside = page.address(0);
    let leaving = page.address(0x100);
    let thunk = page.address(0x200);
    assert_eq!(call(inside, 3), 7);
    assert_eq!((call(leaving, 0), call(leaving, 1)), (9, 5));
    assert_eq!(call(thunk, 3), 5);

    let one = install_plus_1000(inside, "31 C0 74 00 90").unwrap();
    let two = install_plus_1000(leaving, "85 C9 74 07 90").unwrap();
    let three = install_plus_1000(thunk, "E9 ?? ?? ?? ??").unwrap();
    for address in [inside, leaving, thunk] {
        assert_eq!(call(address, 3), 1003);
    }
    assert_eq!(call(one.original() as usize, 3), 7);
    assert_eq!(
        (
            call(two.original() as usize, 0),
            call(two.original() as usize, 1)
        ),
        (9, 5)
    );
    assert_eq!(call(three.original() as usize, 3), 5);
    for detour in [one, two, three] {
        detour.uninstall().unwrap();
    }
    assert_eq!(call(inside, 3), 7);
    assert_eq!((call(leaving, 0), call(leaving, 1)), (9, 5));
    assert_eq!(call(thunk, 3), 5);
}

#[test]
fn the_same_bytes_cannot_be_hooked_twice() {
    let _lock = serial();
    let page = Page::executable();
    page.write(0, &PLUS_ONE);
    let first = install_plus_1000(page.address(0), PLUS_ONE_SIGNATURE).unwrap();
    let error = install_plus_1000(page.address(0), PLUS_ONE_SIGNATURE).unwrap_err();
    assert!(
        matches!(error, DetourError::AlreadyHooked { .. }),
        "{error}"
    );
    // Overlapping, but not at the same address.
    let error = install_plus_1000(page.address(1), "48 83 EC 20 89").unwrap_err();
    assert!(
        matches!(error, DetourError::AlreadyHooked { .. }),
        "{error}"
    );
    first.uninstall().unwrap();
}

#[test]
fn foreign_patches_stop_an_uninstall_and_keep_the_trampoline() {
    let _lock = serial();
    let page = Page::executable();
    page.write(0, &PLUS_ONE);
    let target = page.address(0);
    let detour = install_plus_1000(target, PLUS_ONE_SIGNATURE).unwrap();
    let ours = page.read(0, 5);
    let trampoline = detour.original() as usize;

    // Someone else patches over our jump.
    page.write(0, &[0xE9, 1, 2, 3, 4]);
    let error = detour.uninstall().unwrap_err();
    assert!(matches!(error, DetourError::Modified { .. }), "{error}");
    // The trampoline is still alive and still runs the original.
    assert_eq!(call(trampoline, 4), 5);
    assert_eq!(page.read(0, 5), [0xE9, 1, 2, 3, 4]);

    // The detour stayed registered: once our bytes are back, the global
    // clean-up removes it.
    page.write(0, &ours);
    assert!(uninstall_all_and_wait(Duration::from_secs(5)).unwrap());
    assert_eq!(call(target, 4), 5);
}

static UNIQUE_BYTES: [u8; 12] = [
    0xA1, 0x5C, 0x7E, 0x3D, 0x91, 0x0B, 0xF4, 0x62, 0xD8, 0x17, 0xE3, 0x4A,
];

/// A function whose body holds a byte pattern found nowhere else in the
/// test executable. It is never called.
#[inline(never)]
fn marker_function() {
    // SAFETY: emits data bytes inside the function; the function is never
    // called, so they are never executed.
    unsafe {
        core::arch::asm!(
            "jmp 2f",
            ".byte 0x6B, 0x21, 0xD4, 0x09, 0xEE, 0x53, 0x88, 0x1F, 0xC7, 0xA0, 0x3B, 0x75",
            "2:",
            options(nomem, nostack)
        );
    }
}

#[test]
fn finds_signatures_in_the_executable_sections() {
    let _lock = serial();
    // Keep the function in the binary.
    let marker = core::hint::black_box(marker_function as *const () as usize);
    assert_ne!(marker, 0);

    let found = find_in_module(None, &sig("6B 21 D4 09 ?? 53 88 1F C7 A0 3B 75")).unwrap();
    let base = module_base(None).unwrap();
    assert!(found > base);
    assert!(found.abs_diff(marker) < 0x100);
    // SAFETY: the address was just found in this process's code.
    let bytes = unsafe { core::slice::from_raw_parts(found as *const u8, 4) };
    assert_eq!(bytes, [0x6B, 0x21, 0xD4, 0x09]);

    // The signature text is not in executable memory, and neither is this
    // array, so it has no match.
    assert!(UNIQUE_BYTES.len() == 12);
    let none = find_in_module(None, &sig("A1 5C 7E 3D 91 0B F4 62 D8 17 E3 4A")).unwrap_err();
    assert!(
        matches!(none, DetourError::SignatureMatches { matches: 0 }),
        "{none}"
    );
    assert!(none.to_string().contains('0'));

    // Padding between functions is a run of int3.
    let many = find_in_module(None, &sig("CC CC CC CC")).unwrap_err();
    let DetourError::SignatureMatches { matches } = many else {
        panic!("{many}");
    };
    assert!(matches > 1, "{matches}");
    assert!(many.to_string().contains(&matches.to_string()));

    // Another module, by name.
    let kernel = module_base(Some("kernel32.dll")).unwrap();
    assert_ne!(kernel, base);
    let error = find_in_module(Some("no-such-module-for-tests.dll"), &sig("90")).unwrap_err();
    assert!(matches!(error, DetourError::ModuleNotFound(_)), "{error}");
}

extern "system" fn plus_1001(x: i32) -> i32 {
    x + 1001
}

#[test]
fn installing_and_removing_under_a_running_caller_is_safe() {
    let _lock = serial();
    let page = Arc::new(Page::executable());
    page.write(0, &PLUS_ONE);
    let target = page.address(0);
    let stop = Arc::new(AtomicBool::new(false));
    let calls = Arc::new(AtomicUsize::new(0));
    let wrong = Arc::new(AtomicUsize::new(0));
    let worker = {
        let (stop, calls, wrong) = (stop.clone(), calls.clone(), wrong.clone());
        thread::spawn(move || {
            let mut x = 0i32;
            while !stop.load(Ordering::Relaxed) {
                x = (x + 1) & 0xFFFF;
                let result = call(target, x);
                if result != x + 1 && result != x + 1001 {
                    wrong.fetch_add(1, Ordering::Relaxed);
                }
                calls.fetch_add(1, Ordering::Relaxed);
            }
        })
    };

    for round in 0..100 {
        // SAFETY: `plus_1001` has the signature of the target.
        let detour =
            unsafe { Detour::install(target, &sig(PLUS_ONE_SIGNATURE), plus_1001 as *const ()) }
                .unwrap();
        thread::yield_now();
        if round % 25 == 0 {
            assert!(detour.uninstall_and_wait(Duration::from_secs(10)).unwrap());
        } else {
            detour.uninstall().unwrap();
        }
    }
    stop.store(true, Ordering::Relaxed);
    worker.join().unwrap();
    assert!(calls.load(Ordering::Relaxed) > 0);
    assert_eq!(wrong.load(Ordering::Relaxed), 0);
    assert_eq!(page.read(0, 16), PLUS_ONE);
}

static ENTERED: AtomicBool = AtomicBool::new(false);
static RELEASE: AtomicBool = AtomicBool::new(false);

ufathom_runtime::detour_fn! {
    fn blocker(x: i32) -> i32 {
        ENTERED.store(true, Ordering::Release);
        while !RELEASE.load(Ordering::Acquire) {
            thread::sleep(Duration::from_millis(1));
        }
        x + 2000
    }
}

fn install_blocker(target: usize) -> Detour {
    ENTERED.store(false, Ordering::Release);
    RELEASE.store(false, Ordering::Release);
    // SAFETY: `blocker` has the signature of the target.
    unsafe { Detour::install(target, &sig(PLUS_ONE_SIGNATURE), blocker as *const ()) }.unwrap()
}

fn wait_for(flag: &AtomicBool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !flag.load(Ordering::Acquire) {
        assert!(Instant::now() < deadline, "timed out");
        thread::sleep(Duration::from_millis(1));
    }
}

#[test]
fn uninstall_and_wait_waits_for_a_running_replacement() {
    let _lock = serial();
    let page = Arc::new(Page::executable());
    page.write(0, &PLUS_ONE);
    let target = page.address(0);

    // The replacement is blocked: the wait times out and leaks on purpose.
    let detour = install_blocker(target);
    let caller = thread::spawn(move || call(target, 1));
    wait_for(&ENTERED);
    assert_eq!(in_flight(), 1);
    let started = Instant::now();
    let idle = detour
        .uninstall_and_wait(Duration::from_millis(300))
        .unwrap();
    assert!(!idle);
    assert!(started.elapsed() >= Duration::from_millis(300));
    // The bytes are restored all the same.
    assert_eq!(page.read(0, 16), PLUS_ONE);
    RELEASE.store(true, Ordering::Release);
    assert_eq!(caller.join().unwrap(), 2001);
    assert_eq!(in_flight(), 0);

    // Released in time: the wait succeeds.
    let detour = install_blocker(target);
    let caller = thread::spawn(move || call(target, 1));
    wait_for(&ENTERED);
    let releaser = thread::spawn(|| {
        thread::sleep(Duration::from_millis(200));
        RELEASE.store(true, Ordering::Release);
    });
    assert!(detour.uninstall_and_wait(Duration::from_secs(10)).unwrap());
    releaser.join().unwrap();
    assert_eq!(caller.join().unwrap(), 2001);
    assert_eq!(call(target, 1), 2);
}

ufathom_runtime::detour_fn! {
    fn panics_for_non_negative(x: i32) -> i32 {
        assert!(x < 0, "the replacement panicked on purpose");
        x
    }
}

ufathom_runtime::detour_fn! {
    on_panic = -7;
    fn panics_with_a_fallback(x: i32) -> i32 {
        assert!(x < 0, "the replacement panicked on purpose");
        x
    }
}

#[test]
fn a_panic_in_a_replacement_does_not_unwind_into_the_caller() {
    let _lock = serial();
    let page = Page::executable();
    page.write(0, &PLUS_ONE);
    let target = page.address(0);

    // SAFETY: the replacements have the signature of the target.
    let detour = unsafe {
        Detour::install(
            target,
            &sig(PLUS_ONE_SIGNATURE),
            panics_for_non_negative as *const (),
        )
    }
    .unwrap();
    assert_eq!(call(target, 5), 0, "the default of i32");
    assert_eq!(call(target, -3), -3);
    assert_eq!(in_flight(), 0);
    detour.uninstall().unwrap();

    // SAFETY: as above.
    let detour = unsafe {
        Detour::install(
            target,
            &sig(PLUS_ONE_SIGNATURE),
            panics_with_a_fallback as *const (),
        )
    }
    .unwrap();
    assert_eq!(call(target, 5), -7);
    assert_eq!(in_flight(), 0);
    assert!(detour.uninstall_and_wait(Duration::from_secs(5)).unwrap());
}
