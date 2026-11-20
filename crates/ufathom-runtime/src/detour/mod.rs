//! Inline detours of plain native functions, for mods.
//!
//! Many game functions are not reflected: they have no `UFunction`, and
//! neither `ProcessEvent` nor `UFunction::Func` ever sees a call. A mod that
//! wants to intercept one patches its first bytes. [`Detour`] does that, and
//! refuses to patch unless the bytes found at the address are the bytes the
//! author expects, so a game update turns into an error, not a crash.
//!
//! This is the only module of the runtime that patches code. It is opt-in
//! (the `detour` feature), meant for mods, and nothing in the dump path uses
//! it. It is available on Windows x86-64 only.
//!
//! # Finding the function
//!
//! [`find_in_module`] finds a function by [`Signature`], which must match
//! exactly once. [`module_base`] plus an RVA finds it by address. Either way,
//! [`Detour::install`] is given the bytes that must be there.
//!
//! # How a detour works
//!
//! 1. The bytes at the target are compared with the signature, which must
//!    cover every byte that will be overwritten.
//! 2. Whole instructions covering at least 5 bytes are decoded (with
//!    `iced-x86`). The function is refused when they contain a `ret`, an
//!    `int3` or other trap, a jump that leaves more stolen bytes behind it, or
//!    a branch into the stolen bytes.
//! 3. A block of memory within 2 GB of the target gets the stolen
//!    instructions, re-encoded so RIP-relative operands and relative branches
//!    still work (the trampoline), and a relay that jumps to the replacement.
//!    The block is written read-write, then switched to read-execute; no page
//!    is ever writable and executable at once.
//! 4. With every other thread of the process suspended, the target gets a
//!    5-byte `jmp rel32` to the relay, padded with `int3`. A thread stopped
//!    inside the overwritten bytes is moved to the same place in the
//!    trampoline.
//!
//! [`Detour::original`] is the trampoline: calling it runs the function as it
//! was.
//!
//! # Threads and the heap
//!
//! Installing and removing a detour suspends every other thread. A suspended
//! thread may hold the heap lock, so nothing allocates while they are
//! suspended: everything is allocated first. A thread created while a patch
//! is being written is not suspended; it could run the half-written bytes.
//! The window is a few instructions wide.
//!
//! # Unloading
//!
//! A replacement can be running when the module that holds it unloads, which
//! crashes the game. Define replacements with [`detour_fn!`](crate::detour_fn):
//! it counts the threads inside the body. [`Detour::uninstall_and_wait`]
//! restores the bytes, waits until no thread is inside a replacement, in the
//! relay or in the trampoline, and only then frees the memory. The generated
//! DLL's `hot-unload` calls [`uninstall_all_and_wait`] for every detour that
//! is still installed.
//!
//! # Limits
//!
//! - Dropping a [`Detour`] does nothing: the patch stays. Call
//!   [`uninstall`](Detour::uninstall) or [`uninstall_and_wait`](Detour::uninstall_and_wait).
//! - Code elsewhere that jumps into the middle of the overwritten bytes
//!   cannot be detected and will fail.
//! - The trampoline has no unwind information: a C++ exception thrown
//!   through [`Detour::original`] cannot be unwound.
//! - Another tool that patches the same function breaks the guarantees here;
//!   an uninstall that finds foreign bytes refuses and keeps everything.
//! - Executable memory is not required to belong to a module, so code in
//!   memory a packer allocated can be hooked, if the bytes match.

mod analyze;
mod error;
mod image;
mod inflight;
mod memory;
mod signature;
mod threads;

use core::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use ufathom_mem::{MemoryReader, SelfMemory};

pub use error::DetourError;
pub use image::{find_in_module, module_base};
pub use inflight::{InFlightGuard, in_flight};
pub use signature::Signature;

use analyze::{JUMP_LEN, WINDOW_LEN};
use memory::{BLOCK_SIZE, NearBlock, WriteFailure};
use threads::{ThreadSet, Visit, VisitFailure};

/// Size of an absolute jump: `jmp [rip + 0]` followed by the 8-byte address.
const ABSOLUTE_JUMP_LEN: usize = 14;

/// `int3`, which pads the bytes after the 5-byte jump.
const INT3: u8 = 0xCC;

