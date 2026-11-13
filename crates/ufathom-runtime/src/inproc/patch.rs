//! Replacing one pointer in a virtual table.
//!
//! Virtual tables live in read-only pages. A slot is made writable for the
//! write alone, swapped atomically (an aligned 8-byte store, so a thread
//! reading the slot sees the old or the new pointer, never half of each) and
//! the old protection is restored.

use core::sync::atomic::{AtomicUsize, Ordering};

/// Writes `new` to the slot at `slot` if it still holds `expected`.
///
/// Returns `Ok(true)` when the slot was changed and `Ok(false)` when it held
/// something else.
///
/// # Safety
///
/// `slot` is a pointer-aligned, mapped address of a virtual table slot in this
/// process, and every reader of the slot can call `new` in place of `expected`.
pub(crate) unsafe fn swap_slot(slot: usize, expected: usize, new: usize) -> Result<bool, String> {
    if slot == 0 || slot % align_of::<usize>() != 0 {
        return Err(format!("slot {slot:#x} is not a pointer-aligned address"));
    }
    // SAFETY: the caller promises a mapped slot; `writable` restores the
    // protection when `guard` drops.
    let guard = unsafe { writable(slot)? };
    // SAFETY: the address is aligned, mapped and now writable; other threads
    // only read it, through atomic-width loads.
    let atom = unsafe { AtomicUsize::from_ptr(slot as *mut usize) };
    let changed = atom
        .compare_exchange(expected, new, Ordering::AcqRel, Ordering::Acquire)
        .is_ok();
    drop(guard);
    Ok(changed)
}

/// Restores a page protection on drop.
struct Restore {
    #[cfg(windows)]
    old: u32,
    #[cfg(unix)]
    old: libc::c_int,
    page: usize,
    len: usize,
}

#[cfg(windows)]
unsafe fn writable(slot: usize) -> Result<Restore, String> {
    use windows_sys::Win32::Foundation::GetLastError;
    use windows_sys::Win32::System::Memory::{PAGE_READWRITE, VirtualProtect};

    let mut old = 0u32;
    let len = size_of::<usize>();
    // SAFETY: the caller promises the slot is mapped in this process.
    let ok = unsafe { VirtualProtect(slot as *const _, len, PAGE_READWRITE, &mut old) };
    if ok == 0 {
        // SAFETY: no preconditions.
        let error = unsafe { GetLastError() };
        return Err(format!("VirtualProtect failed with error {error}"));
    }
    Ok(Restore {
        old,
        page: slot,
        len,
    })
}

#[cfg(windows)]
impl Drop for Restore {
    fn drop(&mut self) {
        let mut ignored = 0u32;
        // SAFETY: restores the protection read when the guard was made.
        unsafe {
            windows_sys::Win32::System::Memory::VirtualProtect(
                self.page as *const _,
                self.len,
                self.old,
                &mut ignored,
            );
        }
    }
}

#[cfg(unix)]
unsafe fn writable(slot: usize) -> Result<Restore, String> {
    use ufathom_mem::{MemoryReader, SelfMemory};

    // SAFETY: no preconditions.
    let page_size = usize::try_from(unsafe { libc::sysconf(libc::_SC_PAGESIZE) })
        .ok()
        .filter(|&size| size.is_power_of_two())
        .ok_or("cannot read the page size")?;
    let page = slot & !(page_size - 1);
    let regions = SelfMemory::new()
        .regions()
        .map_err(|error| format!("cannot read the memory map: {error}"))?;
    let region = regions
        .iter()
        .find(|region| region.contains(slot as u64))
        .ok_or_else(|| format!("slot {slot:#x} is not mapped"))?;
    let mut old = libc::PROT_NONE;
    if region.perms.read {
        old |= libc::PROT_READ;
    }
    if region.perms.write {
        old |= libc::PROT_WRITE;
    }
    if region.perms.execute {
        old |= libc::PROT_EXEC;
    }
    // SAFETY: `page` is the page holding the mapped slot.
    let result = unsafe {
        libc::mprotect(
            page as *mut libc::c_void,
            page_size,
            old | libc::PROT_READ | libc::PROT_WRITE,
        )
    };
    if result != 0 {
        return Err(format!(
            "mprotect failed: {}",
            std::io::Error::last_os_error()
        ));
    }
    Ok(Restore {
        old,
        page,
        len: page_size,
    })
}

#[cfg(unix)]
impl Drop for Restore {
    fn drop(&mut self) {
        // SAFETY: restores the protection read when the guard was made.
        unsafe {
            libc::mprotect(self.page as *mut libc::c_void, self.len, self.old);
        }
    }
}

#[cfg(not(any(windows, unix)))]
unsafe fn writable(_slot: usize) -> Result<Restore, String> {
    Err("virtual table patching is not supported on this platform".to_owned())
}
