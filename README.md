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

### Build the generated application

Every SDK includes an editable `src/main.rs`. Put your logic in its `run` function and import generated types through the SDK crate, for example `ufathom_sdk::core_uobject::UObject`. Later SDK generation preserves this file; other generated files may be refreshed. The SDK also gets a `.cargo/config.toml` that links the C runtime statically on `x86_64-pc-windows-msvc`, so the built DLL and executable run on machines without the Visual C++ Redistributable.

**Windows DLL (the default library configuration):**

```bat
cd my_sdk
cargo build --release --lib
```

The result is `target/release/ufathom_sdk.dll`. Loading it starts `run` once on a worker thread with `SelfMemory`. By default the DLL stays loaded until the game exits. Two features change this:

- `--features hot-unload`: when `run` returns, the DLL removes its hooks and unloads itself. It stays loaded if a thread is still inside a hook.
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

It also provides `find_object` by path, `find_by_name`, `is_a`, `cast::<T>()`, `default_object::<T>()`, `gengine()` and checked field reads through `session.object(ptr).get(T::FIELD)`.

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
game.before("/Script/Engine.Actor:ReceiveTick", |ctx| {
    let _actor = ctx.object();
})?;
```

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

The hook changes only data pointers. It patches the `ProcessEvent` slot in the virtual tables of live native classes. Calls from other threads pass straight through. The game thread is chosen in this order:

1. a thread named with `game.set_game_thread(id)`;
2. the process main thread;
3. the first thread to make 64 hooked calls.

Tasks and callbacks get an executor, and calls made through that executor do not trigger callbacks again. A panic in user code is caught.

A few more parts of the hook:

- **`before_native` and `after_native`** hook the `UFunction::Func` pointer. Blueprint code reaches native functions through it without `ProcessEvent`.
- **`start_auto_rescan(interval)`** hooks the virtual tables of classes that get their first instance later.
- **`uninstall()`** puts the slots back. `uninstall_and_wait(timeout)` also waits until no thread is inside a hook, so the module can be unloaded.

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
