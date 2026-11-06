# Compatibility

An entry changes only when a check was actually run and recorded here. Synthetic tests do not count as support.

## Evidence levels

| Level | Meaning |
|---|---|
| none | Not tested |
| static | Real executable inspected without running it |
| oracle | Dump compared with an independent source for the same build |
| live | Dump of the running game succeeded |

## Target games

| Game | Engine | Static | DLL dump | External dump | Oracle | Rust SDK compiles | Reference |
|---|---|---|---|---|---|---|---|
| DBD 2.6.4 | UE 4.21.1 | none | none | none | none | none | Dumper-7 C++ SDK |
| DBD 3.7.2 | UE 4.22.3 | none | none | none | none | none | Dumper-7 usmap and SDK |
| DBD 4.4.2 | UE 4.25.1 | none | none | none | none | none | Known layout values |
| DBD 8.1.0 | UE5, minor unverified | none | none | none | none | none | usmap, schema only. Live check needs a user decision |

Engine versions above come from file metadata read with a one-off script, not from ufathom.

## "Better than Dumper-7" criteria

Defined in `ARCHITECTURE.md`. None has a result yet.

## Engine generations

| Range | Status |
|---|---|
| UE 4.11 to 4.20 | Not tested. Fixed object array and legacy names are designed for, no real target yet |
| UE 4.21 to 4.24 | Planned through DBD 2.6.4 and 3.7.2 |
| UE 4.25 to 4.27 | Planned through DBD 4.4.2 |
| UE 5.x | Planned through DBD 8.1.0, memory layout unverified |

## Known limits

- x86-64 Windows for the DLL. External reader also targets Linux, not tested on real games.
- A dump contains only objects loaded when it ran.
- Obfuscated object arrays need a user-supplied transform.
- Rust SDK only. C++ output is not planned.
