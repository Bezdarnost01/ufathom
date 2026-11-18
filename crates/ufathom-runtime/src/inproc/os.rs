//! Operating system thread identities.
//!
//! The hook compares threads by the identifier the operating system gives
//! them, not by Rust's `ThreadId`, so a thread can be named from outside Rust
//! (for example the engine's game thread id) and the main thread can be found.

/// The operating system identifier of the calling thread. Never 0.
#[cfg(windows)]
pub(crate) fn current_thread_id() -> u64 {
    // SAFETY: no preconditions.
    u64::from(unsafe { windows_sys::Win32::System::Threading::GetCurrentThreadId() })
}

/// The operating system identifier of the calling thread. Never 0.
#[cfg(target_os = "linux")]
pub(crate) fn current_thread_id() -> u64 {
    // SAFETY: no preconditions.
    let tid = unsafe { libc::gettid() };
    u64::try_from(tid).unwrap_or(u64::MAX)
}

/// The operating system identifier of the calling thread. Never 0.
#[cfg(all(unix, not(target_os = "linux")))]
pub(crate) fn current_thread_id() -> u64 {
    // SAFETY: no preconditions.
    let id = unsafe { libc::pthread_self() } as u64;
    id.max(1)
}

/// The process's main thread: the first one it started, which is the game
/// thread of a standard Unreal Engine build. `None` when it cannot be told.
#[cfg(windows)]
pub(crate) fn main_thread_id() -> Option<u64> {
    use windows_sys::Win32::Foundation::{CloseHandle, FILETIME, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First, Thread32Next,
    };
    use windows_sys::Win32::System::Threading::{
        GetCurrentProcessId, GetThreadTimes, OpenThread, THREAD_QUERY_LIMITED_INFORMATION,
    };

    /// Threads looked at, at most: a bound on a list another process could grow.
    const MAX_THREADS: usize = 1 << 16;

    // SAFETY: no preconditions.
    let pid = unsafe { GetCurrentProcessId() };
    // SAFETY: a thread snapshot of all processes; the handle is closed below.
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) };
    if snapshot == INVALID_HANDLE_VALUE || snapshot.is_null() {
        return None;
    }
    // SAFETY: `THREADENTRY32` is plain data; all zeroes is a valid value.
    let mut entry: THREADENTRY32 = unsafe { core::mem::zeroed() };
    entry.dwSize = u32::try_from(size_of::<THREADENTRY32>()).unwrap_or(0);
    let mut best: Option<(u64, u64)> = None;
    // SAFETY: `snapshot` is open and `entry.dwSize` is set.
    let mut more = unsafe { Thread32First(snapshot, &mut entry) } != 0;
    let mut seen = 0usize;
    while more && seen < MAX_THREADS {
        seen += 1;
        if entry.th32OwnerProcessID == pid {
            // SAFETY: opening a thread of this process for a query only.
            let thread =
                unsafe { OpenThread(THREAD_QUERY_LIMITED_INFORMATION, 0, entry.th32ThreadID) };
            if !thread.is_null() {
                let zero = FILETIME {
                    dwLowDateTime: 0,
                    dwHighDateTime: 0,
                };
                let (mut created, mut exited, mut kernel, mut user) = (zero, zero, zero, zero);
                // SAFETY: `thread` is open with query access; the out
                // pointers are valid for the call.
                let ok = unsafe {
                    GetThreadTimes(thread, &mut created, &mut exited, &mut kernel, &mut user)
                } != 0;
                // SAFETY: closing the handle opened above.
                unsafe { CloseHandle(thread) };
                if ok {
                    let time = (u64::from(created.dwHighDateTime) << 32)
                        | u64::from(created.dwLowDateTime);
                    if best.is_none_or(|(earliest, _)| time < earliest) {
                        best = Some((time, u64::from(entry.th32ThreadID)));
                    }
                }
            }
        }
        // SAFETY: as for `Thread32First`.
        more = unsafe { Thread32Next(snapshot, &mut entry) } != 0;
    }
    // SAFETY: closing the snapshot opened above.
    unsafe { CloseHandle(snapshot) };
    best.map(|(_, id)| id)
}

/// The process's main thread, whose thread id equals the process id.
#[cfg(target_os = "linux")]
pub(crate) fn main_thread_id() -> Option<u64> {
    // SAFETY: no preconditions.
    u64::try_from(unsafe { libc::getpid() }).ok()
}

/// Not known on this platform.
#[cfg(all(unix, not(target_os = "linux")))]
pub(crate) fn main_thread_id() -> Option<u64> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thread_ids_are_stable_and_distinct() {
        let me = current_thread_id();
        assert_ne!(me, 0);
        assert_eq!(me, current_thread_id());
        let other = std::thread::spawn(current_thread_id).join().unwrap_or(me);
        assert_ne!(other, me);
    }

    #[test]
    fn the_main_thread_is_not_a_fresh_thread() {
        let main = main_thread_id();
        if cfg!(any(windows, target_os = "linux")) {
            assert!(main.is_some());
        }
        let fresh = std::thread::spawn(current_thread_id).join().unwrap_or(0);
        assert_ne!(main, Some(fresh));
    }
}
