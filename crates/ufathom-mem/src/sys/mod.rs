//! Operating system backends. Each exposes the same `Target` type and
//! `list_processes` function, so the public readers contain no `cfg`.

#[cfg(all(windows, target_arch = "x86_64"))]
mod fastpath;
#[cfg(all(windows, target_arch = "x86_64"))]
mod guarded;
#[cfg(target_os = "linux")]
mod linux;
#[cfg(not(any(windows, target_os = "linux")))]
mod unsupported;
#[cfg(windows)]
mod windows;

#[cfg(target_os = "linux")]
pub(crate) use linux::{Target, list_processes};
#[cfg(not(any(windows, target_os = "linux")))]
pub(crate) use unsupported::{Target, list_processes};
#[cfg(windows)]
pub use windows::PROCESS_OPEN_ACCESS;
#[cfg(windows)]
pub(crate) use windows::{Target, list_processes};

/// Faults the guarded direct copy has recovered from since the process
/// started. Always 0 where there is no direct copy.
pub(crate) fn recovered_faults() -> u64 {
    #[cfg(all(windows, target_arch = "x86_64"))]
    {
        guarded::recovered_faults()
    }
    #[cfg(not(all(windows, target_arch = "x86_64")))]
    {
        0
    }
}
