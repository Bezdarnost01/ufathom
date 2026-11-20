# Changelog

All notable changes are recorded here, in the [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) format. The project follows [Semantic Versioning](https://semver.org/); releases before 1.0.0 may change APIs and output formats between minor versions.

## [Unreleased]

### Added

- **Typed hooks.** A generated SDK now implements `ufathom_runtime::inproc::GameFunction` (behind its `inproc` feature) for the parameter struct of every function, with the function's path, whether it is native, and the struct itself as the parameter block. `GameThread::before_fn::<F>` and `after_fn::<F>` hook a function by that type, so a wrong name does not compile, and the callback gets the parameter block as `&mut F::Params`. If the live block is smaller than the struct, the callback is skipped. `before_native_fn::<F>` and `after_native_fn::<F>` accept only functions marked `FUNC_Native`; a script function is a compile error. The string-path hooks stay.
- **Asset preload before a dump** (`[preload]` in `ufathom.toml`, off by default). The DLL lists the assets under the given content paths through the asset registry and loads the Blueprint ones on the game thread, then reads the dump again. Classes that the game has not loaded yet reach the dump and the SDK this way. Every call is planned from the dump that was just read: parameter offsets, block sizes and the `FAssetData` layout, which is `AssetClass` before UE 5.1 and `AssetClassPath` after. Afterwards the hook is removed, and the DLL refuses to unload while a thread is still inside it. Checked in a UE 4.25 editor, where Blueprints under the requested path reached the dump and others did not; not yet run in a shipping game.
- **Freeing engine memory.** `InProcessExecutor` has `take_string`, `take_strings`, `free_string` and `free_array`, and `ufathom_runtime::engine_mem` has the same functions for any executor. They hand strings and arrays the engine returned back to its allocator through `KismetArrayLibrary::Array_Clear`, so calling such functions every frame no longer leaks. Buffers of `FStringBuf` and `TArrayBuf` are refused.
- **Native function hooks.** `GameThread::before_native` and `after_native` hook `UFunction::Func`. Blueprint calls of native functions go through that pointer without `ProcessEvent`. The offset of `Func` is solved inside the game.
- **Hook upkeep.** `start_auto_rescan` hooks the virtual tables of classes that get their first instance after `install`. `uninstall_and_wait` restores everything and reports when no thread is still inside a hook, so the module can be unloaded.
- **`hot-unload` SDK feature.** When `run` returns, the DLL removes its hooks and unloads itself. It stays loaded if a thread is still inside a hook.
- **`proxy-version` SDK feature.** Rename the built DLL to `version.dll` and put it next to the game, and the game loads it at start-up with no injector. It exports all 17 functions of the system `version.dll` and forwards each one to the real DLL, which is loaded from the system folder on first use. `run` waits until the engine has started.
- **Example config.** `ufathom.example.toml` lists every DLL key with its default, and ships in the release archive. A test keeps it valid.

### Changed

- **Game thread choice.** The game thread is now chosen by OS thread id: first a thread named with `set_game_thread`, then the process main thread, and only then the first thread to make 64 hooked calls. Before, any busy worker thread could win.
- **Faster hook.** Callbacks live in a copy-on-write table behind a bit filter. Calls of unhooked functions take no lock, and no call allocates.

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
