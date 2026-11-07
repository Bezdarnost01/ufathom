//! Bounds on everything read from an untrusted image.
//!
//! Each constant caps a loop, an allocation or a recursion that a damaged or
//! hostile file could otherwise drive without limit.

/// Most sections a PE image may declare. The format itself allows 96.
pub const MAX_SECTIONS: u16 = 96;

/// Largest accepted `e_lfanew`, the offset of the NT headers.
pub const MAX_NT_OFFSET: u32 = 0x1_0000;

/// Data directory entries read from the optional header. The format defines 16.
pub const MAX_DATA_DIRECTORIES: u32 = 16;

/// Bytes read from the start of a PE file to parse its headers: more than the
/// NT offset, the largest optional header and the largest section table.
pub const HEAD_BYTES: u64 = 0x4_0000;

/// Largest number of bytes [`crate::load_pe_file`] maps. Real shipping
/// executables reach about 300 MB.
pub const MAX_LOAD_BYTES: u64 = 1 << 30;

/// Deepest resource directory level followed. A version resource sits at
/// depth 2 (type, name, language), so this leaves one spare level.
pub const MAX_RESOURCE_DEPTH: u32 = 3;

/// Most entries read from one resource directory.
pub const MAX_RESOURCE_ENTRIES: u32 = 1 << 14;

/// Most resource directories read in one walk.
pub const MAX_RESOURCE_NODES: u32 = 64;

/// Most version resource blocks considered, one per language.
pub const MAX_VERSION_LEAVES: usize = 8;

/// Largest version resource accepted, in bytes. A block length is a `u16`,
/// so no valid version resource is larger.
pub const MAX_VERSION_RESOURCE_BYTES: u32 = 0x1_0000;

/// Longest key in a version block, in UTF-16 units.
pub const MAX_VERSION_KEY_UNITS: usize = 128;

/// Longest value in a version string, in UTF-16 units.
pub const MAX_VERSION_VALUE_UNITS: usize = 1024;

/// Most blocks parsed in one version resource.
pub const MAX_VERSION_BLOCKS: usize = 512;

/// Longest build string accepted, in characters.
pub const MAX_BUILD_STRING_LEN: usize = 256;

/// Most distinct build strings kept. Further ones are counted, not stored.
pub const MAX_BUILD_CANDIDATES: usize = 32;

/// Bytes read from the target in one piece while scanning for strings.
pub const SCAN_CHUNK_BYTES: u64 = 4 << 20;

/// Bytes read past each chunk so a string that spans the boundary is whole.
/// It exceeds the longest build string in UTF-16 plus its prefix.
pub const SCAN_OVERLAP_BYTES: u64 = 1024;

/// Most bytes scanned in one section.
pub const MAX_SCAN_BYTES_PER_SECTION: u64 = 1 << 29;

/// Most bytes scanned in one image.
pub const MAX_SCAN_BYTES_TOTAL: u64 = 1 << 30;

// ----- layout solver -----
//
// Bounds of the solving steps. Every loop over target data and every buffer
// sized from target data is bounded by one of these.

/// Most bytes of writable module data copied into local buffers.
pub const MAX_LOCAL_DATA_BYTES: u64 = 1 << 30;

/// Bytes read per request when copying module data.
pub const LOCAL_READ_CHUNK: usize = 1 << 20;

/// Most pointer slots examined per search that walks module data.
pub const MAX_CANDIDATE_SLOTS: usize = 4_000_000;

/// Window around an object array `Objects` pointer searched for counts, in
/// bytes on each side.
pub const OBJECT_ARRAY_WINDOW: i64 = 0x30;

/// Fewest objects an object array must hold to be accepted.
///
/// Every engine registers all of CoreUObject's classes, structs, enums and
/// their properties before any game code runs, which is thousands of objects
/// in a real process; the synthetic images hold about 450 at the least.
/// Anything with fewer slots in use is some other table that happens to have
/// the right counts around a pointer, and a low bar only admits more of them.
pub const MIN_OBJECTS: u32 = 256;

