//! Step 2: the name table.
//!
//! A pool or an array is recognised by the hardcoded names at its start; the
//! entry format is inferred from them (see [`entry`]). Object names decide
//! the one value that cannot be read from the table itself, the block offset
//! bits of a pool (see [`bits`]).

mod array;
mod bits;
mod entry;
mod fallback;
mod pool;

use ufathom_engine::{LoadOptions, NameTable};
use ufathom_model::layout::{FNameLayout, NamePoolLayout, NameTableLayout, PoolEntryLayout};

use crate::ctx::Ctx;
use crate::error::ScanError;

use array::ArrayCandidate;
use bits::{BitsChoice, choose_bits, plausible_bits};
use fallback::Fallback;
use pool::{PoolCandidate, PoolProbe};

/// First engine version with `FNamePool`. Only orders the two searches.
const FIRST_POOL_VERSION: u32 = 423;

/// A name table found in memory, not yet fully described.
pub(crate) struct Names<'c, 'a> {
    kind: Kind<'c, 'a>,
}

enum Kind<'c, 'a> {
    Array {
        candidate: ArrayCandidate,
        table: NameTable,
    },
    Pool {
        probe: PoolProbe<'c, 'a>,
        found_by_fallback: bool,
    },
}

fn load_array(ctx: &Ctx<'_>, candidate: &ArrayCandidate) -> Result<NameTable, ScanError> {
    let layout = NameTableLayout::Array(candidate.layout.clone());
    Ok(NameTable::load(
        ctx.reader(),
        &layout,
        &LoadOptions {
            pool: ctx.options.pool,
            outline_numbers: false,
        },
    )?)
}

impl<'c, 'a> Names<'c, 'a> {
    /// Finds the table. The pool is tried first unless the hint says the
    /// target predates it; the fallback scan comes last.
    pub(crate) fn find(ctx: &'c Ctx<'a>) -> Result<Self, ScanError> {
        let array_first = ctx
            .options
            .hint_number()
            .is_some_and(|hint| hint < FIRST_POOL_VERSION);
        let try_pool = |ctx: &'c Ctx<'a>| -> Option<Self> {
            let candidate = pool::find_in_module(ctx).into_iter().next()?;
            Some(Self {
                kind: Kind::Pool {
                    probe: PoolProbe::new(ctx, candidate),
                    found_by_fallback: false,
                },
            })
        };
        let try_array = |ctx: &'c Ctx<'a>| -> Option<Self> {
            let candidate = array::find_in_module(ctx).into_iter().next()?;
            let table = load_array(ctx, &candidate).ok()?;
            Some(Self {
                kind: Kind::Array { candidate, table },
            })
        };
        // The module the caller selected first, then those that say Core, then
        // the rest: in a modular build the name table is in `Core`, far from
        // the executable.
        for index in 0..ctx.module_count() {
            if !ctx.use_module(index) {
                continue;
            }
            let found = if array_first {
                try_array(ctx).or_else(|| try_pool(ctx))
            } else {
                try_pool(ctx).or_else(|| try_array(ctx))
            };
            if let Some(found) = found {
                return Ok(found);
            }
        }
        match fallback::find(ctx) {
            Some(Fallback::Pool(candidate)) => Ok(Self {
                kind: Kind::Pool {
                    probe: PoolProbe::new(ctx, candidate),
                    found_by_fallback: true,
                },
            }),
            Some(Fallback::Array(candidate)) => {
                let table = load_array(ctx, &candidate)?;
                Ok(Self {
                    kind: Kind::Array { candidate, table },
                })
            }
            None => Err(ScanError::NameTableNotFound {
                reason: format!(
                    "neither an FNamePool nor a TNameEntryArray whose first entries are None, \
                     ByteProperty, IntProperty and BoolProperty was found in the writable data \
                     of any of {} modules, and the scan of other memory found no ByteProperty \
                     text that a pointer leads to",
                    ctx.module_count()
                ),
            }),
        }
    }

    /// Whether the table is an `FNamePool`.
    pub(crate) fn is_pool(&self) -> bool {
        matches!(self.kind, Kind::Pool { .. })
    }

    /// The interpretations of a stored name index to try: one for an array,
    /// one per plausible block offset split for a pool.
    pub(crate) fn variants(&self) -> Vec<Option<u32>> {
        match &self.kind {
            Kind::Array { .. } => vec![None],
            Kind::Pool { probe, .. } => plausible_bits(probe).into_iter().map(Some).collect(),
        }
    }

