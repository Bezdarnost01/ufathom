//! `FNamePool`: blocks of variable-length entries.

use rayon::prelude::*;
use ufathom_mem::{Addr, MemoryReader, MemoryReaderExt};
use ufathom_model::layout::NamePoolLayout;

use super::decode::decode;
use super::{BlockResult, LoadOptions, Parsed, ParsedBlock};
use crate::address::resolve_location;
use crate::error::EngineError;
use crate::limits::{MAX_NAME_BLOCK_BYTES, MAX_NAME_BLOCKS, MAX_NAME_LENGTH};

/// Layout facts the block parser needs, validated once.
#[derive(Debug, Clone, Copy)]
pub(crate) struct PoolParams {
    stride: usize,
    bits: u32,
    header_offset: usize,
    string_offset: usize,
    len_shift: u32,
    wide_mask: u32,
    outline_numbers: bool,
}

impl PoolParams {
    fn new(layout: &NamePoolLayout, outline_numbers: bool) -> Result<Self, EngineError> {
        let bad = |reason: String| EngineError::InvalidLayout {
            what: "name pool",
            reason,
        };
        if layout.stride == 0 {
            return Err(bad("stride is zero".to_owned()));
        }
        if layout.block_offset_bits == 0 || layout.block_offset_bits > 31 {
            return Err(bad(format!(
                "block offset bits {} is outside 1 to 31",
                layout.block_offset_bits
            )));
        }
        if layout.entry.len_shift > 15 {
            return Err(bad(format!(
                "length shift {} does not fit a 16-bit header",
                layout.entry.len_shift
            )));
        }
        if layout.entry.string_offset < layout.entry.header_offset.saturating_add(2) {
            return Err(bad("characters start inside the entry header".to_owned()));
        }
        if layout.entry.string_offset > 64 {
            return Err(bad(format!(
                "characters start at {:#x}",
                layout.entry.string_offset
            )));
        }
        let params = Self {
            stride: layout.stride as usize,
            bits: layout.block_offset_bits,
            header_offset: layout.entry.header_offset as usize,
            string_offset: layout.entry.string_offset as usize,
            len_shift: layout.entry.len_shift,
            wide_mask: layout.entry.wide_mask,
            outline_numbers,
        };
        if params.block_bytes() > MAX_NAME_BLOCK_BYTES {
            return Err(bad(format!(
                "a block of {} bytes is above the limit {MAX_NAME_BLOCK_BYTES}",
                params.block_bytes()
            )));
        }
        Ok(params)
    }

    fn block_bytes(&self) -> u64 {
        (self.stride as u64) << self.bits
    }
}

/// Loads every block of the pool at `layout`.
///
/// Returns one result per block, in block order. Only failures that make the
/// whole table unusable are returned as `Err`.
pub(crate) fn load(
    mem: &dyn MemoryReader,
    layout: &NamePoolLayout,
    options: &LoadOptions<'_>,
) -> Result<Vec<BlockResult>, EngineError> {
    let params = PoolParams::new(layout, options.outline_numbers)?;
    let base = resolve_location(mem, &layout.location)?;
    let field = |offset: u32, what: &'static str| -> Result<Addr, EngineError> {
        base.checked_add(u64::from(offset))
            .ok_or(EngineError::Inconsistent {
                addr: base,
                what,
                reason: "field address overflows".to_owned(),
            })
    };
    let current_at = field(layout.current_block, "current block")?;
    let current = mem
        .read_u32(current_at)
        .map_err(EngineError::read(current_at, "current block"))?;
    let cursor_at = field(layout.cursor, "block cursor")?;
    let cursor = mem
        .read_u32(cursor_at)
        .map_err(EngineError::read(cursor_at, "block cursor"))?;
    if current >= MAX_NAME_BLOCKS {
        return Err(EngineError::Limit {
            addr: current_at,
            what: "name pool current block",
            value: u64::from(current),
            limit: u64::from(MAX_NAME_BLOCKS - 1),
        });
    }
    if u64::from(current) << params.bits > u64::from(u32::MAX) {
        return Err(EngineError::InvalidLayout {
            what: "name pool",
            reason: format!(
                "block {current} with {} offset bits overflows a name index",
                params.bits
            ),
        });
    }
    let count = current as usize + 1;
    let pointers_at = field(layout.blocks, "block pointers")?;
    let pointers = mem
        .read_vec::<u64>(pointers_at, count)
        .map_err(EngineError::read(pointers_at, "block pointers"))?;

    let load_one = |block: usize| -> BlockResult {
        let ptr = pointers.get(block).copied().unwrap_or(0);
        let last = block + 1 == count;
        let extent = if last {
            u64::from(cursor)
        } else {
            params.block_bytes()
        };
        BlockResult {
            block: block as u32,
            addr: ptr,
            outcome: read_block(mem, ptr, extent, block as u32, &params),
        }
    };
    Ok(match options.pool {
        Some(pool) => pool.install(|| (0..count).into_par_iter().map(load_one).collect()),
        None => (0..count).map(load_one).collect(),
    })
}