/// Share of sampled objects, in percent, that must hold a pointer to an object
/// of the array at the class offset. Low enough that one damaged class object
/// does not fail the array (it takes all of its instances with it: a fifth of
/// the objects in the synthetic images), high enough that a table which is not
/// a set of `UObject`s, whose pointers rarely lead to members of the array at
/// all, never reaches it.
pub const STRUCTURE_IN_ARRAY_PERCENT: usize = 60;

/// Share of sampled objects, in percent, whose class chain must reach the
/// same fixed point (the metaclass). Low on purpose: a damaged class link
/// breaks every chain that runs through it, and the only thing this check
/// has to separate is "some objects reach a fixed point" from "none do".
pub const STRUCTURE_FIXED_POINT_PERCENT: usize = 10;

/// Fewest sampled objects that must reach the fixed point. A floor under the
/// percentage for small samples.
pub const MIN_FIXED_POINTS: usize = 3;

/// Largest object array accepted, in objects.
pub const MAX_OBJECT_COUNT: u32 = 1 << 26;

/// Smallest number of items per chunk accepted.
pub const MIN_ITEMS_PER_CHUNK: u32 = 0x100;

/// Largest number of items per chunk accepted.
pub const MAX_ITEMS_PER_CHUNK: u32 = 1 << 20;

/// Largest chunk capacity accepted.
pub const MAX_ARRAY_CHUNKS: u32 = 1 << 14;

/// Most shape tuples kept per `Objects` pointer slot.
pub const MAX_TUPLES_PER_SLOT: usize = 64;

/// Item sizes tried, in bytes. `FUObjectItem` is 0x10 to 0x30 in every era.
pub const ITEM_SIZE_RANGE: (u32, u32) = (0x10, 0x40);

/// Unreadable sampled items after which an item size is abandoned.
pub const ITEM_FAILURES_TO_GIVE_UP: usize = 8;

/// Matches among the first slots that let an item layout go on to the full
/// sample.
pub const QUICK_FIT_MATCHES: usize = 4;

/// Items sampled when confirming an object array.
pub const ARRAY_SAMPLE_COUNT: usize = 256;

/// Items sampled from each end of the array.
pub const ARRAY_SAMPLE_EDGE: usize = 24;

/// Fewest occupied sampled items for a confirmation.
pub const MIN_CONFIRMING_ITEMS: usize = 16;

/// Share of occupied sampled items that must store their own index.
pub const MIN_INDEX_AGREEMENT: f64 = 0.9;

/// Bytes of an object read when looking for the index field.
pub const OBJECT_PROBE_BYTES: usize = 0x80;

/// Largest offset searched for `UObject` fields.
pub const MAX_UOBJECT_OFFSET: u32 = 0x60;

/// How far back from the end of the array to look for the last live object.
pub const LAST_LIVE_PROBE: usize = 1 << 17;

/// Bytes of the first name pool block examined when recognising a pool.
pub const POOL_PROBE_BYTES: usize = 0x80;

/// Largest string offset in a name entry.
pub const MAX_ENTRY_STRING_OFFSET: u32 = 0x20;

/// Window before the first block pointer searched for the block counters.
pub const POOL_COUNTER_WINDOW: i64 = 0x40;

/// Block offset bit counts tried, in order of preference among equals.
pub const BLOCK_BITS_ORDER: [u32; 17] = [
    16, 15, 17, 14, 18, 13, 19, 12, 20, 11, 21, 10, 22, 9, 8, 23, 24,
];

/// Names compared per block when testing the comparison id evidence.
pub const COMPARISON_ID_SAMPLES: usize = 512;

/// Most chunks of a `TNameEntryArray` followed.
pub const MAX_NAME_ARRAY_CHUNKS: u32 = 8192;

/// Entries read from the first chunk when recognising a name array.
pub const NAME_ARRAY_PROBE_ENTRIES: usize = 8;

/// Window after the chunk pointers searched for the name array counters.
pub const NAME_ARRAY_COUNTER_WINDOW: u64 = 0x2000;

