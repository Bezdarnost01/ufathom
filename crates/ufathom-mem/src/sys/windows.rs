//! Windows backend: `ReadProcessMemory`, `VirtualQueryEx`, module and process lists.
//!
//! Another process is never dereferenced. Every byte comes from
//! `ReadProcessMemory`, which reports an error for unmapped, `PAGE_NOACCESS`
//! and `PAGE_GUARD` pages instead of raising an exception. The current process
//! can additionally read through the fault-guarded copy of `fastpath` (x86-64
//! only), which is validated first and recovers from a fault; any read it does
//! not complete falls back to `ReadProcessMemory`. The one write,
//! `Target::write`, goes through `WriteProcessMemory` and refuses anything but
//! committed read-write memory.

use std::ffi::c_void;
use std::mem::size_of;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::ptr;

use windows_sys::Win32::Foundation::{
    ERROR_ACCESS_DENIED, ERROR_INVALID_HANDLE, ERROR_INVALID_PARAMETER, ERROR_NOACCESS,
    ERROR_PARTIAL_COPY, GetLastError, HANDLE, HMODULE, INVALID_HANDLE_VALUE, STILL_ACTIVE,
};
use windows_sys::Win32::System::Diagnostics::Debug::{ReadProcessMemory, WriteProcessMemory};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
};
use windows_sys::Win32::System::Memory::{MEM_COMMIT, MEMORY_BASIC_INFORMATION, VirtualQueryEx};
use windows_sys::Win32::System::ProcessStatus::{
    K32EnumProcessModulesEx, K32GetModuleFileNameExW, K32GetModuleInformation, LIST_MODULES_ALL,
    MODULEINFO,
};
use windows_sys::Win32::System::SystemInformation::{GetSystemInfo, SYSTEM_INFO};
use windows_sys::Win32::System::Threading::{
    GetCurrentProcess, GetExitCodeProcess, OpenProcess, PROCESS_QUERY_INFORMATION,
    PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_VM_READ, QueryFullProcessImageNameW,
};

#[cfg(target_arch = "x86_64")]
use super::fastpath;
use crate::error::{MemError, range_end};
use crate::protect::{is_plain_read_write, kind_from_type, perms_from_protect};
use crate::reader::Addr;
use crate::types::{MemoryRegion, ModuleInfo, ProcessInfo, file_name};

/// Access rights requested by [`Target::open`]: query and read, nothing else.
///
/// There is deliberately no write, debug, thread or terminate right.
pub const PROCESS_OPEN_ACCESS: u32 = PROCESS_QUERY_INFORMATION | PROCESS_VM_READ;

/// Upper bound on `VirtualQueryEx` calls in one region walk.
const MAX_REGION_QUERIES: usize = 1 << 22;

/// Upper bound on regions one write may span. A write is a field or a few
/// bytes; one that crosses more regions than this is a bug in the caller.
const MAX_WRITE_REGIONS: usize = 64;

/// Upper bound on modules listed for one process.
const MAX_MODULES: usize = 1 << 16;

/// Retries when the module list changes between sizing and filling.
const MODULE_LIST_RETRIES: usize = 8;

/// Longest path accepted from the OS, in UTF-16 units.
const MAX_PATH_UNITS: usize = 32_768;

/// Upper bound on processes returned by [`list_processes`].
const MAX_PROCESSES: usize = 1 << 20;

/// A process whose memory can be read: either this one or a opened by pid.
pub(crate) struct Target {
    pid: u32,
    /// `None` means the current process, addressed by its pseudo handle.
    owned: Option<OwnedHandle>,
    /// Whether reads may use the fault-guarded direct copy. Only ever true for
    /// the current process.
    #[cfg_attr(
        not(target_arch = "x86_64"),
        allow(dead_code, reason = "the guarded copy exists for x86-64 only")
    )]
    direct: bool,
}

impl Target {
    /// The current process, read with the fault-guarded direct copy where the
    /// platform has one.
    pub(crate) fn current() -> Self {
        Self {
            pid: std::process::id(),
            owned: None,
            direct: true,
        }
    }