/// How long [`Detour::uninstall_and_wait`] keeps waiting after it first finds
/// nothing inside, for a thread that jumped into the replacement just before
/// the bytes were restored and has not yet counted itself.
const UNLOAD_GRACE: Duration = Duration::from_millis(50);

/// How often the wait looks again.
const WAIT_POLL: Duration = Duration::from_millis(5);

/// Bytes at the start of a replacement that count as "not yet counted": a
/// thread there is between the relay and the guard of
/// [`detour_fn!`](crate::detour_fn).
const REPLACEMENT_ENTRY: usize = 128;

/// Every detour installed through this library and not yet removed. The lock
/// also serialises installs and removals, so two threads never suspend each
/// other.
static INSTALLED: Mutex<Vec<Arc<Inner>>> = Mutex::new(Vec::new());

fn lock_installed() -> MutexGuard<'static, Vec<Arc<Inner>>> {
    INSTALLED.lock().unwrap_or_else(PoisonError::into_inner)
}

/// What a detour owns.
struct Inner {
    /// The detoured function.
    target: usize,
    /// The bytes the patch replaced.
    original: Vec<u8>,
    /// The bytes the patch wrote.
    patch: Vec<u8>,
    /// The allocation holding the trampoline and the relay.
    block: usize,
    /// Start of the trampoline, which is the start of the block.
    trampoline: usize,
    /// Length of the moved instructions in the trampoline.
    moved_len: usize,
    /// Start of the relay.
    relay: usize,
    /// The replacement function.
    replacement: usize,
    /// Pairs of (offset from `target`, offset from `trampoline`) for every
    /// moved instruction and for the end of the stolen bytes.
    map: Vec<(u32, u32)>,
}

impl Inner {
    /// Where a thread stopped at `rip` in the trampoline or relay can
    /// continue in the original code once it is restored, if it can.
    fn way_back(&self, rip: usize) -> Option<usize> {
        let stolen_end = self.target + self.original.len();
        if (self.relay..self.relay + ABSOLUTE_JUMP_LEN).contains(&rip) {
            // The relay only jumps; the thread has not started the call.
            return Some(self.target);
        }
        let offset = rip.checked_sub(self.trampoline)?;
        if offset >= self.moved_len {
            return (offset < self.moved_len + ABSOLUTE_JUMP_LEN).then_some(stolen_end);
        }
        self.map
            .iter()
            .find(|&&(_, moved)| moved as usize == offset)
            .map(|&(original, _)| self.target + original as usize)
    }

    /// Whether a thread at `rip` is in code that is about to be freed or
    /// that is the not-yet-counted start of the replacement.
    fn holds(&self, rip: usize) -> bool {
        (self.block..self.block + BLOCK_SIZE).contains(&rip)
            || (self.replacement..self.replacement + REPLACEMENT_ENTRY).contains(&rip)
    }

    fn overlaps(&self, address: usize, len: usize) -> bool {
        self.target < address.saturating_add(len)
            && address < self.target.saturating_add(self.original.len())
    }
}

/// An installed inline detour.
///
/// See the [module documentation](self) for what it does and its limits.
/// Dropping the value leaves the patch in place; remove it with
/// [`uninstall`](Self::uninstall) or [`uninstall_and_wait`](Self::uninstall_and_wait).
pub struct Detour {
    inner: Arc<Inner>,
}

impl core::fmt::Debug for Detour {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Detour")
            .field("target", &format_args!("{:#x}", self.inner.target))
            .field(
                "replacement",
                &format_args!("{:#x}", self.inner.replacement),
            )
            .field("trampoline", &format_args!("{:#x}", self.inner.trampoline))
            .field("stolen_len", &self.inner.original.len())
            .finish()
    }
}

/// What went wrong between suspending the threads and resuming them. Small
/// and `Copy`, so nothing is built while the threads are stopped.
#[derive(Clone, Copy)]
enum Failure {
    Aborted,
    Changed,
    Os(u32),
    Length,
}

impl Failure {
    fn into_error(self, target: usize) -> DetourError {
        match self {
            Self::Aborted => DetourError::ThreadInPatch { address: target },
            Self::Changed => DetourError::Modified { address: target },
            Self::Os(code) => DetourError::Os {
                call: "VirtualProtect or SetThreadContext",
                code,
            },
            Self::Length => DetourError::Unrelocatable {
                address: target,
                reason: "internal: bad patch length".to_owned(),
            },
        }
    }
}