    /// Whether `index` is an entry of the table under `variant`.
    pub(crate) fn is_name(&self, variant: Option<u32>, index: u32) -> bool {
        match (&self.kind, variant) {
            (Kind::Array { table, .. }, _) => table.is_entry_start(index),
            (Kind::Pool { probe, .. }, Some(bits)) => {
                let block = (index >> bits) as usize;
                let slot = index & ((1u32 << bits) - 1);
                probe.is_start(block, slot)
            }
            (Kind::Pool { .. }, None) => false,
        }
    }

    /// Settles the variant among those that decoded names equally well, and
    /// describes the table. Records the evidence.
    pub(crate) fn describe(
        &self,
        ctx: &Ctx<'_>,
        viable: &[Option<u32>],
        samples: usize,
        matched: usize,
    ) -> Result<NameTableLayout, ScanError> {
        match &self.kind {
            Kind::Array { candidate, .. } => Ok(describe_array(ctx, candidate)),
            Kind::Pool {
                probe,
                found_by_fallback,
            } => {
                let bits: Vec<u32> = viable.iter().flatten().copied().collect();
                let choice = choose_bits(ctx, probe, &bits).ok_or_else(|| {
                    ScanError::missing("names.block_offset_bits", "no split fits the pool")
                })?;
                describe_pool(
                    ctx,
                    &probe.candidate,
                    &choice,
                    *found_by_fallback,
                    samples,
                    matched,
                )
            }
        }
    }
}

fn describe_array(ctx: &Ctx<'_>, candidate: &ArrayCandidate) -> NameTableLayout {
    let layout = &candidate.layout;
    let table = format!(
        "chunk pointer table at {:#x} ({} chunks, {} names)",
        candidate.table, candidate.chunks, candidate.names
    );
    ctx.evidence(
        "names.array.string_offset",
        u64::from(layout.entry.string_offset),
        "position of None and ByteProperty in the first entries",
        4,
        4,
    );
    ctx.evidence(
        "names.array.index_offset",
        u64::from(layout.entry.index_offset),
        "entry k stores k << 1 at one offset for the first entries",
        8,
        8,
    );
    ctx.evidence(
        "names.array.num_elements",
        u64::from(layout.num_elements),
        format!("first counter after the unused chunk pointers; {table}"),
        1,
        1,
    );
    ctx.evidence(
        "names.array.num_chunks",
        u64::from(layout.num_chunks),
        format!("equals the number of non-null chunk pointers; {table}"),
        1,
        1,
    );
    let method = if candidate.per_chunk_open {
        ctx.warn(format!(
            "names.array.elements_per_chunk: {} names in one chunk leave the chunk size open; \
             {:#x} is the engine's conventional value, not an observation",
            candidate.names, layout.elements_per_chunk
        ));
        "chunk count and name count allow several sizes; the conventional one was taken"
    } else {
        "the only power of two with ceil(names / per_chunk) == chunks"
    };
    ctx.evidence(
        "names.array.elements_per_chunk",
        u64::from(layout.elements_per_chunk),
        method,
        1,
        1,
    );
    NameTableLayout::Array(layout.clone())
}

fn describe_pool(
    ctx: &Ctx<'_>,
    candidate: &PoolCandidate,
    choice: &BitsChoice,
    found_by_fallback: bool,
    samples: usize,
    matched: usize,
) -> Result<NameTableLayout, ScanError> {
    let bits = ctx.resolve("names.block_offset_bits", Some(choice.bits), String::new)?;
    if !choice.determined && ctx.options.overrides.names_block_offset_bits.is_none() {
        ctx.warn(format!(
            "names.block_offset_bits = {bits}: {}. The names of this image do not reach beyond \
             one block, so the value is not observable; a pool that grows will decode wrongly \
             if the engine uses another split",
            choice.method
        ));
    }
    let addresses = [
        candidate.first_slot,
        candidate.current_block_at,
        candidate.cursor_at,
    ];
    let base = addresses.into_iter().min().unwrap_or(candidate.first_slot);
    let offset = |at: u64| u32::try_from(at - base).unwrap_or(0);
    let format = candidate.format;
    let layout = NamePoolLayout {
        location: ctx.location(base),
        blocks: offset(candidate.first_slot),
        current_block: offset(candidate.current_block_at),
        cursor: offset(candidate.cursor_at),
        stride: format.stride,
        block_offset_bits: bits,
        entry: PoolEntryLayout {
            header_offset: format.header_offset,
            string_offset: format.string_offset,
            len_shift: format.len_shift,
            wide_mask: format.wide_mask,
        },
    };
    let run = format!(
        "pool whose first block starts with the hardcoded names; {} blocks{}",
        candidate.blocks.len(),
        if found_by_fallback {
            ", found by the scan of other memory"
        } else {
            ""
        }
    );
    let entry_method =
        "None, ByteProperty, IntProperty and BoolProperty read back at the inferred positions";
    for (field, value, method) in [
        ("names.pool.blocks", layout.blocks, run.as_str()),
        (
            "names.pool.current_block",
            layout.current_block,
            "equals the number of block pointers minus one",
        ),
        (
            "names.pool.cursor",
            layout.cursor,
            if candidate.cursor_verified {
                "equals the end of the last block's entries"
            } else {
                "adjacent to the block counter; the last block was unreadable"
            },
        ),
        ("names.pool.stride", format.stride, entry_method),
        (
            "names.pool.header_offset",
            format.header_offset,
            entry_method,
        ),
        (
            "names.pool.string_offset",
            format.string_offset,
            entry_method,
        ),
        ("names.pool.len_shift", format.len_shift, entry_method),
        (
            "names.pool.wide_mask",
            format.wide_mask,
            "bit zero holds the wide flag",
        ),
    ] {
        ctx.evidence(field, u64::from(value), method, 4, 4);
    }
    ctx.evidence(
        "names.block_offset_bits",
        u64::from(bits),
        choice.method.clone(),
        samples,
        matched,
    );
    Ok(NameTableLayout::Pool(layout))
}