    /// The current process, read only through `ReadProcessMemory`.
    pub(crate) fn current_os_copy() -> Self {
        Self {
            direct: false,
            ..Self::current()
        }
    }

    /// Opens `pid` with [`PROCESS_OPEN_ACCESS`].
    pub(crate) fn open(pid: u32) -> Result<Self, MemError> {
        // SAFETY: `OpenProcess` takes plain values and returns a new handle or null.
        let raw = unsafe { OpenProcess(PROCESS_OPEN_ACCESS, 0, pid) };
        if raw.is_null() {
            // SAFETY: reads the calling thread's last-error value.
            let code = unsafe { GetLastError() };
            return Err(match code {
                ERROR_ACCESS_DENIED => MemError::AccessDenied {
                    context: format!("OpenProcess({pid})"),
                },
                ERROR_INVALID_PARAMETER => MemError::ProcessGone { pid },
                other => MemError::Os {
                    operation: "OpenProcess",
                    code: other as i32,
                },
            });
        }
        // SAFETY: `raw` is a valid handle we own and nothing else will close.
        let owned = unsafe { OwnedHandle::from_raw_handle(raw) };
        let target = Self {
            pid,
            owned: Some(owned),
            direct: false,
        };
        if target.has_exited() {
            return Err(MemError::ProcessGone { pid });
        }
        Ok(target)
    }

    pub(crate) fn pid(&self) -> u32 {
        self.pid
    }

    fn handle(&self) -> HANDLE {
        match &self.owned {
            Some(owned) => owned.as_raw_handle(),
            // SAFETY: returns the constant pseudo handle for the current process.
            None => unsafe { GetCurrentProcess() },
        }
    }

    /// Whether the process has terminated. Always `false` for the current process.
    fn has_exited(&self) -> bool {
        if self.owned.is_none() {
            return false;
        }
        let mut code = 0u32;
        // SAFETY: the handle is valid for the lifetime of `self` and `code` is a valid out pointer.
        let ok = unsafe { GetExitCodeProcess(self.handle(), &mut code) };
        ok != 0 && code != STILL_ACTIVE as u32
    }

    /// Turns a failed OS call into the most specific [`MemError`].
    fn classify(&self, operation: &'static str, code: u32, addr: Addr, len: usize) -> MemError {
        if self.has_exited() {
            return MemError::ProcessGone { pid: self.pid };
        }
        match code {
            ERROR_PARTIAL_COPY | ERROR_NOACCESS | ERROR_INVALID_PARAMETER => {
                MemError::unreadable(addr, len)
            }
            ERROR_ACCESS_DENIED => MemError::AccessDenied {
                context: format!("{operation} of {len:#x} bytes at {addr:#x}"),
            },
            ERROR_INVALID_HANDLE => MemError::ProcessGone { pid: self.pid },
            other => MemError::Os {
                operation,
                code: other as i32,
            },
        }
    }

    pub(crate) fn read(&self, addr: Addr, buf: &mut [u8]) -> Result<(), MemError> {
        range_end(addr, buf.len())?;
        if buf.is_empty() {
            return Ok(());
        }
        #[cfg(target_arch = "x86_64")]
        if self.direct && matches!(fastpath::try_read(addr, buf), fastpath::Outcome::Done) {
            return Ok(());
        }
        let Ok(remote) = usize::try_from(addr) else {
            return Err(MemError::unreadable(addr, buf.len()));
        };
        let mut copied = 0usize;
        // SAFETY: `buf` is a valid writable buffer of `buf.len()` bytes. `remote`
        // is only an address value for the kernel, which validates it and returns
        // failure for anything unreadable; this process never dereferences it.
        let ok = unsafe {
            ReadProcessMemory(
                self.handle(),
                remote as *const c_void,
                buf.as_mut_ptr().cast::<c_void>(),
                buf.len(),
                &mut copied,
            )
        };
        if ok != 0 && copied == buf.len() {
            return Ok(());
        }
        let code = if ok != 0 {
            ERROR_PARTIAL_COPY
        } else {
            // SAFETY: reads the calling thread's last-error value.
            unsafe { GetLastError() }
        };
        Err(self.classify("ReadProcessMemory", code, addr, buf.len()))
    }

