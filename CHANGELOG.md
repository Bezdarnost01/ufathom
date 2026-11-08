# Changelog

All notable changes are recorded here, in the [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) format. The project follows [Semantic Versioning](https://semver.org/); releases before 1.0.0 may change APIs and output formats between minor versions.

## [Unreleased]

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

[Unreleased]: https://github.com/Bezdarnost01/ufathom/compare/v0.1.1...HEAD
[0.1.1]: https://github.com/Bezdarnost01/ufathom/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/Bezdarnost01/ufathom/releases/tag/v0.1.0
