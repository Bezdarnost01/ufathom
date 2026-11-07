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
