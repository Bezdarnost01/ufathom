# Roadmap

First goal: a DLL that dumps DBD 2.6.4, 3.7.2, 4.4.2 and 8.1.0 more reliably than Dumper-7's and generates a Rust SDK. External mode comes from the same code. Wider engine support follows after that.

Each milestone lands through feature branches and pull requests. A milestone is done only when its criteria were actually run.

## M0: Foundation (stages 1 to 4)

Research, naming, architecture, workspace, memory reading.

- [x] `RESEARCH.md`, `IDEAS.md`, `ARCHITECTURE.md`, `CONTRIBUTING.md`
- [ ] Cargo workspace with all crates, shared lints and CI on Windows and Linux
- [ ] `ufathom-mem`: `Snapshot`, `PageCache`, `SelfMemory`, `ProcessMemory`, `MinidumpMemory`, `list_processes`
- [ ] `ufathom-model`: `EngineLayout`, `Dump`, `diff`

**Done when:** fmt, clippy and tests pass in CI. `SelfMemory` returns errors for unmapped and guarded addresses without faulting. `ProcessMemory` reads a child process in a test. A minidump written by the OS for a child process is parsed in a test on Windows.

## M1: Engine layer and fixtures (stage 5)

- [ ] `ufathom-testkit`: images matching each calibration column, shuffled layouts, corruption injection, a fake PE module, a `fake-host` process
- [ ] `ufathom-engine`: names (array and pool), objects (fixed and chunked), legacy and modern properties

**Done when:** views read every target-shaped image correctly and every corruption mode returns errors with no panic.

## M2: Discovery (stage 7)

- [ ] PE parsing and version detection
- [ ] Object array, name table and layout solvers
- [ ] Evidence, invariants, overrides, globals, `pointer_transform`

**Done when:** `discover` recovers the exact `EngineLayout` on each target-shaped image and on shuffled ones with no hints. Wrong overrides are rejected. `inspect-exe` reports the right version for the three real executables that carry one.

## M3: Reflection and the DLL (stages 6, 10)

- [ ] `ufathom-reflect`, with re-validation for objects changing during a read
- [ ] `ufathom-usmap` with `compare`
- [ ] `ufathom-agent`: config, sessions, identity, commands, long paths
- [ ] `ufathom-dll`: lifecycle, containment, hotkeys, double-load guard
- [ ] CLI: `ps`, `inspect-exe`, `probe`, `doctor`, `dump`, `status`, `command`, `check-oracle`, `usmap`

**Done when:** on `fake-host`, the real DLL dumps a synthetic image matching ground truth, dumps again, unloads with no threads left, survives corrupted images with the host still running, and works from a path over 260 characters. Host stall during a dump is measured and reported. The usmap reader parses the real 3.7.2 and 8.1.0 files locally.

## M4: Rust SDK and runtime (stages 8, 9)

- [ ] `ufathom-codegen` with golden and compile tests
- [ ] `ufathom-runtime`: containers, `Session`, `ObjectRef`, `Executor`, `inproc`
- [ ] DLL and CLI: SDK output

**Done when:** the SDK generated from each fixture passes `cargo check`, and a test reads a synthetic object through the generated types. In-process call mechanics are tested against a fake object with a fake vtable.

## M5: Real game validation (stage 11)

Requires the user to start each game and to load the DLL or allow attaching.

- [ ] 2.6.4: DLL dump, compared with the existing Dumper-7 SDK
- [ ] 3.7.2: DLL dump, `check-oracle` against the Dumper-7 usmap, `GWorld` and name table found
- [ ] 4.4.2: DLL dump, checked against known layout values
- [ ] 8.1.0: see the risk below
- [ ] External dump for at least one game
- [ ] Generated SDK for each game compiles
- [ ] Each "better than Dumper-7" criterion from `ARCHITECTURE.md` has a recorded result

**Done when:** results are recorded in `COMPATIBILITY.md`, including anything that failed.

## M6: Polish and wider support (stage 12)

- [ ] `diff` command, documentation, performance numbers measured on real dumps
- [ ] Rust mod toolkit, so new mods need no C++: ProcessEvent and native function hooks, proxy DLL exports, game thread dispatch, engine allocator
- [ ] Opt-in asset loading in DLL mode
- [ ] Generic UE fixtures from public type descriptions
- [ ] Other engine versions, added as each is verified on a real game

## Risks and external dependencies

- **DBD 8.1.0 live.** This copy may not run offline, and a live client has anti-cheat. **The DLL must not be loaded into a client with active anti-cheat.** Testing it needs an offline setup and a deliberate decision by the user. Without it, 8.1.0 stays at static and usmap schema level, and its memory layout stays unverified.
- **Loaded classes only.** A dump sees objects loaded at that moment, so results depend on the game state.
- **Objects changing during a dump.** The game keeps running. Re-validation catches most races, and what it cannot resolve becomes a diagnostic.
- **Protected targets.** If loading or `OpenProcess` is denied, the tool reports it where it can. Bypassing that is a non-goal.
