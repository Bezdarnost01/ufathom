//! The global object array (`GUObjectArray`) and pointer decoding.

use serde::{Deserialize, Serialize};

use super::Location;

/// How the object array stores its items.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ObjectArrayKind {
    /// One flat allocation.
    Fixed,
    /// Chunks of equal size reached through a table of chunk pointers.
    Chunked {
        /// Number of items in one chunk, for example 0x10000.
        elements_per_chunk: u32,
    },
}

/// One reversible step applied to a stored pointer to recover the real one.
///
/// Some titles scramble pointers in the object array. The user states the
/// steps in a config file. This crate never searches for or ships keys.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TransformOp {
    /// Bitwise exclusive or with a constant.
    Xor(u64),
    /// Wrapping addition of a constant.
    Add(u64),
    /// Wrapping subtraction of a constant.
    Sub(u64),
    /// Rotate left by this many bits. The count is taken modulo 64.
    RotateLeft(u32),
    /// Rotate right by this many bits. The count is taken modulo 64.
    RotateRight(u32),
    /// Bitwise complement.
    Not,
}

impl TransformOp {
    /// Applies this step to `value`. Arithmetic wraps and no input panics.
    #[must_use]
    pub fn apply(&self, value: u64) -> u64 {
        match *self {
            Self::Xor(key) => value ^ key,
            Self::Add(delta) => value.wrapping_add(delta),
            Self::Sub(delta) => value.wrapping_sub(delta),
            Self::RotateLeft(bits) => value.rotate_left(bits),
            Self::RotateRight(bits) => value.rotate_right(bits),
            Self::Not => !value,
        }
    }
}

/// Layout of the global object array and of its items (`FUObjectItem`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObjectArrayLayout {
    /// Where the object array lives.
    pub location: Location,
    /// Fixed or chunked storage.
    pub kind: ObjectArrayKind,
    /// Offset of the pointer to the item storage (or chunk table).
    pub objects: u32,
    /// Offset of the capacity.
    pub max_elements: u32,
    /// Offset of the live element count.
    pub num_elements: u32,
    /// Offset of the chunk capacity. `None` for fixed arrays.
    pub max_chunks: Option<u32>,
    /// Offset of the chunk count. `None` for fixed arrays.
    pub num_chunks: Option<u32>,
    /// `sizeof(FUObjectItem)`.
    pub item_size: u32,
    /// Offset of the object pointer inside an item.
    pub item_object: u32,
    /// Offset of the serial number inside an item. `None` when absent.
    pub item_serial: Option<u32>,
    /// Steps that turn a stored pointer into the real pointer, applied in
    /// order. Empty for builds that store plain pointers.
    ///
    /// They apply to exactly one pointer: the `Objects` field read at
    /// [`objects`](Self::objects), for both kinds of array. For a chunked
    /// array the decoded value is the address of the chunk table; for a fixed
    /// array it is the item storage. Entries of the chunk table and the object
    /// pointers inside items are used as stored.
    #[serde(default)]
    pub pointer_transform: Vec<TransformOp>,
}

impl ObjectArrayLayout {
    /// Applies [`pointer_transform`](Self::pointer_transform) to a stored
    /// pointer value, in order, using wrapping arithmetic.
    ///
    /// With no steps the value is returned unchanged.
    #[must_use]
    pub fn apply_pointer_transform(&self, stored: u64) -> u64 {
        self.pointer_transform
            .iter()
            .fold(stored, |value, op| op.apply(value))
    }
}
