//! The name table fallback: look for the bytes of `ByteProperty` in memory
//! outside the module's own data, then for a pointer to the block or entry
//! that holds them.
//!
//! This covers builds that keep the name table in memory owned by another
//! module or by the heap, where the module's own writable data holds no
//! pointer to it that the main search would follow.

use std::collections::{HashMap, HashSet};

use memchr::memmem;

use crate::ctx::{Ctx, le_u32, le_u64};
use crate::limits::MAX_FALLBACK_SCAN_BYTES;

use super::array::{ArrayCandidate, from_table};
use super::entry::{EntryFormat, HARDCODED};
use super::pool::{PoolCandidate, block_format, build_candidate};

/// Bytes read per request while scanning.
const SCAN_CHUNK: usize = 1 << 20;

/// Longest distance from the start of a block to its `ByteProperty` text.
const MAX_BLOCK_HEAD: u64 = 0x40;

/// Longest distance from the start of an array entry to its text.
const MAX_ENTRY_HEAD: u64 = 0x30;

/// What the fallback found.
#[derive(Debug, Clone)]
pub(crate) enum Fallback {
    Pool(PoolCandidate),
    Array(ArrayCandidate),
}

/// Every writable region of the target, as `(base, size)`, up to the budget.
/// Memory outside every module comes first: a name table lives on the heap,
/// and the module data has already been searched.
fn scan_spans(ctx: &Ctx<'_>) -> Vec<(u64, u64)> {
    let mut spans: Vec<(u64, u64)> = ctx.writable_regions().collect();
    spans.sort_by_key(|&(base, _)| (ctx.in_image(base), base));
    let mut budget = MAX_FALLBACK_SCAN_BYTES;
    let mut out = Vec::new();
    for (base, size) in spans {
        if budget == 0 {
            break;
        }
        let size = size.min(budget);
        budget -= size;
        out.push((base, size));
    }
    out
}

fn for_each_chunk(ctx: &Ctx<'_>, spans: &[(u64, u64)], mut f: impl FnMut(u64, &[u8])) {
    for &(base, size) in spans {
        let mut offset = 0u64;
        while offset < size {
            let take = usize::try_from((size - offset).min(SCAN_CHUNK as u64)).unwrap_or(0);
            let at = base + offset;
            // The chunk overlaps the next one so text across the seam is seen.
            // The request goes straight to the target: a scan touches each
            // page once, and the page cache would only evict what the solver
            // reads again.
            let remaining = usize::try_from(size - offset)
                .unwrap_or(0)
                .saturating_sub(take);
            let want = take + remaining.min(16);
            f(at, &ctx.read_scan(at, want));
            offset += take as u64;
        }
    }
}

/// Blocks (pool) and second entries (array) that contain `ByteProperty`.
fn find_name_bytes(
    ctx: &Ctx<'_>,
    spans: &[(u64, u64)],
) -> (HashMap<u64, EntryFormat>, HashSet<u64>) {
    let text = HARDCODED[1].as_bytes();
    let mut blocks: HashMap<u64, EntryFormat> = HashMap::new();
    let mut entries: HashSet<u64> = HashSet::new();
    for_each_chunk(ctx, spans, |base, bytes| {
        for hit in memmem::find_iter(bytes, text) {
            let q = base + hit as u64;
            for back in 0..MAX_BLOCK_HEAD {
                let Some(start) = q.checked_sub(back) else {
                    break;
                };
                if let Some(format) = block_format(ctx, start) {
                    blocks.insert(start, format);
                }
            }
            for back in 4..=MAX_ENTRY_HEAD {
                let Some(start) = q.checked_sub(back) else {
                    break;
                };
                let Some(head) = ctx.read(start, 0x40) else {
                    continue;
                };
                let index_ok = (0..back.saturating_sub(3)).step_by(4).any(|at| {
                    le_u32(&head, at as usize).is_some_and(|v| v >> 1 == 1 && v & 1 == 0)
                });
                if index_ok && head.get(back as usize + text.len()) == Some(&0) {
                    entries.insert(start);
                }
            }
        }
    });
    (blocks, entries)
}

/// Aligned words equal to one of `targets`, as `(slot, value)`.
fn find_words(ctx: &Ctx<'_>, spans: &[(u64, u64)], targets: &HashSet<u64>) -> Vec<(u64, u64)> {
    let mut found = Vec::new();
    for_each_chunk(ctx, spans, |base, bytes| {
        let mut at = (8 - (base % 8)) % 8;
        while let Some(word) = le_u64(bytes, at as usize) {
            if targets.contains(&word) {
                found.push((base + at, word));
            }
            at += 8;
        }
    });
    found.sort_unstable();
    found.dedup();
    found
}

/// Searches memory outside the main search for a pool or an array.
pub(crate) fn find(ctx: &Ctx<'_>) -> Option<Fallback> {
    let spans = scan_spans(ctx);
    let (blocks, entries) = find_name_bytes(ctx, &spans);
    if blocks.is_empty() && entries.is_empty() {
        return None;
    }
    let targets: HashSet<u64> = blocks.keys().chain(entries.iter()).copied().collect();
    let hits = find_words(ctx, &spans, &targets);
    for &(slot, value) in &hits {
        if let Some(&format) = blocks.get(&value)
            && let Some(candidate) = build_candidate(ctx, slot, format)
        {
            return Some(Fallback::Pool(candidate));
        }
    }
    // An array: the hit is the second pointer of the first chunk. Follow it
    // to the chunk, then to the table that points at the chunk.
    for &(slot, value) in &hits {
        if !entries.contains(&value) {
            continue;
        }
        let chunk = slot.wrapping_sub(8);
        let chunk_targets: HashSet<u64> = HashSet::from([chunk]);
        for (table_slot, _) in find_words(ctx, &spans, &chunk_targets) {
            let table_targets: HashSet<u64> = HashSet::from([table_slot]);
            let holders = find_words(ctx, &spans, &table_targets);
            if let Some(&(holder, _)) = holders.first()
                && let Some(candidate) = from_table(ctx, holder, true, table_slot)
            {
                return Some(Fallback::Array(candidate));
            }
            if let Some(candidate) = from_table(ctx, table_slot, false, table_slot) {
                return Some(Fallback::Array(candidate));
            }
        }
    }
    None
}
