# ufathom

An Unreal Engine dumper DLL and Rust SDK generator, built to survive what breaks Dumper-7's DLL. Load it in a game and it writes a reflection dump, a usmap and a Rust SDK. If your offsets are wrong, you get an error message, not a crashed game.

**Status: pre-release.** `v0.2.1` is available, with real-build results for the UE 4.25 editor and a UE 4.21 shipping build. See [COMPATIBILITY.md](COMPATIBILITY.md) for exactly what has been tested. The target range is UE 4.11 to 5.8, and support is claimed version by version as real builds are verified.

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

Choose a process by its list number or PID. ufathom writes `dump.json`, `Mappings.usmap` and a self-contained Rust SDK into a new folder, without overwriting earlier results:

```
ufathom-dumps/<timestamp>-p<PID>/
├── dump.json
├── Mappings.usmap
└── sdk/
```

Use `ufathom quick --all` if the game is not in the probable-target list, or `ufathom quick --output D:/dumps` to change the parent folder. Empty input cancels before anything is written.

**As a DLL.** Put `ufathom.toml` next to the game executable, load `ufathom.dll` with your loader of choice, press F8 to dump and F6 to unload.

```toml
output = "D:/dumps"
formats = ["json", "usmap", "rust-sdk"]
auto_dump = false
```

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

Every SDK includes an editable `src/main.rs`. Put your logic in its `run` function and import generated types through the SDK crate, for example `ufathom_sdk::core_uobject::UObject`. Later SDK generation preserves this file; other generated files may be refreshed.

**Windows DLL (the default library configuration):**

```bat
cd my_sdk
cargo build --release --lib
```

The result is `target/release/ufathom_sdk.dll`. Loading it starts `run` once on a worker thread with `SelfMemory`. This scaffold is resident and does not implement hot-unload.

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

The hook changes only data pointers. It patches the `ProcessEvent` slot in the virtual tables of live native classes. The thread that first makes 64 hooked calls is taken as the game thread. Tasks and callbacks get an executor, and calls made through that executor do not trigger callbacks again. A panic in user code is caught. `uninstall()` puts the slots back.

The file and target names above use the default `ufathom-sdk` crate name. If you customize it, replace hyphens with underscores in the DLL file name and append `-external` to the package name for the binary target.

## Documents

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
