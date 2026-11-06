# ufathom

An Unreal Engine dumper DLL and Rust SDK generator, built to survive what breaks Dumper-7's DLL. Load it in a game and it writes a reflection dump, a usmap and a Rust SDK. If your offsets are wrong, you get an error message, not a crashed game.

**Status: early development.** Nothing is released and no game is verified yet. See [COMPATIBILITY.md](COMPATIBILITY.md) for what has actually been tested. First targets are specific Dead by Daylight builds on UE 4.21, 4.22, 4.25 and UE5.

## Goals

- **A bad read cannot crash the game.** The dump path never dereferences a raw game pointer or calls a game function. Every read can fail safely.
- **The game keeps running.** Dumps run on background threads, with no need for the game thread.
- **Offsets are solved, not tabulated.** Layouts come from invariants the engine must satisfy, so a modified engine should not need a table entry.
- **Proof instead of guessing.** Every offset records how it was found. A contradiction stops the run and says why.
- **Dump again without restarting.** Each dump gets its own folder and an identity record, and the DLL unloads cleanly.
- **A Rust SDK built for untrusted memory.** Layout checked at compile time, enums that are sound for any value, and an SDK that always compiles.

The same pipeline also runs **without a DLL**, reading a running process from outside or a minidump offline.

## Planned usage

Being implemented. See [ROADMAP.md](ROADMAP.md).

**As a DLL.** Put `ufathom.toml` next to the game executable, load `ufathom.dll` with your loader of choice, press F8 to dump and F6 to unload.

```toml
output = "D:/dumps"
formats = ["json", "usmap", "rust-sdk"]
auto_dump = false
```

**From outside, and for working with results:**

```
ufathom dump --process Game-Win64-Shipping.exe -o dump.json
ufathom gen dump.json -o my_sdk/
ufathom diff old.json new.json
```

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
