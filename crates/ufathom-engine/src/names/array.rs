//! `TNameEntryArray`: chunks of pointers to separately allocated entries.

use rayon::prelude::*;
use ufathom_mem::{Addr, MemoryReader, MemoryReaderExt};
use ufathom_model::layout::{NameArrayLayout, NameEntryArrayLayout};

use super::decode::{decode_narrow, decode_wide, read_terminated};
use super::{BlockResult, LoadOptions, Parsed, ParsedBlock};
use crate::address::resolve_location;
use crate::error::EngineError;
use crate::limits::{MAX_NAME_CHUNKS, MAX_NAME_ENTRIES, MAX_NAME_LENGTH};

/// Bytes read in one request for an entry. Most names fit; longer ones fall
/// back to a terminator scan.
const ENTRY_PREFETCH: usize = 192;

/// Largest offset of the characters inside an entry.
const MAX_STRING_OFFSET: u32 = 64;

/// Loads every chunk of the array at `layout`.
///
/// Returns one result per chunk, in chunk order. Only failures that make the
/// whole table unusable are returned as `Err`.
pub(crate) fn load(
    mem: &dyn MemoryReader,
    layout: &NameArrayLayout,
    options: &LoadOptions<'_>,
) -> Result<Vec<BlockResult>, EngineError> {
    if layout.elements_per_chunk == 0 {
        return Err(EngineError::InvalidLayout {
            what: "name array",
            reason: "elements per chunk is zero".to_owned(),
        });
    }
    if layout.entry.string_offset > MAX_STRING_OFFSET
        || layout.entry.index_offset.saturating_add(4) > MAX_STRING_OFFSET
    {
        return Err(EngineError::InvalidLayout {
            what: "name array",
            reason: format!(
                "entry fields at {:#x} and {:#x}",
                layout.entry.index_offset, layout.entry.string_offset
            ),
        });
    }
    let location = resolve_location(mem, &layout.location)?;
    let table = if layout.indirect {
        let pointer = mem
            .read_u64(location)
            .map_err(EngineError::read(location, "name array pointer"))?;
        if pointer == 0 {
            return Err(EngineError::Null {
                addr: location,
                what: "name array",
            });
        }
        pointer
    } else {
        location
    };
    let field = |offset: u32, what: &'static str| {
        table
            .checked_add(u64::from(offset))
            .ok_or(EngineError::Inconsistent {
                addr: table,
                what,
                reason: "field address overflows".to_owned(),
            })
    };
    let elements_at = field(layout.num_elements, "name count")?;
    let elements = mem
        .read_u32(elements_at)
        .map_err(EngineError::read(elements_at, "name count"))?;
    let chunks_at = field(layout.num_chunks, "name chunk count")?;
    let chunk_count = mem
        .read_u32(chunks_at)
        .map_err(EngineError::read(chunks_at, "name chunk count"))?;
    if chunk_count > MAX_NAME_CHUNKS {
        return Err(EngineError::Limit {
            addr: chunks_at,
            what: "name chunk count",
            value: u64::from(chunk_count),
            limit: u64::from(MAX_NAME_CHUNKS),
        });
    }
    if u64::from(elements) > MAX_NAME_ENTRIES {
        return Err(EngineError::Limit {
            addr: elements_at,
            what: "name count",
            value: u64::from(elements),
            limit: MAX_NAME_ENTRIES,
        });
    }
    let per_chunk = u64::from(layout.elements_per_chunk);
    let used_chunks = u64::from(elements).div_ceil(per_chunk);
    if used_chunks > u64::from(chunk_count) {
        return Err(EngineError::Limit {
            addr: elements_at,
            what: "names beyond the chunk count",
            value: u64::from(elements),
            limit: u64::from(chunk_count) * per_chunk,
        });
    }
    let used = used_chunks as usize;
    let pointers = mem
        .read_vec::<u64>(table, used)
        .map_err(EngineError::read(table, "name chunk pointers"))?;

    let load_one = |chunk: usize| -> BlockResult {
        let ptr = pointers.get(chunk).copied().unwrap_or(0);
        let first = chunk as u64 * per_chunk;
        let count = (u64::from(elements) - first).min(per_chunk) as usize;
        BlockResult {
            block: chunk as u32,
            addr: ptr,
            outcome: read_chunk(mem, ptr, first as u32, count, &layout.entry),
        }
    };
    Ok(match options.pool {
        Some(pool) => pool.install(|| (0..used).into_par_iter().map(load_one).collect()),
        None => (0..used).map(load_one).collect(),
    })
}

fn read_chunk(
    mem: &dyn MemoryReader,
    chunk: Addr,
    first_index: u32,
    count: usize,
    entry: &NameEntryArrayLayout,
) -> Result<ParsedBlock, EngineError> {
    if chunk == 0 {
        return Err(EngineError::Null {
            addr: chunk,
            what: "name chunk",
        });
    }
    let pointers = mem
        .read_vec::<u64>(chunk, count)
        .map_err(EngineError::read(chunk, "name entry pointers"))?;
    let mut parsed = ParsedBlock::default();
    for (n, &ptr) in pointers.iter().enumerate() {
        if ptr == 0 {
            continue;
        }
        match read_entry(mem, ptr, entry) {
            Ok(text) => parsed
                .entries
                .push((first_index + n as u32, Parsed::Text(text))),
            Err(_) => parsed.unreadable += 1,
        }
    }
    Ok(parsed)
}

/// Reads one entry: the wide flag from the index field, then the characters.
fn read_entry(
    mem: &dyn MemoryReader,
    addr: Addr,
    entry: &NameEntryArrayLayout,
) -> Result<String, EngineError> {
    let string_at =
        addr.checked_add(u64::from(entry.string_offset))
            .ok_or(EngineError::Inconsistent {
                addr,
                what: "name entry",
                reason: "character address overflows".to_owned(),
            })?;
    let head = entry.string_offset as usize;
    let mut prefix = [0u8; ENTRY_PREFETCH];
    if mem.read_exact(addr, &mut prefix).is_err() {
        // Near the end of a mapping: read the pieces separately.
        let field = mem
            .read_u32(addr + u64::from(entry.index_offset))
            .map_err(EngineError::read(addr, "name entry index"))?;
        return read_terminated(mem, string_at, field & entry.wide_mask != 0);
    }
    let field = prefix
        .get(entry.index_offset as usize..entry.index_offset as usize + 4)
        .and_then(|bytes| <[u8; 4]>::try_from(bytes).ok())
        .map(u32::from_le_bytes)
        .ok_or(EngineError::Malformed {
            what: "name entry",
            reason: "index field outside the entry prefix".to_owned(),
        })?;
    let wide = field & entry.wide_mask != 0;
    let chars = prefix.get(head..).unwrap_or_default();
    let terminator = if wide {
        chars.chunks_exact(2).position(|pair| pair == [0, 0])
    } else {
        chars.iter().position(|&byte| byte == 0)
    };
    match terminator {
        Some(units) if units <= MAX_NAME_LENGTH => {
            let bytes = chars
                .get(..units * if wide { 2 } else { 1 })
                .unwrap_or_default();
            Ok(if wide {
                decode_wide(bytes)
            } else {
                decode_narrow(bytes)
            })
        }
        _ => read_terminated(mem, string_at, wide),
    }
}
