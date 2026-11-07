//! Recognising and walking `FNamePool` entries without knowing their format.
//!
//! The engine registers `None`, `ByteProperty`, `IntProperty` and
//! `BoolProperty` first, in that order, in every era this project covers. The
//! format of an entry is inferred from where those four sit:
//!
//! - the string offset is the position of `None` in the first entry,
//! - the stride is the only power of two for which the second entry starts
//!   where the aligned end of the first one is,
//! - the header offset and the length shift are the only values for which the
//!   header of `None` reads 4 and the header of `ByteProperty` reads 12,
//! - the next two entries then have to land where the stride predicts.

use memchr::memmem;

use crate::ctx::le_u16;
use crate::limits::MAX_ENTRY_STRING_OFFSET;

/// The names every engine registers first. Index zero is `None`.
pub(crate) const HARDCODED: [&str; 4] = ["None", "ByteProperty", "IntProperty", "BoolProperty"];

/// Strides tried for pool entries.
const STRIDES: [u32; 4] = [2, 4, 8, 16];

/// Largest length shift of the entry header.
const MAX_LEN_SHIFT: u32 = 6;

/// Most characters an entry may claim.
const MAX_ENTRY_CHARS: usize = 1024;

/// The layout of one pool entry, as inferred.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct EntryFormat {
    pub stride: u32,
    pub header_offset: u32,
    pub string_offset: u32,
    pub len_shift: u32,
    /// Bit tested to find wide entries. Bit zero in every engine that packs
    /// the flag below the length.
    pub wide_mask: u32,
}

fn align_up(value: usize, align: usize) -> usize {
    value.div_ceil(align) * align
}

/// Whether the entry that starts at `start` holds `name` with a header that
/// agrees with `format`. Returns the aligned start of the next entry.
fn entry_matches(
    bytes: &[u8],
    start: usize,
    name: &str,
    header_offset: usize,
    string_offset: usize,
    shift: u32,
    stride: usize,
) -> Option<usize> {
    let header = le_u16(bytes, start.checked_add(header_offset)?)?;
    if usize::from(header >> shift) != name.len() || header & 1 != 0 {
        return None;
    }
    let text_at = start.checked_add(string_offset)?;
    let text = bytes.get(text_at..text_at.checked_add(name.len())?)?;
    (text == name.as_bytes()).then(|| align_up(text_at + name.len(), stride))
}

/// Every entry format consistent with a block that starts with the
/// hardcoded names. The four names alone cannot always tell two strides
/// apart: names whose entries are a multiple of the larger stride land in
/// the same places under both.
pub(crate) fn recognise_all(bytes: &[u8]) -> Vec<EntryFormat> {
    let mut out = Vec::new();
    let wanted = [HARDCODED[0], HARDCODED[1]];
    let [none, byte_property] = wanted;
    for string_offset in (2..=MAX_ENTRY_STRING_OFFSET as usize).step_by(2) {
        if bytes.get(string_offset..string_offset + none.len()) != Some(none.as_bytes()) {
            continue;
        }
        let from = string_offset + none.len();
        let to = (from + 0x20).min(bytes.len());
        let Some(found) = bytes
            .get(from..to)
            .and_then(|window| memmem::find(window, byte_property.as_bytes()))
        else {
            continue;
        };
        // The second entry starts one string offset before its characters.
        let second = from + found - string_offset;
        for stride in STRIDES {
            let stride = stride as usize;
            if align_up(string_offset + none.len(), stride) != second {
                continue;
            }
            for header_offset in (0..=string_offset - 2).step_by(2) {
                for shift in 1..=MAX_LEN_SHIFT {
                    let mut at = 0usize;
                    let mut ok = true;
                    for name in HARDCODED {
                        match entry_matches(
                            bytes,
                            at,
                            name,
                            header_offset,
                            string_offset,
                            shift,
                            stride,
                        ) {
                            Some(next) => at = next,
                            None => {
                                ok = false;
                                break;
                            }
                        }
                    }
                    if ok {
                        out.push(EntryFormat {
                            stride: stride as u32,
                            header_offset: header_offset as u32,
                            string_offset: string_offset as u32,
                            len_shift: shift,
                            wide_mask: 1,
                        });
                    }
                }
            }
        }
    }
    out
}