    /// Writes `bytes` at `addr` with `WriteProcessMemory`, after checking with
    /// `VirtualQueryEx` that the whole range is committed, plain read-write
    /// memory. Nothing is written otherwise.
    ///
    /// The check matters: `WriteProcessMemory` on a read-only page may lift
    /// the protection for the copy. This never changes a page protection, so
    /// it refuses such a page instead. Only the current process is ever
    /// written (see `SelfMemory::write_exact`); the handle of a process opened
    /// by `Target::open` has no write right and the OS refuses.
    pub(crate) fn write(&self, addr: Addr, bytes: &[u8]) -> Result<(), MemError> {
        let end = range_end(addr, bytes.len())?;
        if bytes.is_empty() {
            return Ok(());
        }
        let Ok(remote) = usize::try_from(addr) else {
            return Err(MemError::not_writable(addr, bytes.len()));
        };
        self.require_plain_read_write(addr, end, bytes.len())?;
        let mut copied = 0usize;
        // SAFETY: `bytes` is a valid readable buffer of `bytes.len()` bytes.
        // `remote` is only an address value for the kernel, which validates it
        // and returns failure for anything it cannot write; this process never
        // dereferences it. The range was just checked to be read-write.
        let ok = unsafe {
            WriteProcessMemory(
                self.handle(),
                remote as *const c_void,
                bytes.as_ptr().cast::<c_void>(),
                bytes.len(),
                &mut copied,
            )
        };
        if ok != 0 && copied == bytes.len() {
            return Ok(());
        }
        let code = if ok != 0 {
            ERROR_PARTIAL_COPY
        } else {
            // SAFETY: reads the calling thread's last-error value.
            unsafe { GetLastError() }
        };
        Err(
            match self.classify("WriteProcessMemory", code, addr, bytes.len()) {
                MemError::Unreadable { addr, len } => MemError::NotWritable { addr, len },
                other => other,
            },
        )
    }

    /// Checks that every byte of `addr..end` is in committed, plain
    /// read-write regions, walking the regions with `VirtualQueryEx`.
    fn require_plain_read_write(&self, addr: Addr, end: u64, len: usize) -> Result<(), MemError> {
        let refuse = || MemError::not_writable(addr, len);
        let mut cursor = addr;
        for _ in 0..MAX_WRITE_REGIONS {
            let at = usize::try_from(cursor).map_err(|_| refuse())?;
            let mut info = MEMORY_BASIC_INFORMATION::default();
            // SAFETY: `info` is a valid out buffer of the size passed, and
            // `at` is only an address value for the kernel to look up.
            let written = unsafe {
                VirtualQueryEx(
                    self.handle(),
                    at as *const c_void,
                    &mut info,
                    size_of::<MEMORY_BASIC_INFORMATION>(),
                )
            };
            if written == 0 || info.State != MEM_COMMIT || !is_plain_read_write(info.Protect) {
                return Err(refuse());
            }
            let region_end = (info.BaseAddress as u64)
                .checked_add(info.RegionSize as u64)
                .ok_or_else(refuse)?;
            if region_end >= end {
                return Ok(());
            }
            if region_end <= cursor {
                return Err(refuse());
            }
            cursor = region_end;
        }
        Err(MemError::LimitExceeded {
            what: "regions spanned by one write",
            requested: MAX_WRITE_REGIONS as u64 + 1,
            limit: MAX_WRITE_REGIONS as u64,
        })
    }

