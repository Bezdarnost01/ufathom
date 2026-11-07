//! Parsing a `VS_VERSIONINFO` block.
//!
//! The block is a tree of `{ wLength, wValueLength, wType, szKey, padding,
//! Value, padding, Children }` records. The root holds a `VS_FIXEDFILEINFO`
//! and a `StringFileInfo` whose tables hold the `ProductVersion` and
//! `FileVersion` strings. Every length is untrusted: a block may not run past
//! its container, and a block that does ends the walk of that level.

use crate::bytes::Le;
use crate::error::ScanError;
use crate::limits::{
    MAX_VERSION_BLOCKS, MAX_VERSION_KEY_UNITS, MAX_VERSION_RESOURCE_BYTES, MAX_VERSION_VALUE_UNITS,
};

/// Smallest block: `wLength`, `wValueLength` and `wType`.
const BLOCK_HEADER_SIZE: usize = 6;
/// Key of the root block.
const ROOT_KEY: &str = "VS_VERSION_INFO";
/// Key of the block that holds the string tables.
const STRING_FILE_INFO_KEY: &str = "StringFileInfo";
/// `dwSignature` of a `VS_FIXEDFILEINFO`.
const FIXED_FILE_INFO_SIGNATURE: u32 = 0xFEEF_04BD;
/// Size of a `VS_FIXEDFILEINFO`.
const FIXED_FILE_INFO_SIZE: usize = 52;
/// `wType` of a block whose value is text.
const TYPE_TEXT: u16 = 1;

/// The binary version numbers of a `VS_FIXEDFILEINFO`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FixedFileInfo {
    /// `dwFileVersionMS` and `dwFileVersionLS` as four 16-bit parts, most
    /// significant first.
    pub file_version: [u16; 4],
    /// `dwProductVersionMS` and `dwProductVersionLS` as four 16-bit parts.
    pub product_version: [u16; 4],
}

impl FixedFileInfo {
    /// The first three parts of the product version as `a.b.c`, followed by
    /// the fourth only when it is not zero.
    #[must_use]
    pub fn product_version_text(&self) -> String {
        join_version(self.product_version)
    }

    /// The same for the file version.
    #[must_use]
    pub fn file_version_text(&self) -> String {
        join_version(self.file_version)
    }
}

fn join_version(parts: [u16; 4]) -> String {
    let [a, b, c, d] = parts;
    if d == 0 {
        format!("{a}.{b}.{c}")
    } else {
        format!("{a}.{b}.{c}.{d}")
    }
}

/// One string of a `StringFileInfo` table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VersionString {
    /// Key of the table, language and code page in hex, such as `040904B0`.
    pub table: String,
    /// Name of the string, such as `ProductVersion`.
    pub key: String,
    /// Its value.
    pub value: String,
}

/// What a version resource holds.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct VersionResource {
    /// The fixed binary version numbers, when present and signed correctly.
    pub fixed: Option<FixedFileInfo>,
    /// Every string, in file order.
    pub strings: Vec<VersionString>,
    /// Damage found below the root: a block that ran past its container, a
    /// bad signature, a value too long. What was read before it is kept.
    pub problems: Vec<String>,
}

impl VersionResource {
    /// The first string called `key`, compared exactly.
    #[must_use]
    pub fn string(&self, key: &str) -> Option<&str> {
        self.strings
            .iter()
            .find(|entry| entry.key == key)
            .map(|entry| entry.value.as_str())
    }
}

/// One block header and where its parts lie.
struct Block {
    /// Offset of the first byte after the block.
    end: usize,
    key: String,
    /// Offset of the value.
    value_start: usize,
    /// `wValueLength`.
    value_length: usize,
    /// `wType`.
    kind: u16,
}

/// Reads the header of the block at `start` inside a container ending at
/// `limit`.
fn read_block(blob: &Le<'_>, start: usize, limit: usize) -> Result<Block, ScanError> {
    let header_end = start.checked_add(BLOCK_HEADER_SIZE);
    if header_end.is_none_or(|end| end > limit) {
        return Err(ScanError::malformed(
            "version block",
            format!("header at {start:#x} does not fit before {limit:#x}"),
        ));
    }
    let length = usize::from(blob.u16(start)?);
    let value_length = usize::from(blob.u16(start + 2)?);
    let kind = blob.u16(start + 4)?;
    if length < BLOCK_HEADER_SIZE {
        return Err(ScanError::malformed(
            "version block",
            format!("block at {start:#x} declares length {length}"),
        ));
    }
    let end = start + length;
    if end > limit {
        return Err(ScanError::malformed(
            "version block",
            format!(
                "block at {start:#x} declares length {length:#x}, past its container end {limit:#x}"
            ),
        ));
    }
    let mut units = Vec::new();
    let mut cursor = start + BLOCK_HEADER_SIZE;
    loop {
        if cursor + 2 > end {
            return Err(ScanError::malformed(
                "version block",
                format!("key of the block at {start:#x} is not terminated"),
            ));
        }
        let unit = blob.u16(cursor)?;
        cursor += 2;
        if unit == 0 {
            break;
        }
        if units.len() >= MAX_VERSION_KEY_UNITS {
            return Err(ScanError::malformed(
                "version block",
                format!(
                    "key of the block at {start:#x} is longer than {MAX_VERSION_KEY_UNITS} characters"
                ),
            ));
        }
        units.push(unit);
    }
    Ok(Block {
        end,
        key: String::from_utf16_lossy(&units),
        value_start: cursor.next_multiple_of(4).min(end),
        value_length,
        kind,
    })
}