/// Bytes of memory scanned for `ByteProperty` by the name table fallback.
pub const MAX_FALLBACK_SCAN_BYTES: u64 = 512 << 20;

/// Objects sampled when solving `UObject` fields.
pub const OBJECT_SAMPLE_COUNT: usize = 1024;

/// Share of sampled objects, in percent, a name field must decode for.
/// A wrong offset decodes for a small share at best (small integers and
/// pointer halves rarely land on an entry start); a real array has a few
/// percent of objects that are freed, under construction or damaged.
pub const NAME_AGREEMENT_PERCENT: usize = 95;

/// Most objects read by the universe scan.
pub const MAX_UNIVERSE_OBJECTS: usize = 500_000;

/// Largest object class chain followed when searching for a fixed point.
pub const MAX_CLASS_HOPS: usize = 8;

/// Deepest outer chain followed.
pub const MAX_OUTER_DEPTH: usize = 32;

/// Largest offset searched inside reflection classes.
pub const MAX_REFLECTION_OFFSET: u32 = 0x180;

/// Most members read from one member chain.
pub const MAX_CHAIN_STEPS: usize = 512;

/// Most struct and class members sampled for the property core.
pub const MAX_MEMBER_SAMPLES: usize = 4000;

/// Most function parameters sampled for the property core.
pub const MAX_PARAM_SAMPLES: usize = 1500;

/// Most instances of one property kind sampled for its typed payload.
pub const MAX_PAYLOAD_SAMPLES: usize = 400;

/// Largest offset searched inside a property.
pub const MAX_PROPERTY_OFFSET: u32 = 0xC0;

/// Largest `FField` or `FFieldClass` offset searched.
pub const MAX_FFIELD_OFFSET: u32 = 0x80;

/// Largest enum entry count accepted while solving enums.
pub const MAX_ENUM_PROBE: u32 = 4096;

/// Largest stride of an enum pair.
pub const MAX_ENUM_STRIDE: u32 = 0x30;

/// Vtable slots read when looking for `ProcessEvent`.
pub const MAX_VTABLE_SLOTS: usize = 0x140;

/// Bytes of a function scanned for the `ProcessEvent` tests.
pub const PROCESS_EVENT_WINDOW: usize = 0x1800;

/// Objects whose vtables are read to learn function boundaries.
pub const VTABLE_SAMPLE_OBJECTS: usize = 96;

/// Most objects examined by the validation pass.
pub const MAX_VALIDATION_STRUCTS: usize = 6000;

/// Share of validated items that may violate an invariant before discovery
/// fails. A wrong layout breaks nearly every struct; the structs of a real
/// game that are half-built or damaged are a few percent at most.
pub const MAX_VIOLATION_SHARE: f64 = 0.05;

/// Fewest violations that fail discovery. A wrong layout breaks hundreds of
/// items; a handful of damaged objects is damage to the target and is
/// reported as a warning.
pub const MIN_VIOLATIONS_TO_FAIL: usize = 8;

/// Failures tolerated when most of a sample must agree, for samples large
/// enough that a few damaged objects do not mean a wrong offset.
pub const TOLERATED_FAILURES: usize = 8;

/// The floor of tolerated failures is at most one in this many sampled items,
/// so that a small sample does not tolerate a large share.
pub const FAILURE_FLOOR_DIVISOR: usize = 25;

/// Share, in percent, of the sampled values that must agree for a reflection
/// field (cast flags nesting, primitive sizes, array dimension, flag bits).
/// Reflection data is regular; the exceptions are editor-only and generated
/// members, a few percent of a real game's.
pub const FIELD_AGREEMENT_PERCENT: usize = 95;

/// Share, in percent, of the instances of a property kind that must resolve at
/// a payload offset. Lower than the other bars: a kind may have few instances
/// and every game adds variants of its own.
pub const PAYLOAD_AGREEMENT_PERCENT: usize = 90;

/// Most bytes of module data scanned for the slots that hold `GWorld` and
/// `GEngine`, over all modules. Each module's data is read once, in large
/// requests.
pub const MAX_GLOBALS_SCAN_BYTES: u64 = 2 << 30;
