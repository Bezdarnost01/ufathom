//! Finding the `RT_VERSION` resource by walking the resource directory.
//!
//! The directory is a tree: resource type, then name, then language, then a
//! data entry. This walks only the `RT_VERSION` branch. It never searches the
//! image for the text `ProductVersion`, which also occurs in unrelated data.

use ufathom_mem::{MemoryReader, MemoryReaderExt};

use super::versioninfo::{VersionResource, parse_version_info};
use crate::bytes::Le;
use crate::error::ScanError;
use crate::limits::{
    MAX_RESOURCE_DEPTH, MAX_RESOURCE_ENTRIES, MAX_RESOURCE_NODES, MAX_VERSION_LEAVES,
    MAX_VERSION_RESOURCE_BYTES,
};
use crate::pe::{DIRECTORY_RESOURCE, PeImage};

/// Resource type id of `RT_VERSION`.
const RT_VERSION: u32 = 16;
/// Size of an `IMAGE_RESOURCE_DIRECTORY`.
const DIRECTORY_SIZE: u64 = 16;
/// Size of an `IMAGE_RESOURCE_DIRECTORY_ENTRY`.
const ENTRY_SIZE: u64 = 8;
/// Size of an `IMAGE_RESOURCE_DATA_ENTRY`.
const DATA_ENTRY_SIZE: u64 = 16;
/// Set in an entry's first word when it names its resource with a string.
const NAME_IS_STRING: u32 = 0x8000_0000;
/// Set in an entry's second word when it points to another directory.
const TARGET_IS_DIRECTORY: u32 = 0x8000_0000;
/// Language id of US English, tried before other languages.
const LANG_EN_US: u32 = 0x409;
/// Neutral language id, tried second.
const LANG_NEUTRAL: u32 = 0;
/// Depth of the directory whose entries are languages.
const LANGUAGE_DEPTH: u32 = 2;

/// One entry of a resource directory.
#[derive(Clone, Copy)]
struct Entry {
    name_or_id: u32,
    target: u32,
}

impl Entry {
    fn is_directory(&self) -> bool {
        self.target & TARGET_IS_DIRECTORY != 0
    }

    /// Offset from the resource root of the directory or data entry.
    fn offset(&self) -> u32 {
        self.target & !TARGET_IS_DIRECTORY
    }
}

/// A version data entry found in the tree.
struct Leaf {
    language: u32,
    /// Offset of the data entry from the resource root.
    offset: u32,
}

struct Walker<'a> {
    mem: &'a dyn MemoryReader,
    image: &'a PeImage,
    /// RVA of the resource root.
    root: u32,
    /// Offsets of the directories on the path to the current one.
    ancestors: Vec<u32>,
    nodes: u32,
}

