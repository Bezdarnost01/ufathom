//! A fault-guarded memory copy for Windows x86-64.
//!
//! [`guarded_copy`] copies bytes from an arbitrary address with plain loads and
//! turns a hardware fault into an error value. It is the only place in this
//! crate that dereferences an address of the target, and it exists so that
//! in-process reads can cost a few nanoseconds instead of a system call.
//!
//! # How a fault becomes an error
//!
//! Rust has no `__try`, so the copy is a hand-written assembly routine that
//! carries its own structured exception handling record: the assembler emits a
//! function table entry (`.pdata`/`.xdata`) for the routine with a language
//! specific handler, [`copy_fault_handler`]. When a load faults, Windows looks
//! up the function that contains the faulting instruction, finds that entry,
//! and calls the handler. The handler resumes execution in a recovery routine
//! that returns a failure code and the faulting address to the caller.
//!
//! This is chosen over a process wide vectored exception handler on purpose:
//!
//! * Nothing is registered with the process. There is no install step, no
//!   global state, nothing to remove, and no handler left dangling in unmapped
//!   code if the DLL is unloaded. The function table belongs to the module's
//!   image and goes away with it.
//! * The handler is only ever called for a fault whose instruction lies in the
//!   copy routine. A fault anywhere else in the game never reaches it, so it
//!   cannot swallow another component's exception.
//!
//! Vectored handlers registered by the game run before structured handlers.
//! One that treats every access violation as fatal would see the fault before
//! we do. [`crate::sys::fastpath`] therefore validates the range first and
//! reaches this code only for ranges that were readable a moment ago, so a
//! fault here needs a page to be freed or re-protected within that moment.
//!
//! The handler claims `EXCEPTION_ACCESS_VIOLATION`, `EXCEPTION_IN_PAGE_ERROR`
//! and `EXCEPTION_GUARD_PAGE` only, during the dispatch phase only, and only
//! when the faulting instruction is inside the routine. It allocates nothing,
//! takes no lock and cannot panic.

use std::arch::naked_asm;
use std::ffi::c_void;
use std::ptr;
use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};

use windows_sys::Win32::Foundation::{
    EXCEPTION_ACCESS_VIOLATION, EXCEPTION_GUARD_PAGE, EXCEPTION_IN_PAGE_ERROR,
};
use windows_sys::Win32::System::Diagnostics::Debug::{
    CONTEXT, DISPATCHER_CONTEXT, EXCEPTION_RECORD, RtlLookupFunctionEntry,
};
use windows_sys::Win32::System::SystemServices::{EXCEPTION_EXIT_UNWIND, EXCEPTION_UNWINDING};

/// `ExceptionContinueExecution`: resume at the (modified) context.
const DISPOSITION_CONTINUE_EXECUTION: i32 = 0;
/// `ExceptionContinueSearch`: this handler does not claim the exception.
const DISPOSITION_CONTINUE_SEARCH: i32 = 1;

/// Why a guarded copy failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct CopyFault {
    /// The address the processor reported, when the exception carried one.
    pub(crate) address: u64,
    /// The exception code: access violation, in-page error or guard page.
    pub(crate) code: u32,
}

impl CopyFault {
    /// Whether the fault was a guard page violation. The page's guard flag is
    /// already cleared by the time this is seen.
    pub(crate) fn is_guard_page(&self) -> bool {
        self.code == EXCEPTION_GUARD_PAGE as u32
    }
}

/// The record the recovery routine fills in. `repr(C)` because assembly writes it.
#[repr(C)]
struct FaultRecord {
    /// Faulting address, from `ExceptionInformation[1]`.
    address: u64,
    /// Exception code.
    code: u64,
}

/// Number of faults the guarded copy recovered from, for tests and diagnosis.
static RECOVERED_FAULTS: AtomicU64 = AtomicU64::new(0);

