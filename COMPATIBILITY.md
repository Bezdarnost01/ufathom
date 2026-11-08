# Compatibility

An entry changes only when a check was actually run and recorded here. Synthetic tests do not count as support.

## Evidence levels

| Level | Meaning |
|---|---|
| none | Not tested |
| synthetic | Passes on a generated image of that era. Proves logic only, not compatibility |
| static | Real executable inspected without running it |
| oracle | Dump compared with an independent source for the same build |
| discovery | `discover` solved the layout of a running build through read-only external reads, checked against an independent source |
| live | Dump of the running game succeeded |

## Engine versions

| Engine | Object array | Names | Properties | Best evidence so far |
|---|---|---|---|---|
| UE 4.11 to 4.20 | Fixed up to 4.19 or 4.20 | `TNameEntryArray` | `UProperty` | static on 4.13 |
| UE 4.21 | Chunked | `TNameEntryArray` | `UProperty` | static |
| UE 4.22 | Chunked | `TNameEntryArray` | `UProperty` | static |
| UE 4.23, 4.24 | Chunked | `FNamePool` | `UProperty` | none |
| UE 4.25 to 4.27 | Chunked | `FNamePool` | `FProperty` | live DLL dump on a 4.25 editor, checked against engine source |
| UE 5.0 to 5.8 | Chunked, reordered in 5.8 | `FNamePool` | `FProperty` | static on 5.4 |

`static` above means only that `inspect-exe` read the right engine version from a real executable. No dump has run on a real build yet.

**Discovery on UE 4.25.** A stock UE 4.25 editor from an engine install, a modular build with editor-only data, was probed read-only from outside. With no hints and no module option, `discover` found the object array and the name pool in two different DLLs and solved the layout in about 2 seconds. Independent check against public per-version type descriptions, for an editor build: `SuperStruct` 0x38 (no `FStructBaseChain` in editor builds), `FProperty::ArrayDim` 0x40 (editor-only `FField` metadata pointer), `Offset_Internal` 0x54, `sizeof(FProperty)` 0x88, and a 12-byte `FName`. All agree. This is not yet a dump and not a shipping build.

**Dump on UE 4.25, checked against engine source.** On the same editor, `build_dump` produced 3,502 classes, 1,950 structs, 1,152 enums and 6,971 functions in about 1 second, with no diagnostics. Checked against the engine's own source headers, which ufathom does not use:
- **Members:** all 267 `UPROPERTY` declarations of `AActor`, `APawn`, `ACharacter`, `AController`, `APlayerController`, `USceneComponent`, `UWorld` and `UGameInstance` appear by name. The one textual difference is expected: the engine drops the `_DEPRECATED` suffix from reflected names.
- **Parents:** every parent matches.
- **Core structs:** `FVector` (0xC, three floats), `FTransform` (0x30, alignment 16, `Rotation`, `Translation`, `Scale3D` at 0, 0x10, 0x20), `FGuid` and the `ENetRole` values match.

This was an external read of an editor build, not the DLL and not a shipping game.

**DLL in UE 4.25.** `ufathom.dll` was loaded into the same kind of editor process, started for testing. It dumped once automatically, then again by command, and unloaded by command.
- **Content:** both in-process dumps were byte-identical and match the external dump on all 5,452 classes and structs and on the layout. The only enum differences come from a later fix to enum width.
- **Oracle:** the same 267 of 267 `UPROPERTY` members match the engine source.
- **Repeat:** the second dump took 1.7 s.
- **Unload:** the module was unmapped, the three threads it started were gone, and the editor kept responding throughout.

Not yet shown on a shipping game.

Two more UE5 executables claim 5.2.1 and 5.7.4 through the fixed version field alone, with no other evidence, so those versions are not listed as confirmed.

Real builds of UE 4.13, 4.21, 4.22, 4.25 and UE5 are available for testing. Other versions need a real build before they can move past `synthetic`.

## "Better than Dumper-7" criteria

Defined in `ARCHITECTURE.md`. Results so far, from the UE 4.25 editor run above, not yet from a shipping game:

| # | Criterion | Result so far |
|---|---|---|
| 1 | A wrong layout leaves the game running | Synthetic only: all 14 corruption modes leave `fake-host` running |
| 2 | The game keeps running during a dump | Editor kept responding. Worst `fake-host` tick gap 6.8 ms against a 5 ms tick |
| 3 | No manual offsets | Met on the editor: no hints, no module option |
| 4 | `GWorld` and the name table found on the measured UE 4.22 build | Not yet run |
| 5 | Dump again, then unload with no threads left | Met on the editor |
| 6 | Own folder and `identity.json` per dump | Met |
| 7 | Long install paths | `fake-host` only |
| 8 | The SDK compiles | Met on the editor dump: 0 errors, 0 warnings |

## Known limits

- x86-64 Windows for the DLL. External reader also targets Linux, not tested on real games.
- A dump contains only objects loaded when it ran.
- Obfuscated object arrays need a user-supplied transform.
- Rust SDK only. C++ output is not planned.
