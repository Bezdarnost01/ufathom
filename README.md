# ufathom

[Руководство на русском](docs/ru/README.md)

An Unreal Engine dumper DLL and Rust SDK generator, built to survive what breaks Dumper-7's DLL. Load it in a game and it writes a reflection dump, a usmap and a Rust SDK. If your offsets are wrong, you get an error message, not a crashed game.

**Status: pre-release.** `v0.3.0` is available, with real-build results for the UE 4.25 editor and a UE 4.21 shipping build. See [COMPATIBILITY.md](COMPATIBILITY.md) for exactly what has been tested. The target range is UE 4.11 to 5.8, and support is claimed version by version as real builds are verified.

## Goals

- **A bad read cannot crash the game.** The dump path never dereferences a raw game pointer or calls a game function. Every read can fail safely.
- **The game keeps running.** Dumps run on background threads, with no need for the game thread.
- **Offsets are solved, not tabulated.** Layouts come from invariants the engine must satisfy, so a modified engine should not need a table entry.
- **Proof instead of guessing.** Every offset records how it was found. A contradiction stops the run and says why.
- **Dump again without restarting.** Each dump gets its own folder and an identity record, and the DLL unloads cleanly.
- **A Rust SDK built for untrusted memory.** Layout checked at compile time, enums that are sound for any value, and an SDK that always compiles.

The same pipeline also runs **without a DLL**, reading a running process from outside or a minidump offline.

## Usage

**The simplest external workflow.** Start the game, then run:

```
ufathom quick
```

Choose a process by its list number or PID. If a number is both, such as list number 128 and a different process with PID 128 under `--all`, ufathom asks again: type `#128` for the list number or `pid:128` for the PID. ufathom writes `dump.json`, `Mappings.usmap` and a self-contained Rust SDK into a new folder, without overwriting earlier results:

```
ufathom-dumps/<timestamp>-p<PID>/
├── dump.json
├── Mappings.usmap
└── sdk/
```

Use `ufathom quick --all` if the game is not in the probable-target list, or `ufathom quick --output D:/dumps` to change the parent folder. Empty input cancels before anything is written.

**As a DLL.** Put `ufathom.toml` next to the game executable (start from [`ufathom.example.toml`](ufathom.example.toml), also in the release archive), load `ufathom.dll` with your loader of choice, press F8 to dump and F6 to unload.

```toml
output = "D:/dumps"
formats = ["json", "usmap", "rust-sdk"]
auto_dump = false
```

A dump sees only loaded objects. To get Blueprint classes the game has not loaded yet, add a `[preload]` section. Before each dump, the DLL then lists the assets under these paths through the asset registry, loads the Blueprint ones on the game thread and reads the dump again:

```toml
[preload]
paths = ["/Game"]        # content paths, listed recursively
# classes = ["*"]        # every asset; the default is Blueprint asset classes only
# max_assets = 2000      # at most this many per dump
# batch = 8              # assets per game thread call; the game runs between calls
```

This is the only time ufathom calls game functions. It is off by default, and the game stalls while each batch loads. It has been checked in a UE 4.25 editor, not yet in a shipping game. The rest of the keys are listed in `crates/ufathom-agent/src/config.rs`.

`cargo build -p ufathom-dll` produces `target/<profile>/ufathom_dll.dll`; copy it next to the game as `ufathom.dll`. The library is not called `ufathom`, so that its debug symbols do not clash with the command line tool's on Windows.

**Individual commands, and working with existing results:**

```
ufathom quick                                      # choose a process and write every format
ufathom ps                                         # probable Unreal Engine processes
ufathom inspect-exe Game-Win64-Shipping.exe        # static facts, nothing is run
ufathom doctor --process Game-Win64-Shipping.exe   # how well the layout was solved
ufathom dump --process Game-Win64-Shipping.exe -o dump.json --usmap Mappings.usmap
ufathom gen dump.json -o my_sdk/
ufathom diff old.json new.json
ufathom command <output dir> dump --label menu --wait 120   # drive a loaded DLL
```

A target is `--pid N`, `--process NAME` or `--minidump PATH`, with optional `--module`, `--layout FILE` and `--hint VERSION`. Exit codes: 0 success, 1 failure, 2 usage error, 3 degraded dump. `ufathom --help` has the rest.

By default, `quick`, `gen` and `dump --sdk` include the matching `ufathom-runtime` and `ufathom-mem` sources and licenses under `vendor/`. The generated folder is self-contained: copy it anywhere and build it without an ufathom checkout or manual manifest edits.

