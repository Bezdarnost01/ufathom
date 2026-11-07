//! Finding a `TNameEntryArray`: a table of chunk pointers whose chunks hold
//! pointers to separately allocated entries.

use ufathom_model::layout::{NameArrayLayout, NameEntryArrayLayout};

use crate::ctx::{Ctx, le_u32};
use crate::limits::{MAX_NAME_ARRAY_CHUNKS, NAME_ARRAY_COUNTER_WINDOW, NAME_ARRAY_PROBE_ENTRIES};

use super::entry::HARDCODED;

/// Bytes of an entry read when looking for its fields.
const ENTRY_PROBE_BYTES: usize = 0x60;

/// Largest string offset tried in an array entry.
const MAX_ARRAY_STRING_OFFSET: usize = 0x30;

/// Smallest and largest entries per chunk accepted.
const PER_CHUNK_RANGE: (u32, u32) = (0x100, 1 << 20);

/// Entries per chunk the engine used until `FNamePool` replaced the array.
/// Only a tie-break when a single chunk leaves the value open.
const CONVENTIONAL_PER_CHUNK: u32 = 0x4000;

/// Where the fields of an entry sit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct EntryProbe {
    pub string_offset: u32,
    pub index_offset: u32,
}

/// A name array found in memory.
#[derive(Debug, Clone)]
pub(crate) struct ArrayCandidate {
    /// The slot that holds the pointer (indirect) or the table itself.
    pub slot: u64,
    pub table: u64,
    pub layout: NameArrayLayout,
    /// The number of entries per chunk was a choice, not an observation.
    pub per_chunk_open: bool,
    pub chunks: u32,
    pub names: u32,
}

/// Recognises the first entries of a chunk of entry pointers.
pub(crate) fn probe_chunk(ctx: &Ctx<'_>, chunk: u64) -> Option<EntryProbe> {
    let mut entries: Vec<Vec<u8>> = Vec::new();
    for k in 0..NAME_ARRAY_PROBE_ENTRIES {
        let pointer = ctx.u64(chunk + 8 * k as u64)?;
        if !ctx.is_pointer(pointer) {
            return None;
        }
        entries.push(ctx.read(pointer, ENTRY_PROBE_BYTES)?);
    }
    let string_offset = (4..=MAX_ARRAY_STRING_OFFSET).find(|&offset| {
        HARDCODED.iter().enumerate().all(|(k, name)| {
            entries.get(k).is_some_and(|bytes| {
                bytes.get(offset..offset + name.len()) == Some(name.as_bytes())
                    && bytes.get(offset + name.len()) == Some(&0)
            })
        })
    })?;
    // The index field holds `index << 1 | wide`.
    let index_offset = (0..string_offset.saturating_sub(3))
        .step_by(4)
        .find(|&offset| {
            entries.iter().enumerate().all(|(k, bytes)| {
                le_u32(bytes, offset).is_some_and(|v| v >> 1 == k as u32 && v & 1 == 0)
            })
        })?;
    Some(EntryProbe {
        string_offset: u32::try_from(string_offset).ok()?,
        index_offset: u32::try_from(index_offset).ok()?,
    })
}

/// Leading non-null pointers of a chunk pointer table.
fn used_chunks(ctx: &Ctx<'_>, table: u64) -> Vec<u64> {
    let mut chunks = Vec::new();
    for i in 0..u64::from(MAX_NAME_ARRAY_CHUNKS) {
        match ctx.u64(table + 8 * i) {
            Some(pointer) if pointer != 0 => chunks.push(pointer),
            _ => break,
        }
    }
    chunks
}

/// Entries per chunk that agree with `names` entries in `chunks` chunks.
/// Returns the value and whether it was the only one.
fn infer_per_chunk(chunks: u32, names: u32) -> Option<(u32, bool)> {
    let mut candidates = Vec::new();
    let mut per = PER_CHUNK_RANGE.0;
    while per <= PER_CHUNK_RANGE.1 {
        if names.div_ceil(per) == chunks {
            candidates.push(per);
        }
        per <<= 1;
    }
    match candidates.as_slice() {
        [] => None,
        [only] => Some((*only, true)),
        many => {
            let chosen = if many.contains(&CONVENTIONAL_PER_CHUNK) {
                CONVENTIONAL_PER_CHUNK
            } else {
                many.first().copied()?
            };
            Some((chosen, false))
        }
    }
}