    /// Walks the address space with `VirtualQueryEx` and returns committed regions.
    pub(crate) fn regions(&self) -> Result<Vec<MemoryRegion>, MemError> {
        let max_address = max_application_address();
        let mut regions = Vec::new();
        let mut cursor: usize = 0;
        for query in 0..MAX_REGION_QUERIES {
            let mut info = MEMORY_BASIC_INFORMATION::default();
            // SAFETY: `info` is a valid out buffer of the size passed, and
            // `cursor` is only an address value for the kernel to look up.
            let written = unsafe {
                VirtualQueryEx(
                    self.handle(),
                    cursor as *const c_void,
                    &mut info,
                    size_of::<MEMORY_BASIC_INFORMATION>(),
                )
            };
            if written == 0 {
                if query == 0 {
                    // SAFETY: reads the calling thread's last-error value.
                    let code = unsafe { GetLastError() };
                    return Err(self.classify("VirtualQueryEx", code, 0, 0));
                }
                // Past the last valid address the call fails by design.
                break;
            }
            let base = info.BaseAddress as usize;
            let size = info.RegionSize;
            if info.State == MEM_COMMIT {
                regions.push(MemoryRegion {
                    base: base as u64,
                    size: size as u64,
                    perms: perms_from_protect(info.Protect),
                    kind: kind_from_type(info.Type),
                });
            }
            let Some(next) = base.checked_add(size) else {
                break;
            };
            if next <= cursor || next > max_address {
                break;
            }
            cursor = next;
        }
        Ok(regions)
    }

    /// Lists loaded modules with `K32EnumProcessModulesEx`.
    pub(crate) fn modules(&self) -> Result<Vec<ModuleInfo>, MemError> {
        let handles = self.module_handles()?;
        let mut modules = Vec::with_capacity(handles.len());
        for module in handles {
            let mut info = MODULEINFO {
                lpBaseOfDll: ptr::null_mut(),
                SizeOfImage: 0,
                EntryPoint: ptr::null_mut(),
            };
            // SAFETY: `info` is a valid out buffer of the size passed.
            let ok = unsafe {
                K32GetModuleInformation(
                    self.handle(),
                    module,
                    &mut info,
                    size_of::<MODULEINFO>() as u32,
                )
            };
            if ok == 0 {
                // The module unloaded since it was listed.
                continue;
            }
            let path = self.module_path(module);
            let name = path
                .as_deref()
                .map_or_else(|| String::from("<unknown>"), |p| file_name(p).to_owned());
            modules.push(ModuleInfo {
                name,
                path,
                base: info.lpBaseOfDll as usize as u64,
                size: u64::from(info.SizeOfImage),
            });
        }
        modules.sort_by_key(|module| module.base);
        Ok(modules)
    }

    fn module_handles(&self) -> Result<Vec<HMODULE>, MemError> {
        let mut capacity = 1024usize;
        for _ in 0..MODULE_LIST_RETRIES {
            let mut handles: Vec<HMODULE> = vec![ptr::null_mut(); capacity];
            let bytes = capacity.saturating_mul(size_of::<HMODULE>());
            let mut needed = 0u32;
            // SAFETY: `handles` has room for `bytes` bytes and `needed` is a valid out pointer.
            let ok = unsafe {
                K32EnumProcessModulesEx(
                    self.handle(),
                    handles.as_mut_ptr(),
                    u32::try_from(bytes).unwrap_or(u32::MAX),
                    &mut needed,
                    LIST_MODULES_ALL,
                )
            };
            if ok == 0 {
                // SAFETY: reads the calling thread's last-error value.
                let code = unsafe { GetLastError() };
                return Err(self.classify("K32EnumProcessModulesEx", code, 0, 0));
            }
            let count = needed as usize / size_of::<HMODULE>();
            if count <= capacity {
                handles.truncate(count);
                return Ok(handles);
            }
            if count > MAX_MODULES {
                return Err(MemError::LimitExceeded {
                    what: "module count",
                    requested: count as u64,
                    limit: MAX_MODULES as u64,
                });
            }
            capacity = count + 16;
        }
        Err(MemError::Os {
            operation: "K32EnumProcessModulesEx",
            code: 0,
        })
    }