```
cargo check --manifest-path my_sdk/Cargo.toml
```

Use `--runtime-path PATH` or `--runtime-version VERSION` only when you want to override the bundled runtime with a local crate or a registry dependency.

### SDK layout and build time

A generated SDK is a Cargo workspace by default (`--layout split`): the packages are member crates under `crates/`, and the root crate (`ufathom-sdk`) re-exports them under the same module paths, so `ufathom_sdk::engine::AActor` reads the same as before. The root crate holds only the engine description, the DLL and executable entry points, and your `src/main.rs`, so changing your code rebuilds only that small crate. Packages that reference each other cyclically share one member crate, and small packages are packed together; a dump with thousands of Blueprint packages gives a few dozen crates.

`--layout single` (also `ufathom dump --sdk-layout single`) writes the earlier layout: one crate, `src/<package>.rs` per package. Regenerating an SDK with the other layout keeps `src/main.rs` and removes only the files the previous generation wrote.

Measured on a UE 4.21 shipping dump with 2,644 classes and 4,381 packages (about one million generated lines; the SDK of one game, 12-core Windows machine that other builds shared, clean target directory, seconds):

| | single | split |
|---|---|---|
| `cargo check`, all packages | 94-117 | 70-78 |
| `cargo check`, `engine` + the game package | 58-68 | 54-64 |
| `cargo build --release --lib`, all packages | 82-97 | 57-80 |
| rebuild after changing `src/main.rs`: `cargo build --release --lib` | 70-83 | 1.8 |
| rebuild after changing `src/main.rs`: `cargo build --lib` (debug) | 40 | 1.3 |
| rebuild after changing `src/main.rs`: `cargo check` | 29-44 | 0.9 |

The first build of a big SDK is still a minute or more, because the engine packages reference each other and form one crate of about 386,000 lines, and the game package waits for it. Only enable what you need: every package is a Cargo feature, `default = ["all", "accessors"]`, and a feature enables the packages it depends on.

```
# a mod that uses the engine and one game package, without the chain accessors
cargo build --release --lib --no-default-features --features "engine,my_game_package"
# with the accessors and the in-game calls
cargo build --release --lib --no-default-features --features "engine,my_game_package,accessors,inproc"
```

The accessors (`accessors`, on by default) are about a third of the type-checking time of the whole SDK: `cargo check` of all packages took 68 seconds in the single layout and 61 in the split one with `--no-default-features --features all`, against 94-117 and 70-78 with them. A build that does not use field chains is shorter with them off.

### Build the generated application

Every SDK includes an editable `src/main.rs`. Put your logic in its `run` function and import generated types through the SDK crate, for example `ufathom_sdk::core_uobject::UObject`. Later SDK generation preserves this file; other generated files may be refreshed. The SDK also gets a `.cargo/config.toml` that links the C runtime statically on `x86_64-pc-windows-msvc`, so the built DLL and executable run on machines without the Visual C++ Redistributable.

**Windows DLL (the default library configuration):**

```bat
cd my_sdk
cargo build --release --lib
```

The result is `target/release/ufathom_sdk.dll`. Loading it starts `run` once on a worker thread with `SelfMemory`. By default the DLL stays loaded until the game exits. Two features change this:

- `--features hot-unload`: when `run` returns, the DLL removes its callbacks and hooks and unloads itself. It stays loaded if a thread is still inside a hook, or if another mod hooked the same slot after it.
- Proxy features: the DLL poses as a system DLL, so the game loads it at start-up with no injector. Enable **one** of them and give the built DLL that name, next to the game executable. It exports every function of the system DLL, at the same ordinals, and forwards each call to the real DLL in the system folder (loaded on the first call). `run` waits until the engine has objects and a `GEngine`.
  - `--features proxy-version`: name it `version.dll`.
  - `--features proxy-dsound`: name it `dsound.dll`.
  - `--features proxy-dinput8`: name it `dinput8.dll`.
  - `--features proxy-winmm`: name it `winmm.dll`.

  Which name a game loads is a property of the game. The forwarding works for any signature, because the stubs pass arguments and the stack through untouched. A function whose real DLL cannot be loaded returns 0 with `ERROR_MOD_NOT_FOUND`, which is a faithful failure only for functions whose failure value is 0. Proxy DLLs are x86-64 Windows only (`x86_64-pc-windows-msvc`).

**External Windows executable:**

```bat
cd my_sdk
cargo build --release --bin ufathom-sdk-external
.\target\release\ufathom-sdk-external.exe <PID>
```