/// Loads the whole table once the shape of `FName` is known, which decides
/// whether the pool holds outline-number entries.
pub(crate) fn load_table(
    ctx: &Ctx<'_>,
    layout: &NameTableLayout,
    fname: &FNameLayout,
) -> Result<NameTable, ScanError> {
    let options = LoadOptions {
        pool: ctx.options.pool,
        outline_numbers: fname.number.is_none(),
    };
    Ok(NameTable::load(ctx.reader(), layout, &options)?)
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

    use ufathom_model::layout::NameTableLayout;
    use ufathom_testkit::{FixtureBuilder, Profile};

    use super::*;
    use crate::options::DiscoverOptions;

    fn fixture(name: &str) -> ufathom_testkit::Fixture {
        FixtureBuilder::new(Profile::by_name(name).unwrap())
            .seed(2)
            .filler_objects(200)
            .name_blocks(3)
            .build()
    }

    #[test]
    fn pool_decoys_pass_the_naive_checks_and_fail_the_real_ones() {
        let fx = fixture("ue4.25-cp");
        let NameTableLayout::Pool(pool) = &fx.layout.names else {
            panic!("not a pool")
        };
        let options = DiscoverOptions::default();
        let ctx = Ctx::new(&fx.memory, &options).unwrap();
        assert!(ctx.use_module(0));
        let data = &fx.anchors.data;

        // A buffer that starts with `None` but is not followed by `ByteProperty`.
        let head = ctx.read(data.decoy_name_target, 0x80).unwrap();
        let has_none = (0..0x10).any(|at| head.get(at..at + 4) == Some(b"None".as_slice()));
        assert!(has_none, "a naive check on the first entry accepts it");
        assert!(pool::block_format(&ctx, data.decoy_name_target).is_none());

        // A run of block pointers with a matching counter, whose blocks are not names.
        let run = data.decoy_counters + u64::from(pool.blocks);
        let counter = ctx
            .u32(data.decoy_counters + u64::from(pool.current_block))
            .unwrap();
        let pointers = (0..8)
            .map_while(|k| ctx.u64(run + 8 * k).filter(|&p| p != 0))
            .count();
        assert_eq!(counter as usize + 1, pointers, "the counter fits the run");
        let found = pool::find_in_module(&ctx);
        assert!(!found.is_empty());
        assert!(
            found.iter().all(|c| c.first_slot != run),
            "the decoy run was taken"
        );
        assert_eq!(
            found[0].first_slot,
            data.names.anchor + u64::from(pool.blocks)
        );
    }

    #[test]
    fn an_array_decoy_that_starts_with_none_is_rejected() {
        let fx = fixture("ue4.21");
        let options = DiscoverOptions::default();
        let ctx = Ctx::new(&fx.memory, &options).unwrap();
        assert!(ctx.use_module(0));
        let data = &fx.anchors.data;
        let table = ctx.u64(data.decoy_name_slot).unwrap();
        let chunk = ctx.u64(table).unwrap();
        let first = ctx.u64(chunk).unwrap();
        let text = ctx.read(first, 0x20).unwrap();
        assert!(
            text.windows(4).any(|w| w == b"None"),
            "entry 0 of the decoy is None"
        );
        assert!(array::probe_chunk(&ctx, chunk).is_none());
        let found = array::find_in_module(&ctx);
        assert!(found.iter().all(|c| c.slot != data.decoy_name_slot));
        assert_eq!(found.len(), 1);
    }
}
