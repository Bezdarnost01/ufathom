//! Finding an `FNamePool`: a run of block pointers, preceded by the counters
//! that describe it, whose first block starts with the hardcoded names.

use std::cell::RefCell;
use std::collections::HashMap;

use crate::ctx::{Ctx, le_u32, le_u64};
use crate::limits::{MAX_CANDIDATE_SLOTS, POOL_COUNTER_WINDOW, POOL_PROBE_BYTES};

use super::entry::{EntryFormat, ParsedBlock, infer_format, recognise_all, walk_block};

/// Most pool blocks a run may hold. The engine allows 8192 (13 block bits).
const MAX_BLOCKS: usize = 8192;

/// First read when walking a block, in bytes. Doubled until the block ends.
const FIRST_BLOCK_READ: usize = 1 << 16;

/// Largest block read, in bytes. The engine caps a block at 16 MiB.
const MAX_BLOCK_READ: usize = 16 << 20;

/// A pool found in memory. Block offset bits are not known yet.
#[derive(Debug, Clone)]
pub(crate) struct PoolCandidate {
    /// Address of `Blocks[0]`.
    pub first_slot: u64,
    /// Every pointer of the run.
    pub blocks: Vec<u64>,
    pub format: EntryFormat,
    /// Address of the current block counter.
    pub current_block_at: u64,
    /// Address of the cursor.
    pub cursor_at: u64,
    /// The cursor equals the end of the last block's entries.
    pub cursor_verified: bool,
}

/// A parsed block with the bytes it was parsed from, or `None` when the block
/// could not be read.
type ParsedBlocks = HashMap<usize, Option<(ParsedBlock, Vec<u8>)>>;

/// Blocks parsed on demand.
pub(crate) struct PoolProbe<'c, 'a> {
    ctx: &'c Ctx<'a>,
    pub candidate: PoolCandidate,
    parsed: RefCell<ParsedBlocks>,
}

/// Reads a block and walks it, growing the read until the walk reaches a
/// terminator.
fn read_and_walk(
    ctx: &Ctx<'_>,
    pointer: u64,
    format: &EntryFormat,
) -> Option<(ParsedBlock, Vec<u8>)> {
    let mut size = FIRST_BLOCK_READ;
    loop {
        let data = ctx.read_prefix(pointer, size);
        if data.is_empty() {
            return None;
        }
        let parsed = walk_block(&data, format, true);
        if parsed.complete || data.len() < size || size >= MAX_BLOCK_READ {
            return Some((parsed, data));
        }
        size = (size * 2).min(MAX_BLOCK_READ);
    }
}

impl<'c, 'a> PoolProbe<'c, 'a> {
    pub(crate) fn new(ctx: &'c Ctx<'a>, candidate: PoolCandidate) -> Self {
        Self {
            ctx,
            candidate,
            parsed: RefCell::new(HashMap::new()),
        }
    }

    /// Runs `f` on the parsed block `block`, parsing it the first time.
    pub(crate) fn with_block<R>(
        &self,
        block: usize,
        f: impl FnOnce(&ParsedBlock, &[u8]) -> R,
    ) -> Option<R> {
        let mut cache = self.parsed.borrow_mut();
        let entry = cache.entry(block).or_insert_with(|| {
            let pointer = self.candidate.blocks.get(block).copied()?;
            read_and_walk(self.ctx, pointer, &self.candidate.format)
        });
        entry.as_ref().map(|(parsed, data)| f(parsed, data))
    }

    /// Whether `(block, slot)` is the start of an entry.
    pub(crate) fn is_start(&self, block: usize, slot: u32) -> bool {
        self.with_block(block, |parsed, _| {
            parsed.starts.binary_search(&slot).is_ok()
        })
        .unwrap_or(false)
    }

    /// Largest slot of any entry of the blocks parsed so far.
    pub(crate) fn largest_slot(&self) -> u32 {
        self.parsed
            .borrow()
            .values()
            .flatten()
            .filter_map(|(parsed, _)| parsed.starts.last().copied())
            .max()
            .unwrap_or(0)
    }
}

/// The run of non-null pointers that starts at `slot`.
fn block_run(ctx: &Ctx<'_>, slot: u64) -> Vec<u64> {
    let Some(bytes) = ctx.read(slot, MAX_BLOCKS * 8).or_else(|| {
        // The run may end near the edge of readable memory.
        let available = usize::try_from(ctx.readable_extent(slot)).ok()?;
        ctx.read(slot, (available / 8 * 8).min(MAX_BLOCKS * 8))
    }) else {
        return Vec::new();
    };
    let mut run = Vec::new();
    let mut offset = 0usize;
    while let Some(value) = le_u64(&bytes, offset) {
        if value == 0 {
            break;
        }
        run.push(value);
        offset += 8;
    }
    run
}