    fn module_path(&self, module: HMODULE) -> Option<String> {
        let mut units = 260usize;
        while units <= MAX_PATH_UNITS {
            let mut buf = vec![0u16; units];
            // SAFETY: `buf` is a valid writable buffer of `units` UTF-16 units.
            let written = unsafe {
                K32GetModuleFileNameExW(self.handle(), module, buf.as_mut_ptr(), units as u32)
            } as usize;
            if written == 0 {
                return None;
            }
            if written < units {
                return Some(String::from_utf16_lossy(buf.get(..written)?));
            }
            units *= 2;
        }
        None
    }
}

/// Highest user-mode address, from `GetSystemInfo`.
fn max_application_address() -> usize {
    let mut info = SYSTEM_INFO::default();
    // SAFETY: `info` is a valid out buffer.
    unsafe { GetSystemInfo(&mut info) };
    info.lpMaximumApplicationAddress as usize
}

/// Lists processes with Toolhelp. Image paths come from
/// `QueryFullProcessImageNameW` on a handle with only
/// `PROCESS_QUERY_LIMITED_INFORMATION`; if that is refused the path is `None`.
pub(crate) fn list_processes() -> Result<Vec<ProcessInfo>, MemError> {
    // SAFETY: takes plain values and returns a new handle or INVALID_HANDLE_VALUE.
    let raw = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
    if raw == INVALID_HANDLE_VALUE || raw.is_null() {
        // SAFETY: reads the calling thread's last-error value.
        let code = unsafe { GetLastError() };
        return Err(MemError::Os {
            operation: "CreateToolhelp32Snapshot",
            code: code as i32,
        });
    }
    // SAFETY: `raw` is a valid handle we own and nothing else will close.
    let snapshot = unsafe { OwnedHandle::from_raw_handle(raw) };
    let mut entry = PROCESSENTRY32W {
        dwSize: size_of::<PROCESSENTRY32W>() as u32,
        ..PROCESSENTRY32W::default()
    };
    let mut processes = Vec::new();
    // SAFETY: the snapshot handle is valid and `entry.dwSize` is set as required.
    let mut more = unsafe { Process32FirstW(snapshot.as_raw_handle(), &mut entry) } != 0;
    while more && processes.len() < MAX_PROCESSES {
        let name_len = entry
            .szExeFile
            .iter()
            .position(|unit| *unit == 0)
            .unwrap_or(entry.szExeFile.len());
        let name = String::from_utf16_lossy(entry.szExeFile.get(..name_len).unwrap_or_default());
        processes.push(ProcessInfo {
            pid: entry.th32ProcessID,
            name,
            path: image_path(entry.th32ProcessID),
        });
        // SAFETY: as for `Process32FirstW`.
        more = unsafe { Process32NextW(snapshot.as_raw_handle(), &mut entry) } != 0;
    }
    Ok(processes)
}

/// Full image path of `pid`, or `None` if it cannot be learned.
fn image_path(pid: u32) -> Option<String> {
    if pid == 0 {
        return None;
    }
    // SAFETY: `OpenProcess` takes plain values and returns a new handle or null.
    let raw = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if raw.is_null() {
        return None;
    }
    // SAFETY: `raw` is a valid handle we own and nothing else will close.
    let process = unsafe { OwnedHandle::from_raw_handle(raw) };
    let mut units = 1024usize;
    while units <= MAX_PATH_UNITS {
        let mut buf = vec![0u16; units];
        let mut len = units as u32;
        // SAFETY: `buf` holds `len` writable UTF-16 units and `len` is a valid in/out pointer.
        let ok = unsafe {
            QueryFullProcessImageNameW(process.as_raw_handle(), 0, buf.as_mut_ptr(), &mut len)
        };
        if ok != 0 {
            return Some(String::from_utf16_lossy(buf.get(..len as usize)?));
        }
        units *= 2;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_access_is_exactly_query_and_read() {
        assert_eq!(PROCESS_OPEN_ACCESS, 0x0400 | 0x0010);
    }
}
