# Compatibility

An entry changes only when a check was actually run and recorded here. Synthetic tests do not count as support.

## Evidence levels

| Level | Meaning |
|---|---|
| none | Not tested |
| synthetic | Passes on a generated image of that era. Proves logic only, not compatibility |
| static | Real executable inspected without running it |
| oracle | Dump compared with an independent source for the same build |
| live | Dump of the running game succeeded |

## Engine versions

| Engine | Object array | Names | Properties | Best evidence so far |
|---|---|---|---|---|
| UE 4.11 to 4.20 | Fixed up to 4.19 or 4.20 | `TNameEntryArray` | `UProperty` | static on 4.13 |
| UE 4.21 | Chunked | `TNameEntryArray` | `UProperty` | static |
| UE 4.22 | Chunked | `TNameEntryArray` | `UProperty` | static |
| UE 4.23, 4.24 | Chunked | `FNamePool` | `UProperty` | none |
| UE 4.25 to 4.27 | Chunked | `FNamePool` | `FProperty` | static on 4.25 |
| UE 5.0 to 5.8 | Chunked, reordered in 5.8 | `FNamePool` | `FProperty` | static on 5.4 |

`static` above means only that `inspect-exe` read the right engine version from a real executable. No dump has run on a real build yet.

Two more UE5 executables claim 5.2.1 and 5.7.4 through the fixed version field alone, with no other evidence, so those versions are not listed as confirmed.

Real builds of UE 4.13, 4.21, 4.22, 4.25 and UE5 are available for testing. Other versions need a real build before they can move past `synthetic`.

## "Better than Dumper-7" criteria

Defined in `ARCHITECTURE.md`. None has a result yet.

## Known limits

- x86-64 Windows for the DLL. External reader also targets Linux, not tested on real games.
- A dump contains only objects loaded when it ran.
- Obfuscated object arrays need a user-supplied transform.
- Rust SDK only. C++ output is not planned.
