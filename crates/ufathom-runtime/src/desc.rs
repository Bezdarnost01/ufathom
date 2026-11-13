//! A `const` description of one engine build, written into every generated SDK.
//!
//! These types mirror the parts of the dump's engine layout that a
//! [`Session`](crate::Session) needs at run time. They hold only integers,
//! `&'static str` and `&'static` slices, so the generator can emit one as a
//! `const` and the SDK needs no parser and no serialization library.

/// Where a global structure lives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Place {
    /// At `rva` bytes from the load address of the module named `module`
    /// (matched by file name, ignoring ASCII case). Stable across ASLR.
    Rva {
        /// File name of the module, for example the game executable.
        module: &'static str,
        /// Offset from the module base.
        rva: u64,
    },
    /// A fixed address, valid for one process image only.
    Absolute(u64),
}

/// One reversible step that turns a stored object array pointer into the
/// real one. See the dump's `pointer_transform`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transform {
    /// Bitwise exclusive or with a constant.
    Xor(u64),
    /// Wrapping addition of a constant.
    Add(u64),
    /// Wrapping subtraction of a constant.
    Sub(u64),
    /// Rotate left by this many bits.
    RotateLeft(u32),
    /// Rotate right by this many bits.
    RotateRight(u32),
    /// Bitwise complement.
    Not,
}

impl Transform {
    /// Applies the step. Arithmetic wraps.
    #[must_use]
    pub const fn apply(self, value: u64) -> u64 {
        match self {
            Self::Xor(key) => value ^ key,
            Self::Add(delta) => value.wrapping_add(delta),
            Self::Sub(delta) => value.wrapping_sub(delta),
            Self::RotateLeft(bits) => value.rotate_left(bits),
            Self::RotateRight(bits) => value.rotate_right(bits),
            Self::Not => !value,
        }
    }
}

/// The global object array (`GUObjectArray`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ObjectArrayDesc {
    /// Where the array lives.
    pub location: Place,
    /// Items per chunk for a chunked array, `None` for one flat allocation.
    pub elements_per_chunk: Option<u32>,
    /// Offset of the pointer to the items (flat) or the chunk table (chunked).
    pub objects: u32,
    /// Offset of the live element count.
    pub num_elements: u32,
    /// `sizeof(FUObjectItem)`.
    pub item_size: u32,
    /// Offset of the object pointer inside an item.
    pub item_object: u32,
    /// Offset of the serial number inside an item, when the build has one.
    pub item_serial: Option<u32>,
    /// Steps applied, in order, to the pointer read at [`objects`](Self::objects).
    pub pointer_transform: &'static [Transform],
}

/// `FNamePool`, from UE 4.23.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NamePoolDesc {
    /// Where the pool lives.
    pub location: Place,
    /// Offset of the array of block pointers.
    pub blocks: u32,
    /// Entry alignment; a name index counts strides.
    pub stride: u32,
    /// Low bits of a name index that address the offset inside a block.
    pub block_offset_bits: u32,
    /// Offset of the entry header.
    pub header_offset: u32,
    /// Offset of the characters.
    pub string_offset: u32,
    /// Right shift that extracts the length from the header.
    pub len_shift: u32,
    /// Header mask of the wide-character flag.
    pub wide_mask: u32,
}

/// `TNameEntryArray`, before UE 4.23.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NameArrayDesc {
    /// Where the array (or a pointer to it) lives.
    pub location: Place,
    /// `true` when `location` holds a pointer to the array.
    pub indirect: bool,
    /// Offset of the element count.
    pub num_elements: u32,
    /// Entries per chunk.
    pub elements_per_chunk: u32,
    /// Offset of the index field of an entry; its low bits hold the wide flag.
    pub index_offset: u32,
    /// Offset of the characters of an entry.
    pub string_offset: u32,
    /// Index field mask of the wide-character flag.
    pub wide_mask: u32,
}

/// How names are stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NamesDesc {
    /// `FNamePool`.
    Pool(NamePoolDesc),
    /// `TNameEntryArray`.
    Array(NameArrayDesc),
}

/// The shape of `FName`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FNameDesc {
    /// `sizeof(FName)`.
    pub size: u32,
    /// Offset of the comparison index.
    pub comparison_index: u32,
    /// Offset of the display index, on builds that keep one.
    pub display_index: Option<u32>,
    /// Offset of the number. `None` on outline-number builds.
    pub number: Option<u32>,
}

/// Offsets inside `UObject` and the reflection classes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ObjectDesc {
    /// `ObjectFlags`.
    pub flags: u32,
    /// `InternalIndex`.
    pub index: u32,
    /// `ClassPrivate`.
    pub class: u32,
    /// `NamePrivate`.
    pub name: u32,
    /// `OuterPrivate`.
    pub outer: u32,
    /// `UStruct::SuperStruct`.
    pub super_struct: u32,
    /// `UStruct::PropertiesSize`, the size of a function's parameter block.
    pub properties_size: u32,
    /// `UClass::ClassDefaultObject`, when found.
    pub default_object: Option<u32>,
}

/// Everything a [`Session`](crate::Session) needs to work with one build.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EngineDesc {
    /// The name table.
    pub names: NamesDesc,
    /// The object array.
    pub objects: ObjectArrayDesc,
    /// `FName`.
    pub fname: FNameDesc,
    /// Object offsets.
    pub object: ObjectDesc,
    /// Virtual table index of `UObject::ProcessEvent`, when found.
    pub process_event_index: Option<u32>,
    /// The `GWorld` slot, when exactly one was found.
    pub gworld: Option<Place>,
    /// The `GEngine` slot, when exactly one was found.
    pub gengine: Option<Place>,
}