impl From<WriteFailure> for Failure {
    fn from(failure: WriteFailure) -> Self {
        match failure {
            WriteFailure::Changed => Self::Changed,
            WriteFailure::Os(code) => Self::Os(code),
            WriteFailure::Length => Self::Length,
        }
    }
}

impl From<VisitFailure> for Failure {
    fn from(failure: VisitFailure) -> Self {
        match failure {
            VisitFailure::Aborted => Self::Aborted,
            VisitFailure::Os(code) => Self::Os(code),
        }
    }
}

/// `E9 rel32`: a jump from `from` to `to`, if 32 bits reach.
fn relative_jump(from: usize, to: usize) -> Option<[u8; JUMP_LEN]> {
    let next = i64::try_from(from.checked_add(JUMP_LEN)?).ok()?;
    let displacement = i32::try_from(i64::try_from(to).ok()?.checked_sub(next)?).ok()?;
    let [a, b, c, d] = displacement.to_le_bytes();
    Some([0xE9, a, b, c, d])
}

/// `jmp [rip + 0]` followed by the absolute destination.
fn absolute_jump(destination: usize) -> [u8; ABSOLUTE_JUMP_LEN] {
    let [a, b, c, d, e, f, g, h] = (destination as u64).to_le_bytes();
    [0xFF, 0x25, 0, 0, 0, 0, a, b, c, d, e, f, g, h]
}

impl Detour {
    /// Detours the function at `target` to `replacement`.
    ///
    /// `expected` must match the bytes at `target` and cover every byte the
    /// patch overwrites: at least 5, and whole instructions. Nothing is
    /// changed unless that holds.
    ///
    /// The replacement can run on another thread before this function
    /// returns. If it needs [`original`](Self::original), use
    /// [`install_with_original`](Self::install_with_original), which stores
    /// the trampoline address before the patch is written.
    ///
    /// # Errors
    ///
    /// - [`DetourError::NotExecutable`] and [`DetourError::BadReplacement`]
    ///   when an address is not in executable memory.
    /// - [`DetourError::SignatureMismatch`] and
    ///   [`DetourError::SignatureTooShort`] when the bytes are not the
    ///   expected ones.
    /// - [`DetourError::Unrelocatable`] when the start of the function cannot
    ///   be moved: see the [module documentation](self).
    /// - [`DetourError::NoNearMemory`] when no memory within 2 GB of the
    ///   target could be allocated.
    /// - [`DetourError::AlreadyHooked`] when this library already patched
    ///   those bytes.
    /// - [`DetourError::ThreadInPatch`], [`DetourError::Modified`] and
    ///   [`DetourError::Os`] when the patch itself failed; the function is
    ///   left unpatched, though the memory of a failed install may be kept.
    ///
    /// # Safety
    ///
    /// - `target` is the start of a function in this process, and the bytes
    ///   at the start are code, not data.
    /// - `replacement` is an `extern "system"` function with the signature of
    ///   the target (same arguments, same return type, same calling
    ///   convention), which stays mapped while the detour is installed and
    ///   until [`uninstall_and_wait`](Self::uninstall_and_wait) or
    ///   [`uninstall_all_and_wait`] reports it idle.
    /// - No other tool is patching the same bytes at the same time.
    pub unsafe fn install(
        target: usize,
        expected: &Signature,
        replacement: *const (),
    ) -> Result<Self, DetourError> {
        // SAFETY: the contract is the caller's.
        unsafe { Self::install_inner(target, expected, replacement, None) }
    }

    /// Like [`install`](Self::install), and stores the address of the
    /// trampoline in `original` before the patch is written, so the
    /// replacement never sees an empty cell.
    ///
    /// The cell holds 0 again if the install fails.
    ///
    /// # Errors
    ///
    /// As [`install`](Self::install).
    ///
    /// # Safety
    ///
    /// As [`install`](Self::install).
    pub unsafe fn install_with_original(
        target: usize,
        expected: &Signature,
        replacement: *const (),
        original: &'static AtomicUsize,
    ) -> Result<Self, DetourError> {
        // SAFETY: the contract is the caller's.
        unsafe { Self::install_inner(target, expected, replacement, Some(original)) }
    }

