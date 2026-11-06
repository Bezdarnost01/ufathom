//! Name storage: the shape of `FName` and the two name table designs.

use serde::{Deserialize, Serialize};

use super::Location;

/// Shape of an `FName` value stored inside objects.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FNameLayout {
    /// `sizeof(FName)`: 4, 8, 12 or 16 depending on the build.
    pub size: u32,
    /// Offset of the comparison index.
    pub comparison_index: u32,
    /// Offset of the display index. `None` when the build keeps a single index.
    pub display_index: Option<u32>,
    /// Offset of the instance number. `None` on builds that encode the number
    /// outside the `FName`, such as outline-number builds.
    pub number: Option<u32>,
}

/// How names are stored.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum NameTableLayout {
    /// `FNamePool`: a table of blocks holding variable-length entries.
    Pool(NamePoolLayout),
    /// `TNameEntryArray`: chunks holding pointers to entries.
    Array(NameArrayLayout),
}

/// `FNamePool`, introduced in UE 4.23.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NamePoolLayout {
    /// Where the pool object lives.
    pub location: Location,
    /// Offset of the array of block pointers inside the pool.
    pub blocks: u32,
    /// Offset of the index of the block currently being filled.
    pub current_block: u32,
    /// Offset of the write cursor inside the current block.
    pub cursor: u32,
    /// Entry alignment in bytes. A name index counts strides, not bytes.
    pub stride: u32,
    /// Number of low bits of a name index that address the offset inside a
    /// block. The remaining high bits select the block.
    pub block_offset_bits: u32,
    /// Layout of one entry.
    pub entry: PoolEntryLayout,
}

/// One `FNameEntry` inside an `FNamePool` block.
///
/// The entry header packs a length and a wide-character flag. The length is
/// `header >> len_shift` and the entry is wide when `header & wide_mask` is
/// non-zero.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PoolEntryLayout {
    /// Offset of the header from the entry start.
    pub header_offset: u32,
    /// Offset of the character data from the entry start.
    pub string_offset: u32,
    /// Right shift that extracts the length from the header.
    pub len_shift: u32,
    /// Mask applied to the header to test the wide-character flag.
    pub wide_mask: u32,
}

/// `TNameEntryArray`, used before UE 4.23.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NameArrayLayout {
    /// Where the array (or a pointer to it) lives.
    pub location: Location,
    /// `true` when `location` holds a pointer to the array object rather than
    /// the object itself.
    pub indirect: bool,
    /// Offset of the element count.
    pub num_elements: u32,
    /// Offset of the chunk count.
    pub num_chunks: u32,
    /// Number of entries in one chunk.
    pub elements_per_chunk: u32,
    /// Layout of one entry.
    pub entry: NameEntryArrayLayout,
}

/// One `FNameEntry` reached through a pointer in a [`NameArrayLayout`] chunk.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NameEntryArrayLayout {
    /// Offset of the index field. Its low bits carry the wide flag.
    pub index_offset: u32,
    /// Offset of the character data.
    pub string_offset: u32,
    /// Mask applied to the index field to test the wide-character flag.
    pub wide_mask: u32,
}
