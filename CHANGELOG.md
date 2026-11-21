# Changelog

All notable changes are recorded here, in the [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) format. The project follows [Semantic Versioning](https://semver.org/); releases before 1.0.0 may change APIs and output formats between minor versions.

## [Unreleased]

### Added

- **Companion DLLs: run an existing mod DLL next to the Rust one.** A generated SDK DLL, in any build, reads `ufathom-companions.txt` from its own folder on the worker thread (never in `DllMain`) and loads each DLL named there with `LoadLibraryExW(full_path, LOAD_WITH_ALTERED_SEARCH_PATH)`, in file order, so a team can move a C++ mod to Rust one feature at a time. One file name or relative path per line, `#` comment lines; absolute paths and `..` are refused, logged with their line number and skipped, and the other lines still load. Companions load at once, before the proxy build waits for the engine, unless the line starts with `after-engine:`. They are not unloaded by default; the line `unload-on-exit` frees them in reverse order after the hooks are removed, when a `hot-unload` DLL unloads itself. No file means no change. The parser is the pure `ufathom_runtime::companions::parse` with unit tests; loading is tested with real DLLs on Windows (plain, proxy and `hot-unload` builds), not yet run next to a real game mod.
- **`HookMode::Detour`: `ProcessEvent` through one detour.** `GameThread::install_with(desc, HookMode::Detour)` detours the engine's `UObject::ProcessEvent` itself, once, instead of swapping its slot in every virtual table (`install` stays `HookMode::Vtable`, which patches no code). Classes with no live object yet and classes that override `ProcessEvent` and call the base are covered, and `rescan`, `start_auto_rescan` and `start_rescan_on_module_load` become no-ops (`Ok(0)`, `Ok(false)`). The same entry code runs in both modes: game thread detection, task drain, callbacks, the busy flag and `catch_unwind`. The mode needs the `detour` feature on Windows x86-64 and otherwise fails with `HookError::DetourUnavailable`. There is no author signature for this target: the address is the one the vote of virtual tables found, checked against its own current bytes, and the detour analysis (whole instructions, no `ret`, trap or branch into the stolen bytes) still applies. `uninstall_and_wait` waits for the hook's in-flight count and the detour's and then frees the trampoline; bytes another tool patched over are left alone and reported as `HookError::Chained`. New: `HookMode`, `GameThread::mode` and `detour_target`, and the errors `HookError::DetourUnavailable`, `Detour` and `ModeConflict`. Tested on a fake engine whose `ProcessEvent` is hand-assembled machine code; not yet run in a real game.
- **One registration for a game function.** `GameThread::hook::<F>(When::Before | When::After, |ctx, params| ...)` hooks a script function through `ProcessEvent` with typed parameters, and `hook_native::<F>(When, |ctx| ...)` hooks a native function through `UFunction::Func` with a `NativeContext`. A native function is registered on `Func` alone: every invocation reaches it, `ProcessEvent`'s included, so each call fires once, where a second registration on `ProcessEvent` would fire twice. Both return the usual `HookId`, so guards and `remove_owned_by` work as for the other hooks. A function of the wrong kind does not compile and the error names the other method. A "Which hook do I use?" table is in the README and the `inproc` module docs.
- **Field writes for mods** (`inproc` feature of `ufathom-runtime`). `session.live_object(ptr)` on a session over `SelfMemory` returns a `LiveObject` with `set(T::FIELD, value)`, `modify(T::FIELD, |v| ..)` and `set_bit(T::BIT, bool)`, so a mod can write `camera_manager.DefaultFOV = 110` without a setter function. Before each write the object must still sit in the object array slot its index names, the field must lie inside `PropertiesSize` of the object's own class, and the page must be committed read-write; otherwise a `WriteError` is returned and nothing is written. `ufathom-mem` gains `SelfMemory::write_exact`, which never faults and never changes page protection (`WriteProcessMemory` after a `VirtualQueryEx` check on Windows, `process_vm_writev` after a `/proc/self/maps` check on Linux, where it is compiled and linted but has not been run). It is not part of `MemoryReader`, so the dump cannot reach it, and `ProcessMemory` stays read-only. A write from a thread other than the game thread races with the engine, and `modify` and `set_bit` are not atomic against it; write inside `GameThread::run` or a hook callback. Tested against a synthetic engine and a generated SDK; not yet run in a real game.
- **Typed hooks.** A generated SDK now implements `ufathom_runtime::inproc::GameFunction` (behind its `inproc` feature) for the parameter struct of every function, with the function's path, whether it is native, and the struct itself as the parameter block. `GameThread::before_fn::<F>` and `after_fn::<F>` hook a function by that type, so a wrong name does not compile, and the callback gets the parameter block as `&mut F::Params`. If the live block is smaller than the struct, the callback is skipped. `before_native_fn::<F>` and `after_native_fn::<F>` accept only functions marked `FUNC_Native`; a script function is a compile error. The string-path hooks stay.
- **Asset preload before a dump** (`[preload]` in `ufathom.toml`, off by default). The DLL lists the assets under the given content paths through the asset registry and loads the Blueprint ones on the game thread, then reads the dump again. Classes that the game has not loaded yet reach the dump and the SDK this way. Every call is planned from the dump that was just read: parameter offsets, block sizes and the `FAssetData` layout, which is `AssetClass` before UE 5.1 and `AssetClassPath` after. Afterwards the hook is removed, and the DLL refuses to unload while a thread is still inside it. Checked in a UE 4.25 editor, where Blueprints under the requested path reached the dump and others did not; not yet run in a shipping game.
- **Freeing engine memory.** `InProcessExecutor` has `take_string`, `take_strings`, `free_string` and `free_array`, and `ufathom_runtime::engine_mem` has the same functions for any executor. They hand strings and arrays the engine returned back to its allocator through `KismetArrayLibrary::Array_Clear`, so calling such functions every frame no longer leaks. Buffers of `FStringBuf` and `TArrayBuf` are refused.
- **Native function hooks.** `GameThread::before_native` and `after_native` hook `UFunction::Func`. Blueprint calls of native functions go through that pointer without `ProcessEvent`. The offset of `Func` is solved inside the game.
- **Hook upkeep.** `start_auto_rescan` hooks the virtual tables of classes that get their first instance after `install`. `uninstall_and_wait` restores everything and reports when no thread is still inside a hook, so the module can be unloaded.
- **`hot-unload` SDK feature.** When `run` returns, the DLL removes its hooks and unloads itself. It stays loaded if a thread is still inside a hook.
- **Hook handles that clean up.** `HookId::into_guard(game)` returns a `HookGuard` that removes the callback when it drops, and `HookGuard::forget` keeps it. Each callback and queued task also records the module that holds its code, and `GameThread::remove_owned_by(module_base)` and `remove_owned_by_current_module()` remove everything one module registered. The generated `hot-unload` code calls the second one before `uninstall_and_wait`, so no callback or task of an unloaded DLL stays queued.
- **Rescan when a module loads.** `GameThread::start_rescan_on_module_load()` registers a loader notification (`LdrRegisterDllNotification`) and rescans one and five seconds after a module loads, which is when the classes of a new native module appear. Loads close together share their rescans, and nothing runs between loads. `rescans_run()` and `module_loads_seen()` count what happened. It does nothing on other platforms; `start_auto_rescan` stays for them and for classes that get their first instance with no module load.
- **Proxy DLLs for `version`, `dsound`, `dinput8` and `winmm`.** The SDK features `proxy-version`, `proxy-dsound`, `proxy-dinput8` and `proxy-winmm` make the built DLL pose as that system DLL: name it accordingly and put it next to the game, and the game loads it at start-up with no injector. It exports every function of the system DLL at the same ordinals, including exports that have only an ordinal, and forwards each call to the real DLL, which is loaded from the system folder on first use. `run` waits until the engine has started. Each export is a small assembly stub that passes the arguments and the stack through untouched, so signatures do not matter. Enable one feature only; two at once are a compile error. Generated SDKs get a `build.rs` and `proxy/*.def` files, and a `ufathom_proxy` cfg. The export tables are compared with the system DLLs by a test; calls through built proxy DLLs were checked for `version` and `dsound`, and the other two were checked for their exports and one call each; none was run in a real game.
- **Inline detours for plain native functions** (`detour` feature of `ufathom-runtime`, and of generated SDKs; Windows x86-64 only). Mods can hook game functions that are not reflected and never reach `ProcessEvent`, found by RVA or by byte pattern (`Signature`, `find_in_module`, `module_base`). `Detour::install` refuses to patch unless the bytes at the target match the expected signature, and decodes the start of the function with `iced-x86`: it refuses `ret`, `int3`, a jump that leaves stolen bytes behind it and a branch into the stolen bytes, and relocates RIP-relative operands and relative branches into a trampoline within 2 GB of the target. The patch is written with all other threads suspended, and `uninstall` refuses to restore over a patch made by someone else. `detour_fn!` defines replacements that count the threads inside and catch panics, and `uninstall_and_wait` (and the SDK's `hot-unload`) free the memory only when no thread can still run in it. It is the one module that patches code: opt-in, mod-only, and never used by the dump. Tested on hand-written machine code (prologues, RIP-relative operands, branches, a concurrent caller during 100 installs and removals); not yet run in a real game.
- **Example config.** `ufathom.example.toml` lists every DLL key with its default, and ships in the release archive. A test keeps it valid.

### Changed

- **`sdk::game_thread()` returns the installed hook** when there is one, whatever its mode, so an SDK can install `HookMode::Detour` first and still use it.
- **Game thread choice.** The game thread is now chosen by OS thread id: first a thread named with `set_game_thread`, then the process main thread, and only then the first thread to make 64 hooked calls. Before, any busy worker thread could win.
- **Faster hook.** Callbacks live in a copy-on-write table behind a bit filter. Calls of unhooked functions take no lock, and no call allocates.
- **`uninstall` reports slots it cannot restore.** Two mods that each link the runtime can hook the same slot, the second saving the first one's hook as its original. Before, the first mod's `uninstall` found the slot holding the other hook, skipped it silently and reported success, so a `hot-unload` DLL unmapped itself while the other mod still forwarded into it. Now such a `ProcessEvent` slot or `Func` field is left as it is and `uninstall` and `uninstall_and_wait` return the new `HookError::Chained`; the hook keeps forwarding, `hot-unload` keeps the DLL loaded, and `uninstall` can be called again after the other mod has removed its hook. A slot that fails to restore for any reason is now also kept, so a later `uninstall` retries it.

### Fixed

- **Generated SDKs link the C runtime statically.** Every SDK now includes a `.cargo/config.toml` with `+crt-static` for `x86_64-pc-windows-msvc`. Before, the built DLL and external executable imported `VCRUNTIME140.dll` and failed to load on machines without the Visual C++ Redistributable. The file is refreshed on every generation.
- **`ufathom quick` list number versus PID.** A number that is both a list number and the PID of another process (easy to hit with `--all`) is no longer taken as the list number silently. `quick` names both processes and asks again. `#N` selects a list number and `pid:N` a PID.

## [0.3.0] - 2026-11-14

### Changed

- **Breaking:** wrappers of static game functions no longer take `this`. They call the class default object themselves, so existing calls of such wrappers must drop the object argument.

### Added

- **In-game toolbox in every SDK.** `sdk::session(memory)` opens a `Session` from the build description the SDK now carries as `ENGINE`. It provides:
  - `FName` to text, object paths and full names;
  - `find_object`, `find_by_name` and `objects_of::<T>()`;
  - `is_a` and `cast::<T>()`;
  - `static_class::<T>()` and `default_object::<T>()`;
  - `gworld()` and `gengine()`.

  It works inside the game with `SelfMemory` and from another process with `ProcessMemory`. Every generated class and struct implements `TypePath`.
- **Calling game functions.** With the SDK's `inproc` feature, `unsafe { sdk::attach() }` returns an `InProcessExecutor`. Generated wrappers call through `ProcessEvent` with it, after the object, the function and the parameter block size are checked.
- **Game thread hook.** With `inproc`, `unsafe { sdk::game_thread() }` hooks `ProcessEvent` by swapping its virtual table slot. No code is patched. The hook supports:
  - `run` and `call` to run code on the game thread, with an executor;
  - `before` and `after` callbacks on any game function, which can read or change parameters and skip the original;
  - `uninstall` and `rescan`.
- **Static functions and strings.** Wrappers of static game functions take no `this`: they call the class default object, which the executor finds through the new `Executor::default_object`. `FStringBuf` and `TArrayBuf` pass Rust-owned strings and arrays as input parameters. `Session::fstring`, `fname_text` and `read_array` read engine strings, names and arrays. Together these give `FName` from text through `UKismetStringLibrary::Conv_StringToName` and text from `FText` through `UKismetTextLibrary`.
- **Typed globals and faster lookups.** The SDK has `gworld(&session)` and `gengine(&session)`, which return the generated `UWorld` and `UEngine`. `find_object` and `find_by_name` use a name index built in one walk of the object array, and walk again only on a miss.
- **Russian user guide** in `docs/ru/README.md`, with a mapping from Dumper-7 SDK calls.
- Dumps record the `ProcessEvent` vtable index and the `GWorld` and `GEngine` slots under `globals`. Dumps without them still load.

## [0.2.1] - 2026-11-09

### Added

- Every generated SDK now includes an editable `src/main.rs` with a shared application entry point. The crate builds as a resident Windows DLL or as an external executable that opens a target PID for read-only access. Regenerating the SDK preserves the existing `main.rs`.

## [0.2.0] - 2026-11-08

### Added

- **`ufathom quick`.** An interactive external-dump workflow lists probable Unreal Engine processes and accepts a list number or PID. It then runs the same discovery and reflection pipeline as `dump` and writes `dump.json`, `Mappings.usmap` and a self-contained Rust SDK. Each run gets a new timestamped folder, so existing results are not overwritten. `--all` offers every running process when the UE heuristic misses a target, and `--output` changes the parent folder.

## [0.1.1] - 2026-11-08

### Changed

- The CLI and DLL now generate a self-contained Rust SDK by default. The matching `ufathom-runtime` and `ufathom-mem` sources and licenses are written under `vendor/`, so the SDK builds without an ufathom checkout or a published runtime package. `--runtime-path` and `--runtime-version` remain available as explicit overrides.

## [0.1.0] - 2026-11-08

First pre-release. Windows x86-64 only for the DLL. Expect breaking changes in the API, the dump format and the generated SDK before 1.0.

### Added

- **`ufathom.dll`.** Load it into a running Unreal Engine game and it writes a dump without any offsets from you:
  - It starts its own worker thread and dumps automatically after a delay you choose.
  - More dumps run on command, and unload is clean.
  - Commands and status go through files in its output folder.
  - All reads of game memory go through non-faulting reads, so a wrong guess fails the dump, not the game.
- **Layout solver.** The engine layout is derived from invariants, not from per-version offset tables:
  - the object array shape;
  - the name table, found by its first entries;
  - core reflection types;
  - property link chains.

  Each result carries evidence. When the evidence is ambiguous, the solver refuses rather than guesses. Modular builds such as the editor are supported.
- **Dump formats.** A deterministic JSON dump, an ABI-aware diff between two dumps, and `.usmap` mappings, both read and write.
- **Rust SDK generator.** It produces a crate with `#[repr(C)]` types and `offset_of!` and `size_of` asserts:
  - enums as newtypes and bitfield accessors;
  - parents embedded through `Deref`;
  - function wrappers over a user-supplied executor.

  Anything that cannot be represented safely becomes bytes and is listed in a report.
- **`ufathom` command line tool:**
  - inspect an executable;
  - list probable engine processes (`ps`);
  - discover and dump a running process by external reads, without injecting anything;
  - generate an SDK;
  - read and write usmap;
  - diff dumps;
  - check a dump against an independent usmap;
  - control a loaded DLL.
- **`ufathom-testkit`.** Synthetic engine images for every minor version from UE 4.11 to 5.8, with corruption modes and decoys. `fake-host` is a real process that hosts an image and can load the DLL.
- **Release workflow.** Binaries are built only by GitHub Actions from a version tag, with `SHA256SUMS` and build provenance.

### Verified on real builds

- **UE 4.25 editor:**
  - external discovery and dump;
  - DLL dump, repeat dump and unload;
  - members checked against the engine source;
  - the generated SDK compiles.
- **UE 4.21 shipping build:**
  - DLL dump, compared with an independent dumper's output for the same build;
  - the generated SDK compiles.
- **Other engine versions:** tested on synthetic images only. See `COMPATIBILITY.md`.

[Unreleased]: https://github.com/Bezdarnost01/ufathom/compare/v0.3.0...HEAD
[0.3.0]: https://github.com/Bezdarnost01/ufathom/compare/v0.2.1...v0.3.0
[0.2.1]: https://github.com/Bezdarnost01/ufathom/compare/v0.2.0...v0.2.1
[0.2.0]: https://github.com/Bezdarnost01/ufathom/compare/v0.1.1...v0.2.0
[0.1.1]: https://github.com/Bezdarnost01/ufathom/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/Bezdarnost01/ufathom/releases/tag/v0.1.0