/// How many faults the guarded copy has turned into errors since the process
/// started. A healthy process sees few or none: ranges are validated first, so
/// a fault needs a page to disappear between the check and the copy.
pub(crate) fn recovered_faults() -> u64 {
    RECOVERED_FAULTS.load(Ordering::Relaxed)
}

/// Whether the system knows this module's function table, so that the
/// handler of [`guarded_copy_routine`] will be called.
///
/// A module that the loader mapped has its exception directory registered.
/// One that was mapped by hand, which an injector may do, usually has not: a
/// fault in the routine would then find no handler and end the process. The
/// answer is looked up once. When it is `false` the guarded copy must not be
/// used and reads go through the operating system's copy call instead.
pub(crate) fn unwind_registered() -> bool {
    match REGISTRATION.load(Ordering::Relaxed) {
        REGISTERED => true,
        UNREGISTERED => false,
        _ => {
            let registered =
                has_exception_handler(guarded_copy_routine as *const () as usize as u64);
            REGISTRATION.store(
                if registered { REGISTERED } else { UNREGISTERED },
                Ordering::Relaxed,
            );
            registered
        }
    }
}

/// `REGISTRATION` before the first lookup.
const UNCHECKED: u8 = 0;
/// The routine has a function table entry with an exception handler.
const REGISTERED: u8 = 1;
/// It has not.
const UNREGISTERED: u8 = 2;

static REGISTRATION: AtomicU8 = AtomicU8::new(UNCHECKED);

/// `UNW_FLAG_EHANDLER`: the unwind info names an exception handler.
const UNW_FLAG_EHANDLER: u8 = 0x1;
/// `UNW_FLAG_CHAININFO`: the entry only chains to another one.
const UNW_FLAG_CHAININFO: u8 = 0x4;

/// Asks the system for the function table entry that starts at `routine` and
/// checks that it carries an exception handler. `false` when no table covers
/// the address, as for memory that was mapped by hand or generated at run time.
fn has_exception_handler(routine: u64) -> bool {
    let mut image_base = 0u64;
    // SAFETY: looks up a table entry for an address of this process; the out
    // pointer is valid and a null history table is allowed.
    let entry = unsafe { RtlLookupFunctionEntry(routine, &mut image_base, ptr::null_mut()) };
    // SAFETY: a non-null result points at a valid entry of the system's table.
    let Some(entry) = (unsafe { entry.as_ref() }) else {
        return false;
    };
    if image_base.wrapping_add(u64::from(entry.BeginAddress)) != routine {
        return false;
    }
    // SAFETY: the entry belongs to a registered table, so `UnwindData` is the
    // RVA of its unwind info, whose first byte holds the version and flags.
    let unwind_rva = unsafe { entry.Anonymous.UnwindData };
    let info = image_base.wrapping_add(u64::from(unwind_rva)) as usize as *const u8;
    // SAFETY: the unwind info is part of the registered image and is at least
    // four bytes long; one byte is read.
    let version_and_flags = unsafe { ptr::read(info) };
    let flags = version_and_flags >> 3;
    flags & UNW_FLAG_EHANDLER != 0 && flags & UNW_FLAG_CHAININFO == 0
}

/// Copies `dst.len()` bytes from `src` into `dst`.
///
/// Returns the fault instead of crashing when any byte of the source cannot be
/// read. On failure `dst` holds an unspecified mix of old and new bytes.
/// `src` is only an address value; it may be anything.
pub(crate) fn guarded_copy(src: usize, dst: &mut [u8]) -> Result<(), CopyFault> {
    let mut record = FaultRecord {
        address: 0,
        code: 0,
    };
    // SAFETY: `dst` is a valid writable buffer of `dst.len()` bytes and
    // `record` is a valid writable `FaultRecord`; those are the only addresses
    // the routine writes to. `src` is read, and a fault while reading it is
    // handled by `copy_fault_handler`, which resumes in `copy_fault_recovery`;
    // the routine then returns 1. The routine keeps the stack pointer where the
    // caller left it, so it can return from any point.
    let status = unsafe { guarded_copy_routine(dst.as_mut_ptr(), src, dst.len(), &mut record) };
    if status == 0 {
        return Ok(());
    }
    RECOVERED_FAULTS.fetch_add(1, Ordering::Relaxed);
    Err(CopyFault {
        address: record.address,
        code: record.code as u32,
    })
}