The executable opens the PID with `ProcessMemory` and calls the same `run` function. Access is read-only and requires the same privileges as the target; access denial is reported, not bypassed. External mode does not provide a `ProcessEvent` executor.

### Running an existing mod DLL next to the Rust one

A team with a C++ mod DLL, perhaps loaded through its own `dsound.dll` proxy, can move to Rust one feature at a time: the Rust DLL loads the old DLL(s) too, so both run side by side. Put a plain text file `ufathom-companions.txt` in the **same folder as the built SDK DLL**. Any build reads it (proxy or not), on the worker thread and never in `DllMain`. With no file, nothing happens.

```text
# One DLL per line: a file name or a path relative to this folder.
# A comment takes a whole line. Blank lines are ignored.
legacy_mod.dll
legacy\second.dll
after-engine:needs_the_engine.dll
unload-on-exit
```

- **Paths.** Absolute paths (a drive, a leading `\` or `/`, UNC) and any `..` component are refused: the line is logged with its line number (`OutputDebugString`, like the rest of the DLL's output) and skipped, and the other lines still load. A DLL that fails to load is logged and does not stop the rest. Each is loaded with `LoadLibraryExW(full_path, LOAD_WITH_ALTERED_SEARCH_PATH)`, so it finds its own dependencies in its own folder. The file may be UTF-8 with or without a byte order mark, with LF or CRLF line ends.
- **Order.** Companions load in file order, at once when the worker thread starts, before the proxy build waits for the engine and before `run`, because legacy proxies often expect to run early. A line starting with `after-engine:` loads only once the engine is up (objects loaded and, when the build has one, `GEngine` set); a build that does not wait for the engine loads those right after the others.
- **Unload.** With `hot-unload` the companions are **not** unloaded by default, because the old DLL may not support being unloaded. Add the line `unload-on-exit` to free them in reverse order, after the Rust hooks are removed, and only when the SDK DLL really unloads itself (it stays loaded, and so do they, when a thread is still inside a hook, and always in a proxy build).

The parser is `ufathom_runtime::companions::parse`, a pure function with unit tests. The loading is tested with real DLLs on Windows; it has not been run next to a real game mod.

### Work with game objects

`sdk::session(memory)` opens a `Session` for the build the SDK was generated from. It works in both modes:

```rust
let session = sdk::session(memory)?;
let actor_class = session.static_class::<sdk::engine::AActor>()?;
for pawn in session.objects_of::<sdk::engine::APawn>()? {
    println!("{}", session.full_name_of(pawn)?);
}
let world = session.gworld()?;
```

It also provides `find_object` by path, `find_by_name`, `is_a`, `cast::<T>()`, `default_object::<T>()` and `gengine()`.

Fields are read as a chain, the way C++ writes `actor->RootComponent->RelativeLocation`. Every link is a checked read and returns a `Result`, so `?` stops at the first one that fails:

```rust
use sdk::engine::*; // brings the accessor traits of this package into scope

let loc = session.object(actor).root_component()?.relative_location()?;
let hp = session.object(pawn).health()?; // `Health` is declared on `AActor`, read through an `APawn`
```

A plain field returns its value. An object pointer field returns an `ObjectRef` on the object it points to, so the chain continues. A null pointer is `ChainError::Null { field: "AActor::RootComponent" }`, so the error says where the chain broke; unreadable memory is `ChainError::Memory`. Reads do not check that an object is alive (a read cannot fault, but a stale object gives stale values). Each class gets an extension trait (`AActorFields`) implemented for every object of that class or a derived one, so inherited fields work with no extra code. Bit-field booleans return `bool`. A field name that clashes with an existing method (`get`, `read`, `addr`, `set`, ...) gets a `field_` prefix (`field_get()`), and a name an ancestor already uses gets a numeric suffix.

The accessors are behind the `accessors` feature of the SDK, on by default; turn it off (`--no-default-features --features all`) for a shorter build. Without it, and for the cases a chain does not cover, read with the field constants: `session.object(ptr).get(AActor::ROOT_COMPONENT)` and `.get_bit(AActor::B_HIDDEN)`.

Calling game functions works only inside the game. Build with `--features inproc` and attach on the game thread:

```rust
// SAFETY: inside the game, on the game thread.
let exec = unsafe { sdk::attach() }?;
// `actor` is a `Ptr<sdk::engine::AActor>`, for example from `objects_of` or `cast`.
sdk::engine::AActor::k2_destroy_actor(actor, &exec)?;
```

Before each call, the executor checks three things: the object is still alive, the path names a `UFunction`, and the parameter block is large enough. The DLL scaffold's worker thread is not the game thread. To get there, hook `ProcessEvent`:

```rust
// SAFETY: inside the game; the DLL stays loaded.
let game = unsafe { sdk::game_thread() }?;
// Run on the game thread and wait for the result.
let world = game.call(Duration::from_secs(5), |ctx| {
    ctx.session().gworld().ok().flatten()
})?;
// Intercept a game function: read or change its parameters, or skip it.
// The function is named by its generated parameter struct, so a typo does not compile,
// and the callback gets the parameter block as that struct.
game.before_fn::<sdk::engine::AActor_ReceiveTick_Params>(|ctx, params| {
    let _actor = ctx.object();
    params.delta_seconds *= 0.5;
})?;
// After the call: the outputs and the return value.
game.after_fn::<sdk::engine::AActor_K2_GetActorLocation_Params>(|_ctx, params| {
    let _location = params.return_value;
})?;
```

Every generated function has such a marker (with the `inproc` feature). If the live parameter block is smaller than the struct, the SDK was generated for another build, and the callback is not called. The untyped form takes a path and raw bytes. Use it when the SDK has no struct for the function:

```rust
game.before("/Script/Engine.Actor:ReceiveTick", |ctx| {
    let _actor = ctx.object();
    let _bytes = ctx.params_mut();
})?;
```

Fields of live objects can be written too, inside the game only (`inproc` feature; a session over `ProcessMemory` has no write path). Do it on the game thread, in `game.run` or a hook callback:

```rust
// Call it on the game thread: from `game.run(|ctx| ...)` or a hook callback.
fn widen_fov(
    session: &rt::Session<rt::mem::SelfMemory>,
    camera: rt::Ptr<sdk::engine::APlayerCameraManager>,
) -> Result<(), rt::WriteError> {
    let camera = session.live_object(camera);
    camera.set(sdk::engine::APlayerCameraManager::DEFAULT_FOV, 110.0)?;
    camera.modify(sdk::engine::APlayerCameraManager::DEFAULT_FOV, |fov| *fov += 5.0)?;
    camera.set_bit(sdk::engine::AActor::B_HIDDEN, true)?;
    Ok(())
}
```

With the `accessors` feature, the same writes read as setters, and a chain can lead to the object to write to. A pointer field returns a `LiveObject` on its target, and each write is checked on the object it reaches:

```rust
use sdk::engine::*; // the `...LiveFields` traits come with the `inproc` feature

fn heal(
    session: &rt::Session<rt::mem::SelfMemory>,
    pawn: rt::Ptr<sdk::engine::APawn>,
) -> Result<(), rt::ChainError> {
    session.live_object(pawn).set_health(100.0)?;
    session.live_object(pawn).controller()?.set_b_hidden(false)?;
    Ok(())
}
```

`ChainError` converts from `WriteError`, so one error type covers a chain with writes. Only fields that hold plain data have a setter (numbers, booleans, enums, names, pointers and structs made of those). A field that owns engine memory (a string, array, map, set, text or delegate) has none: write it through `set` with the field constant if you must. Reads of a live object go through `live.read_only()`.

Before each write the object must still be in the object array slot its index names, the field must lie inside the `PropertiesSize` of the object's own class, and the page must be committed read-write (page protection is never changed). Any failure is a `WriteError` and nothing is written. `modify` and `set_bit` read, change and write back, so they are not atomic against the game: do them on the game thread, because a write from another thread races with the engine.

Strings and names work the way they do in Dumper-7 SDKs:

```rust
// An input string or array may point at Rust memory: ProcessEvent never frees inputs.
let text = rt::FStringBuf::new("Hello");
// Static functions run on the class default object, found by the executor.
let name = sdk::engine::UKismetStringLibrary::conv_string_to_name(ctx.executor(), text.as_fstring())?;
println!("{}", ctx.session().fname_text(&name)?);
// Returned strings and arrays are read through the session.
let label = sdk::engine::UKismetSystemLibrary::get_display_name(ctx.executor(), object)?;
println!("{}", ctx.session().fstring(&label)?);
```

Strings and arrays the engine returns are allocated by the engine. Free them through it with `exec.take_string(&mut s)` (read, then free), `take_strings`, `free_string` and `free_array`. These hand the buffer back through `KismetArrayLibrary::Array_Clear`. `free_array` frees only the array's own buffer: free what the elements own first. Never pass `FStringBuf` or `TArrayBuf` as an out parameter.

By default the hook changes only data pointers. It patches the `ProcessEvent` slot in the virtual tables of live native classes (see [Which mode?](#which-mode) for the alternative). Calls from other threads pass straight through. The game thread is chosen in this order:

1. a thread named with `game.set_game_thread(id)`;
2. the process main thread;
3. the first thread to make 64 hooked calls.

Tasks and callbacks get an executor, and calls made through that executor do not trigger callbacks again. A panic in user code is caught.

A few more parts of the hook:

- **`before_native` and `after_native`** hook the `UFunction::Func` pointer. Blueprint code reaches native functions through it without `ProcessEvent`. `before_native_fn::<Marker>` and `after_native_fn::<Marker>` name the function by type and accept only functions the dump marks native; a script function does not compile.
- **`hook::<Marker>(When::Before | When::After, callback)`** and **`hook_native::<Marker>(When, callback)`** pick the mechanism from the marker, so you need not know which engine path a function takes. A script function (`Kind = Script`) goes through `ProcessEvent` and the callback gets `(&mut CallContext, &mut Params)`. A native function (`Kind = Native`) goes through `UFunction::Func` and the callback gets `&mut NativeContext`. A native function is registered on `Func` alone: every invocation passes through it, including the ones `ProcessEvent` makes, so it fires once per call, whether Blueprint bytecode or `ProcessEvent` made it. Registering it on `ProcessEvent` as well would fire twice for the calls that come through there. Two methods, not one, because the callbacks have different types and the compiler cannot choose a closure signature from the marker; a function of the wrong kind does not compile, and the error names the other method. Both return the usual `HookId`.
- **`start_rescan_on_module_load()`** (Windows) hooks the virtual tables of native classes that a module loaded later adds. The loader tells the hook when a DLL loads; a background thread then rescans one and five seconds later, and loads close together share their rescans. Nothing runs between loads. **`start_auto_rescan(interval)`** does the same on a timer: use it on other platforms, or as well when a class can get its first instance with no module load.
- **`into_guard(game)`** turns the id a hook returns into a `HookGuard` that removes the callback when it drops; `forget()` keeps it. Every callback also records the module that holds its code, so `remove_owned_by(module_base)` and `remove_owned_by_current_module()` remove everything one module registered before it is unloaded.
- **`uninstall()`** puts the slots back. `uninstall_and_wait(timeout)` also waits until no thread is inside a hook, so the module can be unloaded.
- **Several mods hooking the same slots.** Two mods that each link this runtime can hook one virtual table slot, the second saving the first one's hook as its original. The first then cannot restore the slot, and unmapping it would crash the game. `uninstall` leaves such a slot alone and returns `HookError::Chained`; the hook keeps forwarding, and `hot-unload` keeps the DLL loaded. Call `uninstall` again once the other mod has removed its hook.

### Which mode?

`GameThread::install` is `install_with(desc, HookMode::Vtable)`: it swaps the `ProcessEvent` slot in the virtual tables it finds through live objects and patches no code. A class with no live object yet, and a class from a module loaded later, are hooked by a rescan. With `--features detour` (Windows x86-64) you can instead detour the engine's `ProcessEvent` itself, once:

```rust
// SAFETY: inside the game; the majority `ProcessEvent` slot value is the engine's implementation.
let game = unsafe { GameThread::install_with(&sdk::ENGINE, HookMode::Detour) }?;
// `sdk::game_thread()` and `GameThread::get()` return this hook afterwards.
```

| | `HookMode::Vtable` (default) | `HookMode::Detour` |
|---|---|---|
| Patches | data pointers in virtual tables | the first bytes of the engine's `ProcessEvent` |
| Class with no live object yet | missed until a rescan | covered |
| Class that overrides `ProcessEvent` | missed | covered when it calls the base implementation |
| `rescan`, `start_auto_rescan`, `start_rescan_on_module_load` | needed | no-ops (`Ok(0)`, `Ok(false)`) |
| Needs | nothing | the `detour` feature, Windows x86-64 (`HookError::DetourUnavailable` otherwise) |

Callbacks, tasks, game thread choice, guards and `remove_owned_by` work the same in both modes, and native `Func` hooks are unaffected. The detour mode has no author signature to check the target against: it takes the address the vote of virtual tables found and checks that against its own current bytes. The detour analysis still refuses a function it cannot move (a `ret`, a trap, a branch into the stolen bytes), but it cannot notice that the vote elected the wrong function, or that another tool patched the bytes first. A class whose override never calls the base implementation is not seen. `uninstall_and_wait` restores the bytes and waits for threads inside the hook and inside the detour; bytes another tool patched over are left alone and reported as `HookError::Chained`, and the DLL must stay loaded. An uninstalled detour is not installed again.

### Which hook do I use?

| I want to intercept | Use | The callback gets |
|---|---|---|
| a script function (an event a Blueprint implements) | `hook::<Marker>(When, ...)` | `&mut CallContext` and the typed parameters; may change them and skip the function |
| a native function that has a `UFunction` (`FUNC_Native`) | `hook_native::<Marker>(When, ...)` | `&mut NativeContext`: object, function, `FFrame`, result; observes only |
| a reflected function by path, or one with no marker | `before` / `after` (`ProcessEvent`), `before_native` / `after_native` (`Func`) | raw parameter bytes, or the same `NativeContext` |
| every `ProcessEvent` call, classes with no object yet included | `install_with(desc, HookMode::Detour)` | widens what the rows above see |
| a plain C++ function with no `UFunction` | `Detour::install` with a signature (below) | whatever your replacement takes |

A direct C++ call of a native function's implementation, bypassing the `UFunction`, is seen by neither `ProcessEvent` nor `Func`; that needs a `Detour`.

### Hook any native function

Many game functions are plain C++ with no `UFunction`, so `ProcessEvent` never sees them. Build with `--features detour` (Windows x86-64 only) to patch one inline. `Detour::install` refuses unless the bytes at the address match the signature you give, so a game update is an error, not a crash:

```rust
use ufathom_runtime::detour::{Detour, Signature, find_in_module};
static ORIGINAL: AtomicUsize = AtomicUsize::new(0);
ufathom_runtime::detour_fn! {
    fn on_score(this: *mut c_void, points: i32) -> i32 {
        // SAFETY: `ORIGINAL` holds the trampoline, same signature as the target.
        let original: extern "system" fn(*mut c_void, i32) -> i32 = unsafe { std::mem::transmute(ORIGINAL.load(Ordering::Acquire)) };
        original(this, points * 2)
    }
}
let signature = Signature::parse("48 89 5C 24 ?? 57 48 83 EC 20")?; // must also cover the bytes the patch overwrites
let target = find_in_module(None, &signature)?; // exactly one match, or an error
// SAFETY: `target` is the function, `on_score` has its signature and stays loaded.
let detour = unsafe { Detour::install_with_original(target, &signature, on_score as *const (), &ORIGINAL) }?;
// Later: restore the bytes and free the memory once no thread is inside.
detour.uninstall_and_wait(Duration::from_secs(5))?;
```

The start of the function is decoded with `iced-x86`. The detour is refused when it contains a `ret`, an `int3`, or a branch into the bytes it would overwrite; RIP-relative operands and relative branches are moved into a trampoline within 2 GB of the target. The patch is written with all other threads suspended. A panic in `detour_fn!` is caught and returns `Default::default()` (or the `on_panic = ...;` value). With `hot-unload`, the DLL removes every detour that is still installed and stays loaded if a thread is still inside one. `detour` is the only part of ufathom that patches code; the dump never uses it.

Status: tested on hand-written machine code in the test suite; not yet run in a real game.

The file and target names above use the default `ufathom-sdk` crate name. If you customize it, replace hyphens with underscores in the DLL file name and append `-external` to the package name for the binary target.

## Documents

- [docs/ru/README.md](docs/ru/README.md): user guide in Russian (руководство на русском)
- [ARCHITECTURE.md](ARCHITECTURE.md): crates, contracts, the discovery algorithm
- [RESEARCH.md](RESEARCH.md): analysis of existing tools
- [IDEAS.md](IDEAS.md): ranked features
- [ROADMAP.md](ROADMAP.md): milestones and completion criteria
- [COMPATIBILITY.md](COMPATIBILITY.md): what is verified
- [CONTRIBUTING.md](CONTRIBUTING.md): workflow and code style

## Responsible use

For research, modding and interoperability on software you are entitled to analyze. ufathom ships no injector, does not bypass anti-cheat and does not hide from detection. Do not load it into a game protected by anti-cheat or connected to online services whose rules forbid it. You are responsible for following the terms that apply to you.

Generated SDKs and dumps may contain data derived from the game. Do not redistribute them without the right to do so.

## Contributors

- [Dopaminez](https://github.com/Bezdarnost01)
- [Claude](https://claude.com/claude-code)

## License

Dual licensed under [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE), at your option.