    unsafe fn install_inner(
        target: usize,
        expected: &Signature,
        replacement: *const (),
        original_cell: Option<&'static AtomicUsize>,
    ) -> Result<Self, DetourError> {
        let replacement = replacement as usize;
        if replacement == 0 || memory::executable_span(replacement, 1) == 0 {
            return Err(DetourError::BadReplacement {
                address: replacement,
            });
        }
        let not_executable = |reason| DetourError::NotExecutable {
            address: target,
            reason,
        };
        let span = memory::executable_span(target, WINDOW_LEN);
        if span == 0 {
            return Err(not_executable("the page is not committed and executable"));
        }
        if expected.len() > span {
            return Err(not_executable(
                "the signature runs past the executable memory",
            ));
        }

        // Everything that allocates happens before any thread is stopped.
        let mut registry = lock_installed();
        if registry
            .iter()
            .any(|detour| detour.overlaps(target, expected.len().max(JUMP_LEN)))
        {
            return Err(DetourError::AlreadyHooked { address: target });
        }
        let mut window = vec![0u8; span];
        SelfMemory::new()
            .read_exact(target as u64, &mut window)
            .map_err(|_| not_executable("the code cannot be read"))?;
        if let Some(offset) = expected.first_mismatch(&window) {
            return Err(DetourError::SignatureMismatch {
                address: target,
                offset,
                found: window.get(offset).copied().unwrap_or(0),
            });
        }
        let prologue = analyze::analyze(target, &window)?;
        if expected.len() < prologue.len {
            return Err(DetourError::SignatureTooShort {
                address: target,
                needed: prologue.len,
                given: expected.len(),
            });
        }
        if registry
            .iter()
            .any(|detour| detour.overlaps(target, prologue.len))
        {
            return Err(DetourError::AlreadyHooked { address: target });
        }
        let stolen = prologue.len;
        let original: Vec<u8> = window.get(..stolen).unwrap_or_default().to_vec();

        let mut anchors = Vec::with_capacity(prologue.anchors.len() + 1);
        anchors.push(target);
        anchors.extend_from_slice(&prologue.anchors);
        let block = NearBlock::allocate_near(&anchors)?;
        let relocated = analyze::relocate(target, &prologue, block.base())?;
        let moved_len = relocated.code.len();
        let relay_offset = (moved_len + ABSOLUTE_JUMP_LEN).next_multiple_of(16);
        if relay_offset + ABSOLUTE_JUMP_LEN > BLOCK_SIZE {
            return Err(DetourError::Unrelocatable {
                address: target,
                reason: "the moved instructions do not fit in a trampoline block".to_owned(),
            });
        }
        let mut code = relocated.code;
        code.extend_from_slice(&absolute_jump(target + stolen));
        code.resize(relay_offset, INT3);
        code.extend_from_slice(&absolute_jump(replacement));
        block.fill(&code)?;
        block.make_executable()?;
        let trampoline = block.base();
        let relay = trampoline + relay_offset;

        let entry =
            relative_jump(target, relay).ok_or(DetourError::NoNearMemory { address: target })?;
        let mut patch = Vec::with_capacity(stolen);
        patch.extend_from_slice(&entry);
        patch.resize(stolen, INT3);

        let mut map: Vec<(u32, u32)> = prologue
            .offsets
            .iter()
            .copied()
            .zip(relocated.offsets.iter().copied())
            .collect();
        let end = |value: usize| {
            u32::try_from(value).map_err(|_| DetourError::Unrelocatable {
                address: target,
                reason: "internal: offset overflow".to_owned(),
            })
        };
        map.push((end(stolen)?, end(moved_len)?));

        let inner = Arc::new(Inner {
            target,
            original,
            patch,
            block: trampoline,
            trampoline,
            moved_len,
            relay,
            replacement,
            map,
        });

        let mut threads = ThreadSet::prepare()?;
        if let Some(cell) = original_cell {
            cell.store(trampoline, Ordering::Release);
        }
        threads.suspend();
        let outcome = apply_patch(&threads, &inner);
        drop(threads);
        if let Err(failure) = outcome {
            if let Some(cell) = original_cell {
                cell.store(0, Ordering::Release);
            }
            // A thread may already have been moved into the trampoline, so
            // the block is kept, not freed.
            let _ = block.into_raw();
            return Err(failure.into_error(target));
        }
        let _ = block.into_raw();
        registry.push(Arc::clone(&inner));
        Ok(Self { inner })
    }