/// The copy. Microsoft x64 calling convention: `rcx` destination, `rdx`
/// source, `r8` length, `r9` pointer to a [`FaultRecord`]; returns 0 in `eax`,
/// or 1 after a fault.
///
/// Register discipline, which the recovery routine depends on: `r9` is never
/// written, nothing is pushed, so `rsp` always points at the return address.
/// Only volatile registers are used. Every source access is a load whose
/// address is derived from `rdx`; the stores go to the destination only.
///
/// Sizes are handled with overlapping loads: 16 to 32 bytes with two 16-byte
/// blocks, more with a 32-byte loop and an overlapping 32-byte tail, 8 to 15
/// with two 8-byte moves, and so on down to a byte. No access goes outside
/// `[src, src + len)`.
///
/// The `.seh_*` directives make the assembler emit the function table entry
/// and unwind info with an exception handler; `sym` supplies its address.
#[unsafe(naked)]
unsafe extern "C" fn guarded_copy_routine(
    dst: *mut u8,
    src: usize,
    len: usize,
    fault: *mut FaultRecord,
) -> u32 {
    naked_asm!(
        ".seh_proc {routine}",
        ".seh_handler {handler}, @except",
        ".seh_endprologue",
        "cmp r8, 16",
        "jb 3f",
        "cmp r8, 32",
        "ja 2f",
        // 16..=32 bytes: two overlapping 16-byte blocks.
        "movdqu xmm0, xmmword ptr [rdx]",
        "movdqu xmm1, xmmword ptr [rdx + r8 - 16]",
        "movdqu xmmword ptr [rcx], xmm0",
        "movdqu xmmword ptr [rcx + r8 - 16], xmm1",
        "xor eax, eax",
        "ret",
        // More than 32 bytes: 32 per round, then an overlapping 32-byte tail.
        "2:",
        "lea r10, [rdx + r8 - 32]",
        "lea r11, [rcx + r8 - 32]",
        "4:",
        "movdqu xmm0, xmmword ptr [rdx]",
        "movdqu xmm1, xmmword ptr [rdx + 16]",
        "movdqu xmmword ptr [rcx], xmm0",
        "movdqu xmmword ptr [rcx + 16], xmm1",
        "add rdx, 32",
        "add rcx, 32",
        "cmp rdx, r10",
        "jb 4b",
        "movdqu xmm0, xmmword ptr [r10]",
        "movdqu xmm1, xmmword ptr [r10 + 16]",
        "movdqu xmmword ptr [r11], xmm0",
        "movdqu xmmword ptr [r11 + 16], xmm1",
        "xor eax, eax",
        "ret",
        // Fewer than 16 bytes.
        "3:",
        "cmp r8, 8",
        "jb 5f",
        "mov rax, qword ptr [rdx]",
        "mov r10, qword ptr [rdx + r8 - 8]",
        "mov qword ptr [rcx], rax",
        "mov qword ptr [rcx + r8 - 8], r10",
        "xor eax, eax",
        "ret",
        "5:",
        "cmp r8, 4",
        "jb 6f",
        "mov eax, dword ptr [rdx]",
        "mov r10d, dword ptr [rdx + r8 - 4]",
        "mov dword ptr [rcx], eax",
        "mov dword ptr [rcx + r8 - 4], r10d",
        "xor eax, eax",
        "ret",
        "6:",
        "test r8, r8",
        "jz 7f",
        // 1..=3 bytes: first, middle and last byte cover every length.
        "mov r11, r8",
        "shr r11, 1",
        "movzx eax, byte ptr [rdx]",
        "mov byte ptr [rcx], al",
        "movzx eax, byte ptr [rdx + r11]",
        "mov byte ptr [rcx + r11], al",
        "movzx eax, byte ptr [rdx + r8 - 1]",
        "mov byte ptr [rcx + r8 - 1], al",
        "7:",
        "xor eax, eax",
        "ret",
        ".seh_endproc",
        routine = sym guarded_copy_routine,
        handler = sym copy_fault_handler,
    )
}