/// Whether entries `names - 1` and `names` agree with the counters.
fn counters_fit(ctx: &Ctx<'_>, chunks: &[u64], per_chunk: u32, names: u32) -> bool {
    let entry = |index: u32| -> Option<u64> {
        let chunk = chunks.get((index / per_chunk) as usize)?;
        ctx.u64(chunk + 8 * u64::from(index % per_chunk))
    };
    // A last chunk that cannot be read is damage, not a wrong table: the
    // counters are then taken on the word of the earlier chunks.
    let last_chunk_unreadable = chunks
        .get(((names - 1) / per_chunk) as usize)
        .is_some_and(|&chunk| !ctx.is_pointer(chunk));
    let last_is_entry =
        entry(names - 1).is_some_and(|p| ctx.is_pointer(p)) || last_chunk_unreadable;
    let past_is_empty = if u64::from(names) < chunks.len() as u64 * u64::from(per_chunk)
        && !last_chunk_unreadable
    {
        entry(names) == Some(0)
    } else {
        true
    };
    last_is_entry && past_is_empty
}

fn build(
    ctx: &Ctx<'_>,
    slot: u64,
    indirect: bool,
    table: u64,
    probe: EntryProbe,
) -> Option<ArrayCandidate> {
    let chunks = used_chunks(ctx, table);
    let count = u32::try_from(chunks.len()).ok()?;
    if count == 0 {
        return None;
    }
    // After the used pointers come unused ones, all null, and then the two
    // counters: the first non-zero dword is the first counter.
    let tail_start = table + 8 * u64::from(count);
    let tail = ctx.read_prefix(tail_start, usize::try_from(NAME_ARRAY_COUNTER_WINDOW).ok()?);
    let first = (0..tail.len().saturating_sub(7))
        .step_by(4)
        .find(|&at| le_u32(&tail, at).is_some_and(|v| v != 0))?;
    let (a, b) = (le_u32(&tail, first)?, le_u32(&tail, first + 4)?);
    let (names, names_at, chunks_at) = if b == count && a > count {
        (a, first, first + 4)
    } else if a == count && b > count {
        (b, first + 4, first)
    } else {
        return None;
    };
    let (per_chunk, determined) = infer_per_chunk(count, names)?;
    if !counters_fit(ctx, &chunks, per_chunk, names) {
        return None;
    }
    let to_offset = |at: usize| u32::try_from(8 * u64::from(count) + at as u64).ok();
    Some(ArrayCandidate {
        slot,
        table,
        layout: NameArrayLayout {
            location: ctx.location(slot),
            indirect,
            num_elements: to_offset(names_at)?,
            num_chunks: to_offset(chunks_at)?,
            elements_per_chunk: per_chunk,
            entry: NameEntryArrayLayout {
                index_offset: probe.index_offset,
                string_offset: probe.string_offset,
                wide_mask: 1,
            },
        },
        per_chunk_open: !determined,
        chunks: count,
        names,
    })
}

/// Searches module data for a name array, held directly or through a pointer.
pub(crate) fn find_in_module(ctx: &Ctx<'_>) -> Vec<ArrayCandidate> {
    let mut found = Vec::new();
    for (slot, pointer) in ctx.pointer_slots(false) {
        // Indirect: the slot points to the table, whose first word points to
        // the first chunk.
        if let Some(chunk) = ctx.u64(pointer).filter(|&c| ctx.is_pointer(c))
            && let Some(probe) = probe_chunk(ctx, chunk)
            && let Some(candidate) = build(ctx, slot, true, pointer, probe)
        {
            found.push(candidate);
            continue;
        }
        // Direct: the slot is the first word of the table.
        if let Some(probe) = probe_chunk(ctx, pointer)
            && let Some(candidate) = build(ctx, slot, false, slot, probe)
        {
            found.push(candidate);
        }
    }
    found.sort_by_key(|c| (std::cmp::Reverse(c.chunks), c.slot));
    found
}

/// Builds a candidate from a table found by other means, such as the
/// fallback scan. `slot` is where a pointer to the table was seen, or the
/// table itself.
pub(crate) fn from_table(
    ctx: &Ctx<'_>,
    slot: u64,
    indirect: bool,
    table: u64,
) -> Option<ArrayCandidate> {
    let chunk = ctx.u64(table).filter(|&c| ctx.is_pointer(c))?;
    let probe = probe_chunk(ctx, chunk)?;
    build(ctx, slot, indirect, table, probe)
}