    /// The detoured function.
    #[must_use]
    pub fn target(&self) -> usize {
        self.inner.target
    }

    /// Number of bytes the patch overwrites.
    #[must_use]
    pub fn stolen_len(&self) -> usize {
        self.inner.original.len()
    }

    /// The trampoline: a function that runs the original code. It has the
    /// signature of the target.
    ///
    /// Call it only from inside a replacement made with
    /// [`detour_fn!`](crate::detour_fn), or under an [`InFlightGuard`], or
    /// [`uninstall_and_wait`](Self::uninstall_and_wait) cannot know a thread
    /// is using it.
    #[must_use]
    pub fn original(&self) -> *const () {
        self.inner.trampoline as *const ()
    }

    /// The trampoline as a function pointer of type `F`.
    ///
    /// `F` must be an `extern "system"` function pointer type, which is
    /// pointer-sized; any other size is a compile error.
    ///
    /// # Safety
    ///
    /// `F` has the signature of the target function and the calling
    /// convention it uses.
    #[must_use]
    pub unsafe fn original_as<F: Copy>(&self) -> F {
        const {
            assert!(
                size_of::<F>() == size_of::<usize>(),
                "original_as needs a function pointer type"
            );
        }
        let address = self.inner.trampoline;
        // SAFETY: `F` is pointer-sized (checked at compile time) and the
        // caller promises it is the function pointer type of the target.
        unsafe { core::mem::transmute_copy::<usize, F>(&address) }
    }

    /// Restores the original bytes. The trampoline and relay memory is kept
    /// allocated, because a thread may still run in it; use
    /// [`uninstall_and_wait`](Self::uninstall_and_wait) to free it.
    ///
    /// A thread stopped in the trampoline or relay is moved back to the
    /// matching place in the restored code.
    ///
    /// # Errors
    ///
    /// [`DetourError::Modified`] when the bytes at the target are no longer
    /// the ones this detour wrote: someone else patched the function. The
    /// code is left alone and the detour stays registered, so that
    /// [`uninstall_all_and_wait`] can try again. [`DetourError::Os`] when a
    /// protection change or the thread list fails.
    pub fn uninstall(self) -> Result<(), DetourError> {
        let mut registry = lock_installed();
        restore(&self.inner)?;
        registry.retain(|detour| !Arc::ptr_eq(detour, &self.inner));
        Ok(())
    }

    /// Restores the original bytes, then waits up to `timeout` until nothing
    /// can run in the replacement, the relay or the trampoline, and frees
    /// their memory.
    ///
    /// Idle means: no thread is inside a [`detour_fn!`](crate::detour_fn)
    /// replacement or holds an [`InFlightGuard`] (counted over **all**
    /// detours of the process), and no suspended thread's instruction pointer
    /// is in the block or in the first bytes of the replacement. It must hold
    /// for [`UNLOAD_GRACE`] (50 ms) in a row, so give a `timeout` well above
    /// that.
    ///
    /// Returns `Ok(true)` after freeing, and `Ok(false)` when the timeout
    /// passed first: the memory is leaked on purpose, because a thread may
    /// still run in it, and the module holding the replacement must not be
    /// unloaded.
    ///
    /// # Errors
    ///
    /// As [`uninstall`](Self::uninstall).
    pub fn uninstall_and_wait(self, timeout: Duration) -> Result<bool, DetourError> {
        {
            let mut registry = lock_installed();
            restore(&self.inner)?;
            registry.retain(|detour| !Arc::ptr_eq(detour, &self.inner));
        }
        let idle = wait_until_idle(core::slice::from_ref(&self.inner), timeout)?;
        if idle {
            // SAFETY: the bytes are restored, no thread is in the block or
            // counted inside a replacement, and the block is released once:
            // the detour was removed from the registry above.
            unsafe { memory::release(self.inner.block) };
        }
        Ok(idle)
    }
}