/// Infers the entry format from the start of a block, using the whole of
/// `bytes` to tell apart formats that the first four names cannot: the right
/// stride is the one under which the most entries parse.
pub(crate) fn infer_format(bytes: &[u8]) -> Option<EntryFormat> {
    let mut formats = recognise_all(bytes);
    if formats.len() > 1 {
        formats.sort_by_key(|f| {
            (
                std::cmp::Reverse(walk_block(bytes, f, false).starts.len()),
                f.stride,
            )
        });
    }
    formats.into_iter().next()
}

/// One walked block.
#[derive(Debug, Default, Clone)]
pub(crate) struct ParsedBlock {
    /// Slot (offset divided by the stride) of every entry, in order.
    pub starts: Vec<u32>,
    /// Offset after the last entry, aligned to the stride.
    pub end: usize,
    /// The walk ended at a terminator or a malformed entry, not at the end of
    /// the data it was given.
    pub complete: bool,
    /// Entries whose header marks them wide.
    pub wide_entries: Vec<usize>,
}

/// Walks the entries of one block. With `outline`, zero-length entries
/// followed by an id and a number are outline-number indirections and are
/// skipped over; any other zero-length entry ends the block. Without it every
/// zero-length entry ends the block, which is the strict reading used to
/// infer a format.
pub(crate) fn walk_block(data: &[u8], format: &EntryFormat, outline: bool) -> ParsedBlock {
    let stride = format.stride as usize;
    let header_offset = format.header_offset as usize;
    let string_offset = format.string_offset as usize;
    let mut parsed = ParsedBlock::default();
    let mut pos = 0usize;
    loop {
        let Some(header) = pos
            .checked_add(header_offset)
            .and_then(|at| le_u16(data, at))
        else {
            parsed.end = pos;
            return parsed;
        };
        let length = usize::from(header >> format.len_shift);
        let wide = u32::from(header) & format.wide_mask != 0;
        let chars_at = pos + string_offset;
        let end = if length == 0 {
            let number = crate::ctx::le_u32(data, chars_at + 4).unwrap_or(0);
            if !outline || number == 0 {
                parsed.end = pos;
                parsed.complete = true;
                return parsed;
            }
            chars_at + 8
        } else if length > MAX_ENTRY_CHARS {
            parsed.end = pos;
            parsed.complete = true;
            return parsed;
        } else {
            chars_at + length * if wide { 2 } else { 1 }
        };
        if end > data.len() {
            parsed.end = pos;
            return parsed;
        }
        if wide {
            parsed.wide_entries.push(pos);
        }
        parsed
            .starts
            .push(u32::try_from(pos / stride).unwrap_or(u32::MAX));
        pos = align_up(end, stride);
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::panic,
        reason = "test code may panic on failure"
    )]

    use super::*;

    fn block(format: &EntryFormat, names: &[&str]) -> Vec<u8> {
        let mut data = Vec::new();
        for name in names {
            let start = data.len();
            data.resize(start + format.string_offset as usize, 0);
            let header = (name.len() as u16) << format.len_shift;
            let at = start + format.header_offset as usize;
            data[at..at + 2].copy_from_slice(&header.to_le_bytes());
            data.extend_from_slice(name.as_bytes());
            data.resize(align_up(data.len(), format.stride as usize), 0);
        }
        data.resize(data.len() + 32, 0);
        data
    }

    #[test]
    fn infers_every_supported_format() {
        // Entry sizes that are not all a multiple of 8, so strides 4 and 8 differ.
        let names = [
            "None",
            "ByteProperty",
            "IntProperty",
            "BoolProperty",
            "Foo",
            "Foobarx",
            "Bazqux",
            "Quuxquuxq",
        ];
        for (header_offset, string_offset, shift, stride) in [
            (4, 6, 1, 4),
            (0, 2, 6, 4),
            (0, 2, 1, 2),
            (4, 6, 1, 8),
            (0, 2, 6, 8),
            (0, 2, 1, 4),
        ] {
            let format = EntryFormat {
                stride,
                header_offset,
                string_offset,
                len_shift: shift,
                wide_mask: 1,
            };
            let data = block(&format, &names);
            assert_eq!(infer_format(&data), Some(format));
            let parsed = walk_block(&data, &format, true);
            assert_eq!(parsed.starts.len(), names.len());
            assert!(parsed.complete);
        }
    }

    #[test]
    fn rejects_a_second_name_that_is_not_byte_property() {
        let format = EntryFormat {
            stride: 4,
            header_offset: 4,
            string_offset: 6,
            len_shift: 1,
            wide_mask: 1,
        };
        let data = block(
            &format,
            &["None", "ByteBanana", "IntProperty", "BoolProperty"],
        );
        assert_eq!(infer_format(&data), None);
    }
}
