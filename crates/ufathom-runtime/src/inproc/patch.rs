//! Replacing one pointer in a virtual table.
//!
//! Virtual tables live in read-only pages. A slot is made writable for the
//! write alone, swapped atomically (an aligned 8-byte store, so a thread
//! reading the slot sees the old or the new pointer, never half of each) and
//! the old protection is restored.

use core::sync::atomic::{AtomicUsize, Ordering};

/// What [`exchange_slot`] found in the slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Exchange {
    /// The slot held `expected` and now holds `new`.
    Swapped,
    /// The slot held this other value and was not changed.
    Held(usize),
}

/// Writes `new` to the slot at `slot` if it still holds `expected`, and says
/// what it held otherwise.
///
/// # Safety
///
/// As [`swap_slot`].
pub(crate) unsafe fn exchange_slot(
    slot: usize,
    expected: usize,
    new: usize,
) -> Result<Exchange, String> {
    if slot == 0 || !slot.is_multiple_of(align_of::<usize>()) {
        return Err(format!("slot {slot:#x} is not a pointer-aligned address"));
    }
    // SAFETY: the caller promises a mapped slot; `writable` restores the
    // protection when `guard` drops.
    let guard = unsafe { writable(slot)? };
    // SAFETY: the address is aligned, mapped and now writable; other threads
    // only read it, through atomic-width loads.
    let atom = unsafe { AtomicUsize::from_ptr(slot as *mut usize) };
    let result = atom.compare_exchange(expected, new, Ordering::AcqRel, Ordering::Acquire);
    drop(guard);
    Ok(match result {
        Ok(_) => Exchange::Swapped,
        Err(held) => Exchange::Held(held),
    })
}

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
    // SAFETY: the contract is the caller's, passed on unchanged.
    unsafe { exchange_slot(slot, expected, new) }.map(|found| found == Exchange::Swapped)
}

/// How a slot this module patched fared when it was put back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Restored {
    /// The slot holds the original again, no other code reaches the hook
    /// through it.
    Done,
    /// The slot holds something other than the hook or the original:
    /// another module patched it after this one and forwards to the hook.
    /// The slot is untouched and the hook must stay mapped and forwarding.
    Chained {
        /// What the slot holds.
        holder: usize,
    },
}

/// Puts `original` back into the slot at `slot`, which holds `hook` when
/// nothing else patched it since.
///
/// Several modules may each carry a copy of this runtime and hook the same
/// slot. The one that hooked second saved the first one's hook as its
/// original. If the first module restored `original` over the second one's
/// hook, the second one's callers would be dropped; if it only forgot the
/// slot, its own code could be unmapped while the second one still forwards
/// into it. So a slot that no longer holds `hook` is reported, not touched.
///
/// # Safety
///
/// As [`swap_slot`], with `original` what the slot held before `hook`.
pub(crate) unsafe fn restore_slot(
    slot: usize,
    hook: usize,
    original: usize,
) -> Result<Restored, String> {
    // SAFETY: the contract is the caller's, passed on unchanged.
    match unsafe { exchange_slot(slot, hook, original) }? {
        Exchange::Swapped => Ok(Restored::Done),
        // Someone already restored it.
        Exchange::Held(held) if held == original => Ok(Restored::Done),
        Exchange::Held(holder) => Ok(Restored::Chained { holder }),
    }
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

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::indexing_slicing,
        reason = "tests may panic"
    )]

    use super::*;

    const ENGINE: usize = 0x1000;
    const HOOK_A: usize = 0x2000;
    const HOOK_B: usize = 0x3000;

    /// `swap_slot` on a slot of a table the test owns.
    fn swap(slot: usize, expected: usize, new: usize) -> bool {
        // SAFETY: every slot passed is an aligned slot of a live table this
        // test owns; the pointers written are never called.
        unsafe { swap_slot(slot, expected, new) }.unwrap()
    }

    /// `restore_slot` on a slot of a table the test owns.
    fn restore(slot: usize, hook: usize, original: usize) -> Restored {
        // SAFETY: as for `swap`.
        unsafe { restore_slot(slot, hook, original) }.unwrap()
    }

    fn read(table: &[usize], index: usize) -> usize {
        // SAFETY: `index` is inside the live slice; the slot is aligned.
        unsafe { AtomicUsize::from_ptr(table.as_ptr().add(index).cast_mut()) }
            .load(Ordering::Acquire)
    }

    /// Two modules that each carry this code hook the same slot: `B` saves
    /// `A`'s hook as its original. `A` must not be able to restore before `B`.
    #[test]
    fn a_slot_hooked_by_two_modules_is_restored_in_order() {
        let table = vec![0usize, ENGINE, 0];
        let slot = table.as_ptr() as usize + size_of::<usize>();
        assert!(swap(slot, ENGINE, HOOK_A));
        // B read A's hook out of the slot as "the original".
        let b_original = read(&table, 1);
        assert_eq!(b_original, HOOK_A);
        assert!(swap(slot, b_original, HOOK_B));

        // A goes first: the slot holds B's hook, so it stays and is reported.
        assert_eq!(
            restore(slot, HOOK_A, ENGINE),
            Restored::Chained { holder: HOOK_B }
        );
        assert_eq!(read(&table, 1), HOOK_B);
        // Retrying changes nothing.
        assert_eq!(
            restore(slot, HOOK_A, ENGINE),
            Restored::Chained { holder: HOOK_B }
        );
        assert_eq!(read(&table, 1), HOOK_B);

        // B restores A's hook, then A can restore the engine's function.
        assert_eq!(restore(slot, HOOK_B, b_original), Restored::Done);
        assert_eq!(read(&table, 1), HOOK_A);
        assert_eq!(restore(slot, HOOK_A, ENGINE), Restored::Done);
        assert_eq!(read(&table, 1), ENGINE);
        // The neighbours were never touched.
        assert_eq!((read(&table, 0), read(&table, 2)), (0, 0));
    }

    #[test]
    fn restoring_a_slot_that_holds_the_original_is_done() {
        let table = vec![ENGINE];
        let slot = table.as_ptr() as usize;
        assert_eq!(restore(slot, HOOK_A, ENGINE), Restored::Done);
        assert_eq!(read(&table, 0), ENGINE);
    }

    fn restore_slot_checked(slot: usize) -> bool {
        // SAFETY: a null or misaligned slot is refused before any access.
        unsafe { restore_slot(slot, HOOK_A, ENGINE) }.is_err()
    }

    #[test]
    fn a_misaligned_or_null_slot_is_an_error() {
        assert!(restore_slot_checked(0));
        assert!(restore_slot_checked(0x1001));
    }
}