/// Builds a candidate for a first-block slot, or `None` when the counters
/// that must precede the run cannot be found.
pub(crate) fn build_candidate(
    ctx: &Ctx<'_>,
    slot: u64,
    format: EntryFormat,
) -> Option<PoolCandidate> {
    let blocks = block_run(ctx, slot);
    let count = u32::try_from(blocks.len()).ok()?;
    if count == 0 {
        return None;
    }
    // The pool may start at the very beginning of a region, so the window
    // before the run is as long as what is readable, zero-filled in front.
    let window = usize::try_from(POOL_COUNTER_WINDOW).ok()?;
    let mut window_bytes = vec![0u8; window];
    for readable in (4..=window).rev().step_by(4) {
        if let Some(bytes) = ctx.read(slot.wrapping_sub(readable as u64), readable)
            && let Some(tail) = window_bytes.get_mut(window - readable..)
        {
            tail.copy_from_slice(&bytes);
            break;
        }
    }
    let last = read_and_walk(ctx, *blocks.last()?, &format);
    // Positions (distance before the run) whose value is the last block
    // index; the cursor sits next to it.
    let mut best: Option<(PoolCandidate, u32)> = None;
    let mut back = 4i64;
    while back <= POOL_COUNTER_WINDOW {
        let index = usize::try_from(POOL_COUNTER_WINDOW - back).ok()?;
        if le_u32(&window_bytes, index) == Some(count - 1) {
            let current_at = slot.wrapping_sub(back as u64);
            for neighbour in [back - 4, back + 4] {
                if !(4..=POOL_COUNTER_WINDOW).contains(&neighbour) {
                    continue;
                }
                let cursor_index = usize::try_from(POOL_COUNTER_WINDOW - neighbour).ok()?;
                let Some(cursor) = le_u32(&window_bytes, cursor_index) else {
                    continue;
                };
                let verified = last
                    .as_ref()
                    .is_some_and(|(parsed, _)| parsed.complete && parsed.end == cursor as usize);
                let cursor_at = slot.wrapping_sub(neighbour as u64);
                // An unreadable last block cannot verify the cursor; the
                // engine's order (counter, then cursor) is the only hint.
                let usable = verified || (last.is_none() && neighbour == back - 4);
                if !usable {
                    continue;
                }
                let score = u32::from(verified) * 4 + u32::from(neighbour == back - 4);
                let candidate = PoolCandidate {
                    first_slot: slot,
                    blocks: blocks.clone(),
                    format,
                    current_block_at: current_at,
                    cursor_at,
                    cursor_verified: verified,
                };
                if best.as_ref().is_none_or(|(_, s)| score > *s) {
                    best = Some((candidate, score));
                }
            }
        }
        back += 4;
    }
    best.map(|(candidate, _)| candidate)
}

/// The entry format of the block at `pointer`, if it starts like a pool.
pub(crate) fn block_format(ctx: &Ctx<'_>, pointer: u64) -> Option<EntryFormat> {
    // A cheap look first: most pointers lead to something else entirely.
    let head = ctx.read(pointer, POOL_PROBE_BYTES)?;
    if recognise_all(&head).is_empty() {
        return None;
    }
    infer_format(&ctx.read_prefix(pointer, FIRST_BLOCK_READ))
}

/// Searches module data for a pool.
pub(crate) fn find_in_module(ctx: &Ctx<'_>) -> Vec<PoolCandidate> {
    let mut found = Vec::new();
    let mut seen: HashMap<u64, Option<EntryFormat>> = HashMap::new();
    for (slot, pointer) in ctx
        .pointer_slots(false)
        .into_iter()
        .take(MAX_CANDIDATE_SLOTS)
    {
        let format = *seen
            .entry(pointer)
            .or_insert_with(|| block_format(ctx, pointer));
        let Some(format) = format else {
            continue;
        };
        if let Some(candidate) = build_candidate(ctx, slot, format) {
            found.push(candidate);
        }
    }
    found.sort_by_key(|c| {
        (
            std::cmp::Reverse(c.cursor_verified),
            std::cmp::Reverse(c.blocks.len()),
            c.first_slot,
        )
    });
    found
}
