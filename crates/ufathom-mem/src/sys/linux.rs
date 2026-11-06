//! Linux backend: `process_vm_readv` and `/proc`.
//!
//! `process_vm_readv` reports `EFAULT` for unmapped or unreadable memory and
//! never raises a signal in the caller, so target memory is never dereferenced
//! directly, not even for the current process.

use std::ffi::c_void;
use std::fs;

use crate::error::{MemError, range_end};
use crate::maps::{modules_from_maps, parse_maps, regions_from_maps};
use crate::reader::Addr;
use crate::types::{MemoryRegion, ModuleInfo, ProcessInfo};

/// Largest single `process_vm_readv` request. The kernel clamps one vector to
/// just under 2 GiB, so bigger reads are split.
const MAX_CHUNK: usize = 1 << 30;

/// Upper bound on processes returned by [`list_processes`].
const MAX_PROCESSES: usize = 1 << 20;

/// A process whose memory can be read.
pub(crate) struct Target {
    pid: u32,
}

impl Target {
    /// The current process.
    pub(crate) fn current() -> Self {
        Self {
            pid: std::process::id(),
        }
    }

    /// Opens `pid`. Linux has no handle to open, so this checks that the
    /// process exists and that its memory map can be read.
    pub(crate) fn open(pid: u32) -> Result<Self, MemError> {
        let target = Self { pid };
        match fs::metadata(target.proc_path("maps")) {
            Ok(_) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                return Err(MemError::ProcessGone { pid });
            }
            Err(err) if err.kind() == std::io::ErrorKind::PermissionDenied => {
                return Err(MemError::AccessDenied {
                    context: format!("/proc/{pid}"),
                });
            }
            Err(err) => return Err(MemError::Io(err)),
        }
        // A process that exists but is not allowed to be read reports it here.
        target.read_maps()?;
        Ok(target)
    }

    pub(crate) fn pid(&self) -> u32 {
        self.pid
    }

    fn proc_path(&self, leaf: &str) -> String {
        format!("/proc/{}/{leaf}", self.pid)
    }

    pub(crate) fn read(&self, addr: Addr, buf: &mut [u8]) -> Result<(), MemError> {
        range_end(addr, buf.len())?;
        let mut done = 0usize;
        while done < buf.len() {
            let want = (buf.len() - done).min(MAX_CHUNK);
            let at = addr + done as u64;
            let dst = buf
                .get_mut(done..done + want)
                .ok_or_else(|| MemError::unreadable(at, want))?;
            self.read_chunk(at, dst)?;
            done += want;
        }
        Ok(())
    }

    fn read_chunk(&self, addr: Addr, buf: &mut [u8]) -> Result<(), MemError> {
        let local = libc::iovec {
            iov_base: buf.as_mut_ptr().cast::<c_void>(),
            iov_len: buf.len(),
        };
        let remote = libc::iovec {
            iov_base: addr as usize as *mut c_void,
            iov_len: buf.len(),
        };
        loop {
            // SAFETY: `local` points at a valid writable buffer of `iov_len` bytes
            // that outlives the call. `remote` is only an address value for the
            // kernel, which validates it and fails with EFAULT when unreadable;
            // this process never dereferences it. Both iovec arrays have length 1.
            let copied = unsafe {
                libc::process_vm_readv(self.pid as libc::pid_t, &local, 1, &remote, 1, 0)
            };
            if copied >= 0 {
                return if copied as usize == buf.len() {
                    Ok(())
                } else {
                    Err(MemError::unreadable(addr, buf.len()))
                };
            }
            let errno = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
            return Err(match errno {
                libc::EINTR => continue,
                libc::EFAULT => MemError::unreadable(addr, buf.len()),
                libc::EPERM => MemError::AccessDenied {
                    context: format!("process_vm_readv of process {}", self.pid),
                },
                libc::ESRCH => MemError::ProcessGone { pid: self.pid },
                code => MemError::Os {
                    operation: "process_vm_readv",
                    code,
                },
            });
        }
    }

    fn read_maps(&self) -> Result<Vec<crate::maps::MapsEntry>, MemError> {
        match fs::read_to_string(self.proc_path("maps")) {
            Ok(text) => Ok(parse_maps(&text)),
            Err(err) => Err(match err.kind() {
                std::io::ErrorKind::NotFound => MemError::ProcessGone { pid: self.pid },
                std::io::ErrorKind::PermissionDenied => MemError::AccessDenied {
                    context: format!("/proc/{}/maps", self.pid),
                },
                _ => MemError::Io(err),
            }),
        }
    }

    pub(crate) fn regions(&self) -> Result<Vec<MemoryRegion>, MemError> {
        Ok(regions_from_maps(&self.read_maps()?))
    }

    pub(crate) fn modules(&self) -> Result<Vec<ModuleInfo>, MemError> {
        Ok(modules_from_maps(&self.read_maps()?))
    }
}

/// Lists processes from `/proc`. A process that vanishes or hides its
/// executable is still listed when its name is known, with `path: None`.
pub(crate) fn list_processes() -> Result<Vec<ProcessInfo>, MemError> {
    let mut processes = Vec::new();
    for entry in fs::read_dir("/proc")? {
        if processes.len() >= MAX_PROCESSES {
            break;
        }
        let Ok(entry) = entry else { continue };
        let file_name = entry.file_name();
        let Some(pid) = file_name.to_str().and_then(|name| name.parse::<u32>().ok()) else {
            continue;
        };
        let Ok(comm) = fs::read_to_string(format!("/proc/{pid}/comm")) else {
            continue;
        };
        let path = fs::read_link(format!("/proc/{pid}/exe"))
            .ok()
            .map(|path| path.to_string_lossy().into_owned());
        processes.push(ProcessInfo {
            pid,
            name: comm.trim_end().to_owned(),
            path,
        });
    }
    Ok(processes)
}
