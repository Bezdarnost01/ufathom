# Research

Analysis of existing Unreal Engine dumpers, done in November 2026 by reading source code and issue trackers. Scope notes say how deep each project was read.

## Summary

Dumper-7 is the tool to beat: it works on far more games than anything else, and its anchor-based offset search is a sound idea. Its weak points come from how it reads memory and from what it assumes. It dereferences target pointers directly and calls game functions while dumping, so a wrong assumption crashes the game. It also relies on layout tables that grow with each unusual game.

ufathom's main product is a DLL that keeps the in-process convenience but reads only through checked, non-faulting reads and solves layouts from invariants. Among the projects found, none combines that with a Rust SDK. The search was not exhaustive. Matching Dumper-7's coverage can only come from testing on real games over time.

| Project | Mode | Finding offsets | Output | Status | License |
|---|---|---|---|---|---|
| [Dumper-7](https://github.com/Encryqed/Dumper-7) | Injected DLL | Automatic, anchors plus layout tables | C++ SDK, usmap, IDA, Dumpspace | Active | None found, so not reusable |
| [jmap](https://github.com/trumank/jmap) + [patternsleuth](https://github.com/trumank/patternsleuth) | External, PID or minidump | Signatures, per-version type descriptions | JSON, usmap | Active | jmap MIT. patternsleuth declares MIT OR Apache-2.0 in Cargo, no license file |
| [unreal](https://github.com/ItsEthra/unreal) | External | Manual addresses and TOML, 4.25+ | Rust SDK | Inactive since 2023 | MIT |
| [UEDumper](https://github.com/Spuckwaffel/UEDumper) | External with GUI | Manual offsets and macros | C++ SDK | Last push April 2026 | MIT |
| [UEFind](https://github.com/Fyyre/UEFind) | External | Automatic, strings plus layout list | C++ SDK, tools | Young, few users | MIT |
| [UnrealDumper-4.25](https://github.com/guttir14/UnrealDumper-4.25) | External | Per-game table, 4.23 to 4.27 | Dump | Inactive since 2022 | None found |
| [RE-UE4SS](https://github.com/UE4SS-RE/RE-UE4SS) | Injected | Signatures and config | C++ headers, Lua API | Active | MIT |

Depth: Dumper-7's engine core, `jmap` type descriptions and main loop, patternsleuth resolvers for objects and names, `unreal` engine and start of codegen, UEFind object and name finders, and the UnrealDumper table were read directly. UE4SS and UEVR were not read beyond metadata.

## Dumper-7

Offset discovery is anchored on known engine objects, and that idea is sound: `Struct.super == Field`, `Guid` size is 0x10, and so on. The limits come from running inside the game.

- `ObjectArray::Init` scans `.data` against a table of `FChunkedFixedUObjectArray` layouts. The table now holds default, UE 5.8 dev, Back4Blood, MultiVersus, MindsEye and a UE 5.7 variant. Each new reordering needs another entry. Hard limits like `NumChunks <= 0x14` are also arbitrary.
- Name resolution calls the game's own `FName::AppendString`. A wrong address or changed layout crashes the game ([#544](https://github.com/Encryqed/Dumper-7/issues/544): UE 5.8 moved the `FFieldClass` name, causing an access violation).
- `OverwriteIfInvalidOffset` falls back to default offsets when a search fails, so a wrong result can continue silently.
- Discovery can pick a wrong address with no warning ([#529](https://github.com/Encryqed/Dumper-7/issues/529): wrong GNames on Marvel Rivals).
- Crash reports are common ([#507](https://github.com/Encryqed/Dumper-7/issues/507), [#509](https://github.com/Encryqed/Dumper-7/issues/509), [#477](https://github.com/Encryqed/Dumper-7/issues/477)).
- Generated C++ does not always compile ([#562](https://github.com/Encryqed/Dumper-7/issues/562), [#586](https://github.com/Encryqed/Dumper-7/issues/586)).
- `ProcessEvent` is called only to learn `FText` internals. A dump needs only the size, which reflection provides.

Worth keeping as ideas: anchors, the ProcessEvent vtable scan by `FunctionFlags` test encodings, usmap output, and user-supplied pointer decoding.

## jmap and patternsleuth

The closest prior art: Rust, external, supports minidumps. Its `.gs` type descriptions are the best public record of how engine layouts change per version, and were used as a reference for test fixtures.

- It needs the engine version to pick layouts, and build flags such as `case_preserving` change them. Failing version detection stops the run ([#26](https://github.com/trumank/jmap/issues/26)).
- Addresses depend on signatures. Failures need manual environment variables.
- On UE 5.8, addresses resolved yet 0 objects were found ([#30](https://github.com/trumank/jmap/issues/30)). There is a panic on an unknown enum ([#32](https://github.com/trumank/jmap/issues/32)) and an error on non-UTF-8 names ([#22](https://github.com/trumank/jmap/issues/22)).
- No Rust SDK output.

## ItsEthra/unreal

Shows that a Rust SDK from an external dump works. Its limits: manual addresses, a fixed 64K chunk size and 16-bit block offset, TOML offsets per version, only 4.25+, and `unwrap` while walking remote chains. It splits packages into crates, which forces it to merge crates to remove dependency cycles. A single crate with modules avoids that problem entirely.

## UEFind

Closest to the external auto-discovery idea. It scores a fixed list of array layouts and uses `ByteProperty` string search for names. ufathom generalizes the object array step by solving field order instead of listing layouts.

## Common user problems

| Problem | Evidence | ufathom response |
|---|---|---|
| Layout change breaks a dumper | D7 #544, jmap #30 | Solve offsets from invariants |
| Wrong address accepted silently | D7 #529, #582 | Cross-validation and refusal with named invariant |
| Dump failure crashes the game | D7 #507, #509, #477 | The DLL reads only through non-faulting calls and never calls game code |
| Version detection is mandatory | jmap #26, #16 | Version is a hint with evidence |
| Injection is blocked or risky | D7 #593, #574 (titles) | Optional external mode. No bypass |
| SDK does not compile | D7 #562, #586 | Layout planner degrades to bytes, compile test in CI |
| Non-UTF-8 names and unknown enums crash the tool | jmap #22, #32 | Lossy decoding, diagnostics, no panics |
| Only loaded classes are dumped | D7 #542 | Documented limit. An opt-in asset loader is possible only in DLL mode, later |

## Requirements from real use

A patched Dumper-7 in real modding use had to add these. They are direct requirements for the DLL:

- Repeat dumps in one process, with state reset between runs.
- A separate folder per session and per dump, with nothing overwritten.
- Choosing output formats per dump.
- An identity record: executable path and hash, PID, engine version, settings, found offsets.
- A command channel through files, with IDs, atomic results and an archive.
- Hotkeys that act only while the game window is active.
- Long path support, after a real failure on a long Steam install path.
- Clean unload: stop, wait for work to finish, then unload.
- No heavy work on load unless asked.
- ABI-aware comparison between versions, where a renamed parameter is not a change and ambiguous duplicate names are flagged.

That fork also generated on the game thread, blocking it for seconds, and one of its SDKs carried a duplicate `CoreUObject.Struct` declaration. Its output for a UE 4.22 build records `GWorld` and `GNames` as 0.

## Findings from real builds

Static inspection of real shipping executables and Dumper-7 output for two of them showed real builds are not stock, which supports the solver approach:

- The UE 4.22 and 4.25 builds use a **12-byte `FName`** (comparison, display, number) in shipping builds, shifting every later offset by 8.
- The UE 4.25 build uses `FNamePool` with stride 4, entry header at +4, chars at +6, and length as `header >> 1`, unlike the common stride 2 and `>> 6`.

Full table: `ARCHITECTURE.md`, "Calibration data from real builds".

Dumper-7 output for UE 4.21 and 4.22 builds and a usmap for a UE5 build exist locally, which makes real differential testing possible without trusting ufathom's own logic. A local-only test harness uses them; they are never committed.

## Name

Requirements: short, memorable, technical, not derived from Dumper-7. Checked on crates.io and by exact-name GitHub search in November 2026.

| Name | crates.io | GitHub exact | Comment |
|---|---|---|---|
| **ufathom** | free | none | Chosen |
| ueprism | free | none | Reflection pun, slightly narrow |
| ustrata | free | none | Good, less obvious meaning |
| uelens | free | none | Generic |
| uesift | free | none | Suggests filtering only |
| ueforge | free | none | Overused suffix |
| uecarve | free | none | Unclear |
| uetome | free | none | Suggests documents |
| ueatlas | free | none | Suggests maps |
| reflectum | free | none | Long |
| uscope | taken | 15 repos | Taken |
| kismet | taken | many | Taken, and conflicts with UE and a network tool |
| ghostwalk | free | 10 repos | Crowded, wrong tone |

**ufathom** combines the `U` of `UObject` with *fathom*: to measure depth and to understand fully. That is what the tool does to an engine: it measures offsets rather than assuming them. It avoids Epic's trademarks in the name.
