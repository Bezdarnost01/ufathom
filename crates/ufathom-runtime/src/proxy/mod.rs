//! Proxy DLL exports: a generated SDK DLL that poses as a system DLL.
//!
//! A DLL built with one of the `proxy-version`, `proxy-dsound`,
//! `proxy-dinput8` or `proxy-winmm` features exports every function of the
//! matching system DLL (see [`PRESETS`]). Named `version.dll`, `dsound.dll`,
//! `dinput8.dll` or `winmm.dll` and put beside the game executable, it is
//! loaded by the game at start-up, with no injector. Each call is forwarded to
//! the real DLL in the system folder.
//!
//! # How a call is forwarded
//!
//! Typed forwarders do not scale (`winmm` has about 180 exports, and some are
//! undocumented), and linker forwarders cannot do this: `link.exe` refuses a
//! forwarder that names the module by full path, and one that names it as
//! `version` would resolve to this DLL again. Instead each export is a small
//! assembly stub, written with `global_asm!`, that jumps through a per-export
//! slot to the real function. The slots start empty. On the first call a stub
//! asks a resolver, which loads `<system folder>\<name>.dll` by full path
//! once, finds the export with `GetProcAddress` (by name, or by ordinal for an
//! export that has no name), stores the address in the slot and continues
//! there. Nothing runs in `DllMain`, and nothing is loaded until the game
//! calls one of the exports.
//!
//! # Why the signature does not matter
//!
//! On Windows x64 a function receives its first four integer or pointer
//! arguments in `rcx`, `rdx`, `r8` and `r9`, its first four floating-point
//! arguments in `xmm0`-`xmm3`, and the rest on the stack above the return
//! address. A stub is entered by a `call`, so the return address is on top of
//! the stack, and it ends in a `jmp`: it never pushes, pops or reads the stack
//! arguments, and the real function returns straight to the caller. On the
//! fast path the stub overwrites only `r11`, which is volatile and never an
//! argument register. On the first call the resolver runs between the stub and
//! the jump: it reserves its own frame below the return address, saves `rcx`,
//! `rdx`, `r8`, `r9` and `xmm0`-`xmm3`, calls Rust (which may clobber any
//! volatile register), restores them exactly, releases the frame and jumps.
//! The real function then sees the registers and stack the caller built.
//! Callee-saved registers are not touched by the stubs, and the Rust code
//! preserves them by its own calling convention. So the same stub forwards
//! `HRESULT`-returning COM entry points, variadic functions and functions
//! with more than four arguments alike, whatever their signatures are.
//!
//! # When the real DLL cannot be used
//!
//! If the system DLL cannot be loaded, the slot points to a function that
//! sets `ERROR_MOD_NOT_FOUND` and returns 0, and if it lacks one export (an
//! older Windows than the table was read from), `ERROR_PROC_NOT_FOUND`. The
//! caller then sees 0, which is the failure value of most of these functions.
//! For a function whose failure value is not 0 (an `HRESULT` returning
//! function reads 0 as `S_OK`), this is best-effort: the call does not crash,
//! but the result is not a faithful failure.
//!
//! # Linking
//!
//! Rust's `#[export_name]` cannot set an ordinal, and some games import by
//! ordinal (`DirectSoundCreate` is ordinal 1). The generated SDK therefore
//! passes a module-definition file to the linker, which maps each public name
//! and ordinal to the stub `ufathom_proxy_<dll>_<ordinal>`. The file comes
//! from [`Preset::def_file`]. Enable one preset only in a DLL: the stubs of
//! several presets can coexist in one build, but the DLL takes its exports
//! from a single `.def` file.

#[cfg(not(target_arch = "x86_64"))]
compile_error!("the proxy features support x86-64 Windows only");

#[macro_use]
mod tables;
mod resolve;
mod stubs;

pub use tables::{DINPUT8, DSOUND, Export, PRESETS, Preset, VERSION, WINMM};
