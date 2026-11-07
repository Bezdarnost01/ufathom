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
| UE 4.25 to 4.27 | Chunked | `FNamePool` | `FProperty` | discovery on a 4.25 editor |
| UE 5.0 to 5.8 | Chunked, reordered in 5.8 | `FNamePool` | `FProperty` | static on 5.4 |

`static` above means only that `inspect-exe` read the right engine version from a real executable. No dump has run on a real build yet.

**Discovery on UE 4.25.** A stock UE 4.25 editor from an engine install, a modular build with editor-only data, was probed read-only from outside. With no hints and no module option, `discover` found the object array and the name pool in two different DLLs and solved the layout in about 2 seconds. Independent check against public per-version type descriptions, for an editor build: `SuperStruct` 0x38 (no `FStructBaseChain` in editor builds), `FProperty::ArrayDim` 0x40 (editor-only `FField` metadata pointer), `Offset_Internal` 0x54, `sizeof(FProperty)` 0x88, and a 12-byte `FName`. All agree. This is not yet a dump and not a shipping build.

Two more UE5 executables claim 5.2.1 and 5.7.4 through the fixed version field alone, with no other evidence, so those versions are not listed as confirmed.

Real builds of UE 4.13, 4.21, 4.22, 4.25 and UE5 are available for testing. Other versions need a real build before they can move past `synthetic`.

## "Better than Dumper-7" criteria

Defined in `ARCHITECTURE.md`. None has a result yet.

## Known limits

- x86-64 Windows for the DLL. External reader also targets Linux, not tested on real games.
- A dump contains only objects loaded when it ran.
- Obfuscated object arrays need a user-supplied transform.
- Rust SDK only. C++ output is not planned.