fn read_block(
    mem: &dyn MemoryReader,
    ptr: Addr,
    extent: u64,
    block: u32,
    params: &PoolParams,
) -> Result<ParsedBlock, EngineError> {
    if ptr == 0 {
        return Err(EngineError::Null {
            addr: ptr,
            what: "name block",
        });
    }
    if extent > params.block_bytes() {
        return Err(EngineError::Inconsistent {
            addr: ptr,
            what: "name block",
            reason: format!(
                "used size {extent:#x} is above the block size {:#x}",
                params.block_bytes()
            ),
        });
    }
    let mut data = vec![0u8; extent as usize];
    mem.read_exact(ptr, &mut data)
        .map_err(EngineError::read(ptr, "name block"))?;
    Ok(parse_block(&data, block, params))
}

/// Walks the entries of one block's bytes.
///
/// Stops quietly at the end of the data or at a zero-length entry that is not
/// an outline-number indirection. Stops with a recorded problem at an entry
/// that cannot be valid, because the entries after it cannot be located.
pub(crate) fn parse_block(data: &[u8], block: u32, params: &PoolParams) -> ParsedBlock {
    let mut parsed = ParsedBlock::default();
    let mut pos = 0usize;
    while let Some(header) = read_u16(data, pos + params.header_offset) {
        let wide = u32::from(header) & params.wide_mask != 0;
        let length = usize::from(header >> params.len_shift);
        let index = (block << params.bits) | (pos / params.stride) as u32;
        let chars_at = pos + params.string_offset;
        let end;
        if length == 0 {
            if !params.outline_numbers {
                break;
            }
            let (Some(id), Some(number)) = (read_u32(data, chars_at), read_u32(data, chars_at + 4))
            else {
                break;
            };
            if number == 0 {
                // Zero id data after a zero header is the padding that ends a block.
                break;
            }
            parsed
                .entries
                .push((index, Parsed::Numbered { id, number }));
            end = chars_at + 8;
        } else {
            if length > MAX_NAME_LENGTH {
                parsed.damaged = Some(format!(
                    "entry at offset {pos:#x} claims {length} characters"
                ));
                break;
            }
            let bytes = length * if wide { 2 } else { 1 };
            end = chars_at + bytes;
            let Some(chars) = data.get(chars_at..end) else {
                parsed.damaged = Some(format!(
                    "entry at offset {pos:#x} runs past the end of the block"
                ));
                break;
            };
            parsed
                .entries
                .push((index, Parsed::Text(decode(chars, wide))));
        }
        pos = end.div_ceil(params.stride) * params.stride;
    }
    parsed
}

fn read_u16(data: &[u8], at: usize) -> Option<u16> {
    let bytes: [u8; 2] = data.get(at..at.checked_add(2)?)?.try_into().ok()?;
    Some(u16::from_le_bytes(bytes))
}

fn read_u32(data: &[u8], at: usize) -> Option<u32> {
    let bytes: [u8; 4] = data.get(at..at.checked_add(4)?)?.try_into().ok()?;
    Some(u32::from_le_bytes(bytes))
}
