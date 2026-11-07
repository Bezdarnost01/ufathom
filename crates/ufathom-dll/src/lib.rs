//! The `ufathom.dll` shell: `DllMain`, thread lifecycle, hotkeys and panic containment.
//!
//! **This crate is Windows only.** On any other target it compiles to an empty
//! library, so the workspace still checks there; all session logic lives in
//! `ufathom-agent`, which builds everywhere.
//!
//! # Life of the DLL
//!
//! 1. `DllMain`, on process attach, calls `DisableThreadLibraryCalls` and
//!    `CreateThread` and returns. Nothing else runs under the loader lock.
//! 2. The worker thread pins its own module, takes the per-process mutex
//!    `Local\ufathom-<pid>` (a second copy refuses and unloads itself), loads
//!    `ufathom.toml`, opens the log file and starts the agent's `Service`.
//! 3. It polls about every 50 ms: the service (inbox, dump results, automatic
//!    dump) and the hotkeys, which act only while a window of the process is in
//!    the foreground.
//! 4. On `stop`, the unload hotkey or `auto_unload`: the service refuses new
//!    work and cancels the running dump, the session's pool threads are joined,
//!    `status.json` reads `stopped`, the log is closed, the mutex released and
//!    the worker calls `FreeLibraryAndExitThread`. If a thread did not end in
//!    time the DLL stays loaded instead: unmapping code that still runs would
//!    crash the host.
//!
//! The DLL reads the target only through `SelfMemory`, calls no game function
//! and installs no hook.
//!
//! # Unloading and reference counts
//!
//! The worker pins the module with `GetModuleHandleExW`, which adds one
//! reference, so the module cannot vanish while the worker runs. The loader's
//! own `LoadLibrary` reference is the other one. To unload, the worker drops
//! the pin with `FreeLibrary` and then releases the loader's reference with
//! `FreeLibraryAndExitThread`, so the last release and the thread's exit both
//! happen in `kernel32`, outside this module. A loader that also frees the
//! module itself while the DLL is running would over-release: the contract is
//! that the DLL, not its loader, ends its own life.

#![cfg(windows)]

mod entry;
mod logging;
mod os;
mod worker;