/// Reads the text value of a string block: UTF-16 up to the first NUL or the
/// end of the block. `wValueLength` is not trusted, because real files state
/// it in bytes as often as in characters.
fn read_text_value(blob: &Le<'_>, block: &Block) -> Result<String, ScanError> {
    if block.kind != TYPE_TEXT || block.value_length == 0 {
        return Ok(String::new());
    }
    let mut units = Vec::new();
    let mut cursor = block.value_start;
    while cursor + 2 <= block.end {
        let unit = blob.u16(cursor)?;
        if unit == 0 {
            break;
        }
        if units.len() >= MAX_VERSION_VALUE_UNITS {
            return Err(ScanError::malformed(
                "version string",
                format!(
                    "value of `{}` is longer than {MAX_VERSION_VALUE_UNITS} characters",
                    block.key
                ),
            ));
        }
        units.push(unit);
        cursor += 2;
    }
    Ok(String::from_utf16_lossy(&units))
}

/// Parses the bytes of a version resource.
///
/// # Errors
///
/// [`ScanError::Malformed`] if the root block is damaged: too large, too
/// short, wrongly keyed or unterminated. Damage in a child is recorded in
/// [`VersionResource::problems`] instead, and what precedes it is kept.
pub(super) fn parse_version_info(bytes: &[u8]) -> Result<VersionResource, ScanError> {
    if bytes.len() > MAX_VERSION_RESOURCE_BYTES as usize {
        return Err(ScanError::LimitExceeded {
            what: "version resource bytes",
            value: bytes.len() as u64,
            limit: u64::from(MAX_VERSION_RESOURCE_BYTES),
        });
    }
    let blob = Le::new(bytes, "version resource");
    let root = read_block(&blob, 0, bytes.len())?;
    if root.key != ROOT_KEY {
        return Err(ScanError::malformed(
            "version resource",
            format!("root key is `{}`, not `{ROOT_KEY}`", root.key),
        ));
    }
    let mut out = VersionResource::default();
    let value_bytes = if root.kind == TYPE_TEXT {
        root.value_length.saturating_mul(2)
    } else {
        root.value_length
    };
    let children_start = root
        .value_start
        .checked_add(value_bytes)
        .filter(|end| *end <= root.end)
        .ok_or_else(|| {
            ScanError::malformed(
                "version resource",
                format!("root value of {value_bytes} bytes runs past the block"),
            )
        })?;
    if value_bytes >= FIXED_FILE_INFO_SIZE {
        match read_fixed_info(&blob, root.value_start) {
            Ok(fixed) => out.fixed = fixed,
            Err(error) => out.problems.push(error.to_string()),
        }
    }

    let mut blocks = 0usize;
    let mut cursor = children_start.next_multiple_of(4);
    while cursor + BLOCK_HEADER_SIZE <= root.end {
        let child = match read_block(&blob, cursor, root.end) {
            Ok(child) => child,
            Err(error) => {
                out.problems.push(error.to_string());
                break;
            }
        };
        blocks += 1;
        if blocks > MAX_VERSION_BLOCKS {
            out.problems.push(format!(
                "more than {MAX_VERSION_BLOCKS} blocks; the rest is ignored"
            ));
            break;
        }
        if child.key == STRING_FILE_INFO_KEY {
            read_tables(&blob, &child, &mut blocks, &mut out);
        }
        cursor = child.end.next_multiple_of(4);
    }
    Ok(out)
}

/// Reads the `VS_FIXEDFILEINFO` at `at`. A wrong signature is `None`.
fn read_fixed_info(blob: &Le<'_>, at: usize) -> Result<Option<FixedFileInfo>, ScanError> {
    if blob.u32(at)? != FIXED_FILE_INFO_SIGNATURE {
        return Err(ScanError::malformed(
            "VS_FIXEDFILEINFO",
            "wrong signature; the fixed version numbers are ignored",
        ));
    }
    let parts = |offset: usize| -> Result<[u16; 4], ScanError> {
        let most = blob.u32(offset)?;
        let least = blob.u32(offset + 4)?;
        Ok([
            (most >> 16) as u16,
            (most & 0xFFFF) as u16,
            (least >> 16) as u16,
            (least & 0xFFFF) as u16,
        ])
    };
    Ok(Some(FixedFileInfo {
        file_version: parts(at + 8)?,
        product_version: parts(at + 16)?,
    }))
}

/// Reads the string tables inside a `StringFileInfo` block.
fn read_tables(blob: &Le<'_>, info: &Block, blocks: &mut usize, out: &mut VersionResource) {
    let mut table_cursor = info.value_start;
    while table_cursor + BLOCK_HEADER_SIZE <= info.end {
        let table = match read_block(blob, table_cursor, info.end) {
            Ok(table) => table,
            Err(error) => {
                out.problems.push(error.to_string());
                return;
            }
        };
        *blocks += 1;
        let mut cursor = table.value_start;
        while cursor + BLOCK_HEADER_SIZE <= table.end {
            if *blocks > MAX_VERSION_BLOCKS {
                out.problems.push(format!(
                    "more than {MAX_VERSION_BLOCKS} blocks; the rest is ignored"
                ));
                return;
            }
            let entry = match read_block(blob, cursor, table.end) {
                Ok(entry) => entry,
                Err(error) => {
                    out.problems.push(error.to_string());
                    break;
                }
            };
            *blocks += 1;
            match read_text_value(blob, &entry) {
                Ok(value) => out.strings.push(VersionString {
                    table: table.key.clone(),
                    key: entry.key.clone(),
                    value,
                }),
                Err(error) => out.problems.push(error.to_string()),
            }
            cursor = entry.end.next_multiple_of(4);
        }
        table_cursor = table.end.next_multiple_of(4);
    }
}
