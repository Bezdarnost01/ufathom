//! Named limits. Every loop over target data and every allocation sized by
//! target data is bounded by one of these. Exceeding one is an error item,
//! never a panic and never a silent truncation.

/// Longest super or class chain walked. Real hierarchies are a few dozen deep.
pub const MAX_CHAIN_LENGTH: usize = 256;

/// Longest name accepted, in characters. The engine's own limit is 1024.
pub const MAX_NAME_LENGTH: usize = 1024;

/// Most properties one struct may yield.
pub const MAX_PROPERTIES_PER_STRUCT: usize = 16 * 1024;

/// Most links walked in one member list. Legacy lists mix properties and
/// functions, so this is above [`MAX_PROPERTIES_PER_STRUCT`].
pub const MAX_MEMBER_CHAIN_LENGTH: usize = 64 * 1024;

/// Most entries one enum may hold.
pub const MAX_ENUM_ENTRIES: usize = 64 * 1024;

/// Deepest outer chain followed when building an object path.
pub const MAX_PATH_DEPTH: usize = 64;

/// Deepest nesting of inner properties followed when converting a property
/// to a type (`TArray<TMap<K, TArray<V>>>` is depth 3).
pub const MAX_PROPERTY_DEPTH: usize = 16;

/// Most `FNamePool` blocks. The engine allows 8192 (13 block bits).
pub const MAX_NAME_BLOCKS: u32 = 8192;

/// Largest size of one `FNamePool` block in bytes.
pub const MAX_NAME_BLOCK_BYTES: u64 = 16 << 20;

/// Most chunks of a `TNameEntryArray`.
pub const MAX_NAME_CHUNKS: u32 = 8192;

/// Most name entries in one table.
pub const MAX_NAME_ENTRIES: u64 = 1 << 25;

/// Most bytes of decoded name text in one table.
pub const MAX_NAME_TABLE_BYTES: usize = 1 << 30;

/// Most objects in the global object array.
pub const MAX_OBJECTS: u32 = 1 << 26;

/// Most items in one chunk of the global object array.
pub const MAX_ITEMS_PER_CHUNK: u32 = 1 << 20;

/// Items per bulk read when the object array is one flat allocation.
pub const FIXED_ARRAY_CHUNK_ITEMS: u32 = 0x1_0000;

/// Largest `FName` in bytes.
pub const MAX_FNAME_SIZE: usize = 16;

/// Largest size of one object array item in bytes.
pub const MAX_ITEM_SIZE: u32 = 256;
