# Ideas

Ranked by value for the first goal, a DLL that dumps DBD 2.6.4, 3.7.2, 4.4.2 and 8.1.0 better than Dumper-7's, and by how much they set ufathom apart. Complexity is relative to this project.

## 1. A DLL that cannot crash the game through a bad read

- **Gives:** in-process convenience with no access violations. All reads use `ReadProcessMemory` on the current process.
- **Solves:** the most common Dumper-7 report: crashes in `GetClass`, `GetSize` and property walks.
- **Complexity:** medium. Every read goes through a fallible API, and name decoding is fully reimplemented instead of calling `AppendString`.
- **Architecture:** `SelfMemory` is one more `MemoryReader`. The pipeline has no raw pointers to target memory.
- **Better because:** Dumper-7 dereferences raw pointers and calls game functions. A wrong offset here is an error message.

## 2. Layout solver from invariants

- **Gives:** offsets for a modified engine with no table entry.
- **Solves:** per-game layout lists and version-locked layouts. Directly handles the 12-byte `FName` and non-standard pool entries in the target builds.
- **Complexity:** high. This is the core of the project.
- **Architecture:** the solver produces `EngineLayout`. Everything else only reads that struct.
- **Better because:** Dumper-7 needs a new entry for each reordered array. jmap needs the version and build flags up front.

## 3. Evidence and refusal

- **Gives:** each solved offset lists its method and sample counts. Contradictions stop the run and name the invariant.
- **Solves:** silently wrong dumps, and bug reports with no useful detail.
- **Complexity:** low, if built in from the start.
- **Architecture:** `Discovery.evidence`, `identity.json`, `ufathom doctor`.
- **Better because:** Dumper-7 can fall back to default offsets and carry on.

## 4. Non-blocking, repeatable sessions

- **Gives:** dump as often as needed during play, without reloading or stalling the game. Clean unload, separate output folders, an identity record.
- **Solves:** the workarounds a patched Dumper-7 needed in real use.
- **Complexity:** medium. Needs tolerance for objects changing during a read, and strict thread lifecycle.
- **Architecture:** `ufathom-agent` holds session logic. Own thread pool, re-validation after parsing.
- **Better because:** those requirements are designed in rather than patched on.

## 5. Rust SDK that always compiles

- **Gives:** exact layout checked at compile time. Sound reads of arbitrary memory.
- **Solves:** SDK compile failures, and undefined behaviour from reading unknown enum values.
- **Complexity:** medium to high.
- **Architecture:** planning pass with byte fallback. `Pod` everywhere. One crate with feature-gated modules.
- **Better because:** package cycle handling disappears, and enums and booleans are modeled correctly for untrusted memory.

## 6. Oracle comparison

- **Gives:** a real correctness check against existing usmap or Dumper-7 output.
- **Solves:** trusting a tool's own logic to prove itself.
- **Complexity:** low to medium.
- **Architecture:** `ufathom-usmap`, `check-oracle` command.
- **Better because:** no compared tool turns differential checks into a routine part of validation.

## 7. ABI-aware diff between game versions

- **Gives:** a report of moved fields, resized types, changed enums and function ABI changes.
- **Solves:** manual porting work between DBD versions, which the target use case does constantly.
- **Complexity:** low to medium.
- **Architecture:** `ufathom_model::diff` on the JSON IR.
- **Better because:** it compares stable data rather than generated source text, and ignores renames that do not affect ABI.

## 8. Type-level split between reading and calling

- **Gives:** an external session cannot call functions; it fails at compile time. In-process calls go through one checked path.
- **Solves:** promises of remote ProcessEvent that cannot be kept, and stale pointer calls.
- **Complexity:** low.
- **Architecture:** `Executor` trait, `inproc` feature.
- **Better because:** the limit is visible in the API rather than in a README.

## 9. External and offline modes from the same pipeline

- **Gives:** a dump with no code in the target, or from a minidump. Useful when loading a DLL is not possible, and for reproducing problems.
- **Solves:** having only one way in.
- **Complexity:** low, since it is the same code with another reader.
- **Architecture:** `ProcessMemory`, `MinidumpMemory`.
- **Better because:** Dumper-7 cannot do this. jmap can, but has no SDK.

## 10. Version as a range with evidence

- **Gives:** useful results when version strings are missing. 8.1.0 shows no plain version string.
- **Solves:** jmap's hard stop when version detection fails.
- **Complexity:** low.
- **Architecture:** `EngineInfo { claimed, range, evidence }`.
- **Better because:** absence is not fatal.

## 11. Opt-in asset loading before a dump

- **Gives:** Blueprint classes that are not loaded yet.
- **Solves:** incomplete SDKs ([Dumper-7 #542](https://github.com/Encryqed/Dumper-7/issues/542), an open request).
- **Complexity:** high and game-specific. Needs game thread calls.
- **Architecture:** a separate DLL-only feature outside the dump path. It breaks the "no game calls" rule, so it stays explicit and off by default.
- **Better because:** nobody has a stable general version. High value, but only after the core works.

## 12. Incremental SDK output

- **Gives:** only changed package files are rewritten, so Cargo rebuilds less.
- **Solves:** slow rebuilds on large SDKs.
- **Complexity:** low.
- **Architecture:** content hashes in the generator.
- **Better because:** a small gain, worth it once the generator is stable.

## Considered and deferred

| Idea | Decision |
|---|---|
| C++ SDK output | Deferred, open question. Existing C++ mods use Dumper-7's SDK, so replacing it fully would need this |
| Dynamic plugin loader | Rejected. No stable Rust ABI. JSON and traits cover the need |
| Bundled decryption keys | Rejected. Transforms are user-supplied, as in Dumper-7 |
| Bundled injector | Rejected. Loading is the user's choice |
| 32-bit targets | Deferred. Not needed for the current games |
| Signature database | Deferred. Added only if the data-first path fails on a real game |
| GUI | Deferred. Files and CLI come first |