impl Walker<'_> {
    /// Reads the entries of the directory at `offset`.
    fn entries(&mut self, offset: u32, depth: u32) -> Result<Vec<Entry>, ScanError> {
        if depth > MAX_RESOURCE_DEPTH {
            return Err(ScanError::malformed(
                "resource directory",
                format!("nested deeper than {MAX_RESOURCE_DEPTH} levels at offset {offset:#x}"),
            ));
        }
        if self.ancestors.contains(&offset) {
            return Err(ScanError::malformed(
                "resource directory",
                format!("directory at offset {offset:#x} contains itself"),
            ));
        }
        self.nodes += 1;
        if self.nodes > MAX_RESOURCE_NODES {
            return Err(ScanError::LimitExceeded {
                what: "resource directories read",
                value: u64::from(self.nodes),
                limit: u64::from(MAX_RESOURCE_NODES),
            });
        }
        let rva = u64::from(self.root) + u64::from(offset);
        let header_addr = self.address(rva, DIRECTORY_SIZE, "resource directory")?;
        let header = self
            .mem
            .read_vec::<u8>(header_addr, DIRECTORY_SIZE as usize)?;
        let header = Le::new(&header, "resource directory");
        let count = u32::from(header.u16(12)?) + u32::from(header.u16(14)?);
        if count > MAX_RESOURCE_ENTRIES {
            return Err(ScanError::LimitExceeded {
                what: "resource directory entries",
                value: u64::from(count),
                limit: u64::from(MAX_RESOURCE_ENTRIES),
            });
        }
        let body_len = u64::from(count) * ENTRY_SIZE;
        let body_addr =
            self.address(rva + DIRECTORY_SIZE, body_len, "resource directory entries")?;
        let body = self.mem.read_vec::<u8>(body_addr, body_len as usize)?;
        let body = Le::new(&body, "resource directory entries");
        (0..count as usize)
            .map(|index| -> Result<Entry, ScanError> {
                Ok(Entry {
                    name_or_id: body.u32(index * ENTRY_SIZE as usize)?,
                    target: body.u32(index * ENTRY_SIZE as usize + 4)?,
                })
            })
            .collect()
    }

    /// The address of `len` bytes at `rva`, or an error naming `what`.
    fn address(&self, rva: u64, len: u64, what: &'static str) -> Result<u64, ScanError> {
        self.image.address_of(rva, len).ok_or_else(|| {
            ScanError::malformed(
                what,
                format!("{len:#x} bytes at RVA {rva:#x} lie outside the image"),
            )
        })
    }

    /// Collects the data entries under the directory at `offset`.
    fn collect(
        &mut self,
        offset: u32,
        depth: u32,
        language: u32,
        out: &mut Vec<Leaf>,
    ) -> Result<(), ScanError> {
        let entries = self.entries(offset, depth)?;
        self.ancestors.push(offset);
        for entry in entries {
            if out.len() >= MAX_VERSION_LEAVES {
                break;
            }
            let language = if depth == LANGUAGE_DEPTH {
                entry.name_or_id
            } else {
                language
            };
            if entry.is_directory() {
                self.collect(entry.offset(), depth + 1, language, out)?;
            } else {
                out.push(Leaf {
                    language,
                    offset: entry.offset(),
                });
            }
        }
        self.ancestors.pop();
        Ok(())
    }

    /// Reads the data a data entry points to.
    fn read_data(&self, entry_offset: u32) -> Result<Vec<u8>, ScanError> {
        let rva = u64::from(self.root) + u64::from(entry_offset);
        let addr = self.address(rva, DATA_ENTRY_SIZE, "resource data entry")?;
        let raw = self.mem.read_vec::<u8>(addr, DATA_ENTRY_SIZE as usize)?;
        let raw = Le::new(&raw, "resource data entry");
        let data_rva = raw.u32(0)?;
        let size = raw.u32(4)?;
        if size == 0 {
            return Err(ScanError::malformed("resource data entry", "size is zero"));
        }
        if size > MAX_VERSION_RESOURCE_BYTES {
            return Err(ScanError::LimitExceeded {
                what: "version resource bytes",
                value: u64::from(size),
                limit: u64::from(MAX_VERSION_RESOURCE_BYTES),
            });
        }
        let data_addr = self.address(u64::from(data_rva), u64::from(size), "version resource")?;
        Ok(self.mem.read_vec::<u8>(data_addr, size as usize)?)
    }
}

/// Reads the version resource of `image`.
///
/// Returns `Ok(None)` when the image has no resource directory or the
/// directory has no `RT_VERSION` entry. When several languages are present,
/// US English is tried first, then the neutral language, then the rest in
/// file order, and the first that parses wins.
///
/// # Errors
///
/// [`ScanError::Malformed`], [`ScanError::LimitExceeded`] or
/// [`ScanError::Memory`] if the tree or the block is damaged, cyclic, too
/// deep, too large or unreadable.
pub(super) fn read_version_resource(
    mem: &dyn MemoryReader,
    image: &PeImage,
) -> Result<Option<VersionResource>, ScanError> {
    let Some(directory) = image.directory(DIRECTORY_RESOURCE) else {
        return Ok(None);
    };
    let mut walker = Walker {
        mem,
        image,
        root: directory.rva,
        ancestors: Vec::new(),
        nodes: 0,
    };
    let roots = walker.entries(0, 0)?;
    let Some(version_dir) = roots
        .iter()
        .find(|entry| entry.name_or_id & NAME_IS_STRING == 0 && entry.name_or_id == RT_VERSION)
    else {
        return Ok(None);
    };
    if !version_dir.is_directory() {
        return Err(ScanError::malformed(
            "resource directory",
            "the RT_VERSION entry is not a directory",
        ));
    }
    walker.ancestors.push(0);
    let mut leaves = Vec::new();
    walker.collect(version_dir.offset(), 1, 0, &mut leaves)?;
    walker.ancestors.clear();
    if leaves.is_empty() {
        return Ok(None);
    }
    // Stable sort: US English, then neutral, then file order.
    leaves.sort_by_key(|leaf| match leaf.language {
        LANG_EN_US => 0,
        LANG_NEUTRAL => 1,
        _ => 2,
    });
    let mut last_error = None;
    for leaf in &leaves {
        match walker
            .read_data(leaf.offset)
            .and_then(|bytes| parse_version_info(&bytes))
        {
            Ok(resource) => return Ok(Some(resource)),
            Err(error) => last_error = Some(error),
        }
    }
    match last_error {
        Some(error) => Err(error),
        None => Ok(None),
    }
}
