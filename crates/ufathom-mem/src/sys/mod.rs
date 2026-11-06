//! Operating system backends. Each exposes the same `Target` type and
//! `list_processes` function, so the public readers contain no `cfg`.

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
