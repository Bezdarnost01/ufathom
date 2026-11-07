//! Choosing the number of block offset bits of a pool.
//!
//! The bits split a name index into a block number and a slot. Object names
//! decide them when they reach beyond the first block: only the right split
//! sends every name to a recorded entry start. A table whose objects name
//! only the first block leaves several splits consistent, and the choice
//! falls to the structure of the pool itself:
//!
//! 1. case-preserving entries repeat their own index in a comparison id, and
//!    only the right split reproduces it for entries of later blocks,
//! 2. a block must fit in the readable memory behind its pointer, and when
//!    the pointer owns its region exactly, the block size equals that extent,
//! 3. the engine's conventional split orders what is left.

use crate::ctx::{Ctx, le_u32};
use crate::limits::{BLOCK_BITS_ORDER, COMPARISON_ID_SAMPLES};

use super::pool::PoolProbe;

/// Largest block the engine allocates, in bytes.
const MAX_BLOCK_BYTES: u64 = 16 << 20;

/// Blocks beyond the first examined for comparison ids.
const COMPARISON_BLOCKS: usize = 4;

/// The decision and how strong it is.
#[derive(Debug, Clone)]
pub(crate) struct BitsChoice {
    pub bits: u32,
    pub method: String,
    /// The data pinned the value down. `false` means the choice ordered
    /// equally consistent splits.
    pub determined: bool,
}

/// Splits that make a block fit [`MAX_BLOCK_BYTES`]. Whether the entries of a
/// block fit the split is for the names to say: a block whose last entry the
/// walk cannot tell from the memory behind it would make any bound taken from
/// the walk depend on what has been parsed so far.
pub(crate) fn plausible_bits(probe: &PoolProbe<'_, '_>) -> Vec<u32> {
    let stride = u64::from(probe.candidate.format.stride);
    BLOCK_BITS_ORDER
        .into_iter()
        .filter(|&bits| stride << bits <= MAX_BLOCK_BYTES)
        .collect()
}

fn comparison_agreement(probe: &PoolProbe<'_, '_>, bits: u32) -> (usize, usize) {
    let stride = probe.candidate.format.stride as usize;
    let mut matched = 0usize;
    let mut total = 0usize;
    for block in 1..=COMPARISON_BLOCKS.min(probe.candidate.blocks.len().saturating_sub(1)) {
        let _ = probe.with_block(block, |parsed, data| {
            for &slot in parsed.starts.iter().take(COMPARISON_ID_SAMPLES) {
                let Some(id) = le_u32(data, slot as usize * stride) else {
                    continue;
                };
                total += 1;
                let own = u32::try_from(block).ok().and_then(|b| b.checked_shl(bits));
                if own.map(|b| b | slot) == Some(id) {
                    matched += 1;
                }
            }
        });
    }
    (matched, total)
}

/// Decides among the splits that object names left equally consistent.
pub(crate) fn choose_bits(
    ctx: &Ctx<'_>,
    probe: &PoolProbe<'_, '_>,
    viable: &[u32],
) -> Option<BitsChoice> {
    let mut candidates: Vec<u32> = viable.to_vec();
    if candidates.len() == 1 {
        let bits = candidates.first().copied()?;
        return Some(BitsChoice {
            bits,
            method: "object names land on recorded entry starts".to_owned(),
            determined: true,
        });
    }
    let mut methods: Vec<&str> = Vec::new();
    if probe.candidate.format.header_offset >= 4 {
        let scored: Vec<(u32, usize, usize)> = candidates
            .iter()
            .map(|&bits| {
                let (matched, total) = comparison_agreement(probe, bits);
                (bits, matched, total)
            })
            .collect();
        let best = scored
            .iter()
            .map(|&(_, matched, _)| matched)
            .max()
            .unwrap_or(0);
        let total = scored.first().map_or(0, |&(_, _, total)| total);
        if total >= 8 && best * 2 >= total {
            candidates = scored
                .iter()
                .filter(|&&(_, matched, _)| matched == best)
                .map(|&(bits, _, _)| bits)
                .collect();
            methods.push("comparison ids of later blocks repeat the entry's own index");
        }
    }
    if candidates.len() > 1 {
        let stride = u64::from(probe.candidate.format.stride);
        let extents: Vec<u64> = probe
            .candidate
            .blocks
            .iter()
            .map(|&block| ctx.readable_extent(block))
            .filter(|&extent| extent > 0)
            .collect();
        let fitting: Vec<u32> = candidates
            .iter()
            .copied()
            .filter(|&bits| extents.iter().all(|&extent| stride << bits <= extent))
            .collect();
        if !fitting.is_empty() {
            candidates = fitting;
        }
        let exact: Vec<u32> = candidates
            .iter()
            .copied()
            .filter(|&bits| extents.iter().any(|&extent| stride << bits == extent))
            .collect();
        if exact.len() == 1 {
            candidates = exact;
            methods.push("a block fills exactly the readable region behind its pointer");
        }
    }
    let bits = candidates.first().copied()?;
    if candidates.len() == 1 {
        let method = if methods.is_empty() {
            "the only split that fits the pool".to_owned()
        } else {
            methods.join("; ")
        };
        return Some(BitsChoice {
            bits,
            method,
            determined: true,
        });
    }
    Some(BitsChoice {
        bits,
        method: format!(
            "{} splits are equally consistent with the data; the engine's conventional order picked one",
            candidates.len()
        ),
        determined: false,
    })
}
