# ufathom

An Unreal Engine dumper DLL and Rust SDK generator, built to survive what breaks Dumper-7's DLL. Load it in a game and it writes a reflection dump, a usmap and a Rust SDK. If your offsets are wrong, you get an error message, not a crashed game.

**Status: pre-release.** `v0.1.1` is available, with real-build results for the UE 4.25 editor and a UE 4.21 shipping build. See [COMPATIBILITY.md](COMPATIBILITY.md) for exactly what has been tested. The target range is UE 4.11 to 5.8, and support is claimed version by version as real builds are verified.

## Goals

- **A bad read cannot crash the game.** The dump path never dereferences a raw game pointer or calls a game function. Every read can fail safely.
- **The game keeps running.** Dumps run on background threads, with no need for the game thread.
- **Offsets are solved, not tabulated.** Layouts come from invariants the engine must satisfy, so a modified engine should not need a table entry.
- **Proof instead of guessing.** Every offset records how it was found. A contradiction stops the run and says why.
- **Dump again without restarting.** Each dump gets its own folder and an identity record, and the DLL unloads cleanly.
- **A Rust SDK built for untrusted memory.** Layout checked at compile time, enums that are sound for any value, and an SDK that always compiles.

The same pipeline also runs **without a DLL**, reading a running process from outside or a minidump offline.

## Usage

**As a DLL.** Put `ufathom.toml` next to the game executable, load `ufathom.dll` with your loader of choice, press F8 to dump and F6 to unload.

```toml
output = "D:/dumps"
formats = ["json", "usmap", "rust-sdk"]
auto_dump = false
```

`cargo build -p ufathom-dll` produces `target/<profile>/ufathom_dll.dll`; copy it next to the game as `ufathom.dll`. The library is not called `ufathom`, so that its debug symbols do not clash with the command line tool's on Windows.

**From outside, and for working with results:**

```
ufathom ps                                         # probable Unreal Engine processes
ufathom inspect-exe Game-Win64-Shipping.exe        # static facts, nothing is run
ufathom doctor --process Game-Win64-Shipping.exe   # how well the layout was solved
ufathom dump --process Game-Win64-Shipping.exe -o dump.json --usmap Mappings.usmap
ufathom gen dump.json -o my_sdk/
ufathom diff old.json new.json
ufathom command <output dir> dump --label menu --wait 120   # drive a loaded DLL
```

A target is `--pid N`, `--process NAME` or `--minidump PATH`, with optional `--module`, `--layout FILE` and `--hint VERSION`. Exit codes: 0 success, 1 failure, 2 usage error, 3 degraded dump. `ufathom --help` has the rest.

By default, `gen` and `dump --sdk` include the matching `ufathom-runtime` and `ufathom-mem` sources and licenses under `vendor/`. The generated folder is self-contained: copy it anywhere and build it without an ufathom checkout or manual manifest edits.

```
cargo check --manifest-path my_sdk/Cargo.toml
```

Use `--runtime-path PATH` or `--runtime-version VERSION` only when you want to override the bundled runtime with a local crate or a registry dependency.

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
