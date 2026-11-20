//! Suspending the other threads of the process and moving their instruction
//! pointers.
//!
//! Suspending a thread that holds the heap lock, and then allocating, hangs
//! the process. So the work is split: [`ThreadSet::prepare`] allocates and
//! opens every handle first, [`ThreadSet::suspend`] and the visits after it
//! allocate nothing, and the handles are closed after the threads run again.

use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::System::Diagnostics::Debug::{
    CONTEXT, CONTEXT_CONTROL_AMD64, GetThreadContext, SetThreadContext,
};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First, Thread32Next,
};
use windows_sys::Win32::System::Threading::{
    GetCurrentProcessId, GetCurrentThreadId, OpenThread, ResumeThread, SuspendThread,
    THREAD_GET_CONTEXT, THREAD_SET_CONTEXT, THREAD_SUSPEND_RESUME,
};

use super::DetourError;
use super::memory::os_error;

/// Threads looked at, at most: a bound on a list another process could grow.
const MAX_THREADS: usize = 1 << 16;

/// What to do with one suspended thread, decided from its instruction
/// pointer.
pub(super) enum Visit {
    /// Leave it.
    Keep,
    /// Continue it at this address instead.
    MoveTo(u64),
    /// Stop the visit and report failure.
    Abort,
}

/// Why a visit stopped.
#[derive(Clone, Copy, Debug)]
pub(super) enum VisitFailure {
    /// The callback asked to stop.
    Aborted,
    /// `SetThreadContext` failed with this error.
    Os(u32),
}

struct Thread {
    handle: HANDLE,
    suspended: bool,
}

/// The other threads of the process, with open handles.
///
/// Threads that have been suspended resume when the set is dropped.
pub(super) struct ThreadSet {
    threads: Vec<Thread>,
}

impl ThreadSet {
    /// Opens a handle to every other thread of the process. Allocates.
    ///
    /// # Errors
    ///
    /// [`DetourError::Os`] when the thread list cannot be read.
    pub(super) fn prepare() -> Result<Self, DetourError> {
        // SAFETY: no preconditions.
        let pid = unsafe { GetCurrentProcessId() };
        // SAFETY: no preconditions.
        let me = unsafe { GetCurrentThreadId() };
        // SAFETY: a thread snapshot of all processes; closed below.
        let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) };
        if snapshot == INVALID_HANDLE_VALUE || snapshot.is_null() {
            return Err(os_error("CreateToolhelp32Snapshot"));
        }
        // SAFETY: `THREADENTRY32` is plain data; all zeroes is valid.
        let mut entry: THREADENTRY32 = unsafe { core::mem::zeroed() };
        entry.dwSize = u32::try_from(size_of::<THREADENTRY32>()).unwrap_or(0);
        let mut threads = Vec::new();
        // SAFETY: `snapshot` is open and `entry.dwSize` is set.
        let mut more = unsafe { Thread32First(snapshot, &mut entry) } != 0;
        let mut seen = 0usize;
        while more && seen < MAX_THREADS {
            seen += 1;
            if entry.th32OwnerProcessID == pid && entry.th32ThreadID != me {
                // SAFETY: opening a thread of this process for suspension
                // and context access.
                let handle = unsafe {
                    OpenThread(
                        THREAD_SUSPEND_RESUME | THREAD_GET_CONTEXT | THREAD_SET_CONTEXT,
                        0,
                        entry.th32ThreadID,
                    )
                };
                // A thread that cannot be opened (it exited, or the system
                // refuses) is left alone.
                if !handle.is_null() {
                    threads.push(Thread {
                        handle,
                        suspended: false,
                    });
                }
            }
            // SAFETY: as for `Thread32First`.
            more = unsafe { Thread32Next(snapshot, &mut entry) } != 0;
        }
        // SAFETY: closing the snapshot opened above.
        unsafe { CloseHandle(snapshot) };
        Ok(Self { threads })
    }

    /// Suspends every thread of the set. Allocates nothing.
    pub(super) fn suspend(&mut self) {
        for thread in &mut self.threads {
            // SAFETY: the handle is open with suspend access.
            let previous = unsafe { SuspendThread(thread.handle) };
            thread.suspended = previous != u32::MAX;
        }
    }

    /// Shows the instruction pointer of every suspended thread to `visit`
    /// and applies the answer. Allocates nothing.
    ///
    /// A thread whose context cannot be read is skipped: it has most likely
    /// exited.
    pub(super) fn visit(&self, mut visit: impl FnMut(u64) -> Visit) -> Result<(), VisitFailure> {
        for thread in self.threads.iter().filter(|thread| thread.suspended) {
            // SAFETY: `CONTEXT` is plain data; all zeroes is valid.
            let mut context: CONTEXT = unsafe { core::mem::zeroed() };
            context.ContextFlags = CONTEXT_CONTROL_AMD64;
            // SAFETY: the handle is open with context access; the context
            // is aligned and writable.
            if unsafe { GetThreadContext(thread.handle, &mut context) } == 0 {
                continue;
            }
            match visit(context.Rip) {
                Visit::Keep => {}
                Visit::MoveTo(rip) => {
                    context.Rip = rip;
                    // SAFETY: as above; only control registers are set.
                    if unsafe { SetThreadContext(thread.handle, &context) } == 0 {
                        // SAFETY: no preconditions.
                        return Err(VisitFailure::Os(unsafe { GetLastError() }));
                    }
                }
                Visit::Abort => return Err(VisitFailure::Aborted),
            }
        }
        Ok(())
    }
}

impl Drop for ThreadSet {
    fn drop(&mut self) {
        // Resume everything before the vector is freed: freeing takes the
        // heap lock a suspended thread could hold.
        for thread in &self.threads {
            if thread.suspended {
                // SAFETY: the handle is open and the thread was suspended
                // by `suspend`.
                unsafe { ResumeThread(thread.handle) };
            }
            // SAFETY: closing the handle opened in `prepare`.
            unsafe { CloseHandle(thread.handle) };
        }
    }
}