/// Moves the threads out of the stolen bytes and writes the patch.
fn apply_patch(threads: &ThreadSet, inner: &Inner) -> Result<(), Failure> {
    let stolen = inner.original.len();
    threads.visit(|rip| {
        let rip = rip as usize;
        let Some(offset) = rip.checked_sub(inner.target) else {
            return Visit::Keep;
        };
        // A thread at the very start has not run anything yet and takes the
        // new jump; one inside the stolen bytes continues in the trampoline.
        if offset == 0 || offset >= stolen {
            return Visit::Keep;
        }
        match inner
            .map
            .iter()
            .find(|&&(original, _)| original as usize == offset)
        {
            Some(&(_, moved)) => Visit::MoveTo((inner.trampoline + moved as usize) as u64),
            None => Visit::Abort,
        }
    })?;
    // SAFETY: the target is mapped code (checked by `install`); every other
    // thread is suspended and none is left inside the stolen bytes.
    unsafe { memory::write_code_if(inner.target, &inner.original, &inner.patch) }?;
    Ok(())
}

/// Writes the original bytes back if the patch is still in place, and moves
/// threads out of the trampoline and relay. The caller holds the registry
/// lock.
fn restore(inner: &Inner) -> Result<(), DetourError> {
    let mut threads = ThreadSet::prepare()?;
    threads.suspend();
    // SAFETY: the target is mapped code; every other thread is suspended.
    let written = unsafe { memory::write_code_if(inner.target, &inner.patch, &inner.original) };
    if written.is_ok() {
        // A thread that cannot be moved stays where it is; the wait for
        // idleness covers it.
        let _ = threads.visit(|rip| match inner.way_back(rip as usize) {
            Some(address) => Visit::MoveTo(address as u64),
            None => Visit::Keep,
        });
    }
    drop(threads);
    written.map_err(|failure| Failure::from(failure).into_error(inner.target))
}

/// Whether no thread is counted inside a replacement and none is stopped in
/// the memory of `detours`.
fn is_idle(detours: &[Arc<Inner>]) -> Result<bool, DetourError> {
    if in_flight() != 0 {
        return Ok(false);
    }
    let mut threads = ThreadSet::prepare()?;
    threads.suspend();
    let mut busy = false;
    let _ = threads.visit(|rip| {
        if detours.iter().any(|detour| detour.holds(rip as usize)) {
            busy = true;
        }
        Visit::Keep
    });
    drop(threads);
    Ok(!busy && in_flight() == 0)
}

/// Polls [`is_idle`] until it has held for [`UNLOAD_GRACE`], or `timeout`
/// passes.
fn wait_until_idle(detours: &[Arc<Inner>], timeout: Duration) -> Result<bool, DetourError> {
    let deadline = Instant::now() + timeout;
    let mut quiet_since: Option<Instant> = None;
    loop {
        let now = Instant::now();
        if is_idle(detours)? {
            let since = *quiet_since.get_or_insert(now);
            if now.duration_since(since) >= UNLOAD_GRACE {
                return Ok(true);
            }
        } else {
            quiet_since = None;
        }
        if now >= deadline {
            return Ok(false);
        }
        std::thread::sleep(WAIT_POLL.min(deadline.saturating_duration_since(now)));
    }
}

/// Removes every detour this library still holds, then waits up to `timeout`
/// until nothing can run in a replacement, relay or trampoline, and frees
/// their memory. This is what `hot-unload` calls before the DLL unloads.
///
/// Waits for [`in_flight`] to reach 0 even when no detour is left, because
/// the replacement of one that was already uninstalled may still be running.
///
/// Returns `Ok(true)` when it is safe to unload, `Ok(false)` on timeout (the
/// memory is leaked on purpose).
///
/// # Errors
///
/// The first error of an [`uninstall`](Detour::uninstall) that failed. Every
/// detour that could be removed still was; the ones that could not stay
/// installed, and the module must not be unloaded.
pub fn uninstall_all_and_wait(timeout: Duration) -> Result<bool, DetourError> {
    let mut restored = Vec::new();
    let mut first_error = None;
    {
        let mut registry = lock_installed();
        for detour in core::mem::take(&mut *registry) {
            match restore(&detour) {
                Ok(()) => restored.push(detour),
                Err(error) => {
                    first_error.get_or_insert(error);
                    registry.push(detour);
                }
            }
        }
    }
    let idle = wait_until_idle(&restored, timeout)?;
    if idle {
        for detour in &restored {
            // SAFETY: restored above, nothing runs in the block (idle), and
            // each block is released once: the detours left the registry.
            unsafe { memory::release(detour.block) };
        }
    }
    match first_error {
        Some(error) => Err(error),
        None => Ok(idle),
    }
}