/// Where the handler resumes. It runs on the stack of the faulting copy, which
/// has only a return address on top, so its `ret` returns to the caller of
/// [`guarded_copy_routine`]. The handler left the faulting address in `rdx`
/// and the exception code in `r8`; `r9` is still the caller's [`FaultRecord`].
#[unsafe(naked)]
unsafe extern "C" fn copy_fault_recovery() {
    naked_asm!(
        "mov qword ptr [r9], rdx",
        "mov qword ptr [r9 + 8], r8",
        "mov eax, 1",
        "ret",
    )
}

/// The language specific handler of [`guarded_copy_routine`].
///
/// Called by the system during exception dispatch for a fault whose
/// instruction is in the routine, after any vectored handlers. Claims access
/// violations, in-page errors and guard page violations; everything else, and
/// the unwind phase, continues the search.
///
/// # Safety
///
/// Only the operating system calls this, with a valid exception record,
/// context and dispatcher context.
unsafe extern "C" fn copy_fault_handler(
    record: *mut EXCEPTION_RECORD,
    _establisher_frame: *mut c_void,
    context: *mut CONTEXT,
    dispatcher: *mut DISPATCHER_CONTEXT,
) -> i32 {
    // SAFETY: the system passes valid pointers; the references live only here.
    let (Some(record), Some(context), Some(dispatcher)) = (
        unsafe { record.as_mut() },
        unsafe { context.as_mut() },
        unsafe { dispatcher.as_ref() },
    ) else {
        return DISPOSITION_CONTINUE_SEARCH;
    };
    if record.ExceptionFlags & (EXCEPTION_UNWINDING | EXCEPTION_EXIT_UNWIND) != 0 {
        return DISPOSITION_CONTINUE_SEARCH;
    }
    let code = record.ExceptionCode;
    if code != EXCEPTION_ACCESS_VIOLATION
        && code != EXCEPTION_IN_PAGE_ERROR
        && code != EXCEPTION_GUARD_PAGE
    {
        return DISPOSITION_CONTINUE_SEARCH;
    }
    // The instruction that faulted must be inside the copy routine and not in
    // this handler's own function table entry for anything else. The routine is
    // the only function that registers this handler, so this holds by
    // construction; it is checked anyway because claiming a fault that is not
    // ours would hide a real crash.
    let routine = guarded_copy_routine as *const () as usize as u64;
    // SAFETY: `FunctionEntry` is the system's entry for the faulting function.
    let in_routine = unsafe { dispatcher.FunctionEntry.as_ref() }.is_some_and(|entry| {
        dispatcher
            .ImageBase
            .wrapping_add(u64::from(entry.BeginAddress))
            == routine
    });
    if !in_routine {
        return DISPOSITION_CONTINUE_SEARCH;
    }
    // `ExceptionInformation[1]` is the address for these three exceptions.
    let address = if record.NumberParameters >= 2 {
        record.ExceptionInformation.get(1).copied().unwrap_or(0) as u64
    } else {
        0
    };
    context.Rdx = address;
    context.R8 = u64::from(code as u32);
    context.Rip = copy_fault_recovery as *const () as usize as u64;
    DISPOSITION_CONTINUE_EXECUTION
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::mem::size_of;

    #[test]
    fn copies_every_length_exactly() {
        let source: Vec<u8> = (0..=255u8).cycle().take(300).collect();
        for len in 0..=source.len() {
            let mut out = vec![0xEEu8; len + 2];
            // SAFETY-free: the source slice is valid for `len` bytes.
            let slice = &mut out[1..=len];
            guarded_copy(source.as_ptr() as usize, slice).unwrap();
            assert_eq!(&out[1..=len], &source[..len], "len {len}");
            assert_eq!((out[0], out[len + 1]), (0xEE, 0xEE), "overrun at len {len}");
        }
    }

    #[test]
    fn the_handler_is_registered_with_the_loader() {
        assert!(unwind_registered());
    }

    #[test]
    fn code_without_a_registered_table_is_reported() {
        // A function that exists in an executable page the loader knows nothing
        // about has no table entry, which is what a hand-mapped module looks like.
        use windows_sys::Win32::System::Memory::{
            MEM_COMMIT, MEM_RELEASE, MEM_RESERVE, PAGE_EXECUTE_READWRITE, VirtualAlloc, VirtualFree,
        };
        // SAFETY: allocates, writes one `ret` byte into, and releases a private page.
        unsafe {
            let page = VirtualAlloc(
                ptr::null(),
                4096,
                MEM_RESERVE | MEM_COMMIT,
                PAGE_EXECUTE_READWRITE,
            );
            assert!(!page.is_null());
            page.cast::<u8>().write(0xC3);
            assert!(!has_exception_handler(page as usize as u64));
            VirtualFree(page, 0, MEM_RELEASE);
        }
        // An ordinary function of this module has an entry, but no handler.
        assert!(!has_exception_handler(
            code_without_a_registered_table_is_reported as *const () as usize as u64
        ));
        assert!(has_exception_handler(
            guarded_copy_routine as *const () as usize as u64
        ));
    }

    #[test]
    fn a_guard_page_is_reported_and_its_flag_is_consumed() {
        // This is why the fast path validates first: the copy itself cannot
        // read a guard page without clearing its flag.
        use windows_sys::Win32::System::Memory::{
            MEM_COMMIT, MEM_RELEASE, MEM_RESERVE, MEMORY_BASIC_INFORMATION, PAGE_GUARD,
            PAGE_READWRITE, VirtualAlloc, VirtualFree, VirtualProtect, VirtualQuery,
        };
        // SAFETY: allocates, protects, queries and releases a private page.
        unsafe {
            let page = VirtualAlloc(ptr::null(), 4096, MEM_RESERVE | MEM_COMMIT, PAGE_READWRITE);
            assert!(!page.is_null());
            let mut old = 0;
            assert_ne!(
                VirtualProtect(page, 4096, PAGE_READWRITE | PAGE_GUARD, &mut old),
                0
            );
            let mut out = [0u8; 8];
            let fault = guarded_copy(page as usize + 16, &mut out).unwrap_err();
            assert!(fault.is_guard_page());
            assert_eq!(fault.address, page as usize as u64 + 16);
            let mut info = MEMORY_BASIC_INFORMATION::default();
            VirtualQuery(page, &mut info, size_of::<MEMORY_BASIC_INFORMATION>());
            assert_eq!(info.Protect & PAGE_GUARD, 0, "the system clears the flag");
            // Now an ordinary page: the second read succeeds.
            guarded_copy(page as usize + 16, &mut out).unwrap();
            VirtualFree(page, 0, MEM_RELEASE);
        }
    }

    #[test]
    fn a_fault_is_an_error_with_the_address() {
        for len in [1usize, 3, 8, 15, 16, 32, 33, 4096] {
            let mut out = vec![0u8; len];
            let fault = guarded_copy(0x10, &mut out).unwrap_err();
            assert_eq!(fault.address, 0x10, "len {len}");
            assert_eq!(fault.code, EXCEPTION_ACCESS_VIOLATION as u32);
        }
        assert!(recovered_faults() >= 8);
    }
}
