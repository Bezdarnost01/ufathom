//! Phase 1: one header per object.

use std::collections::HashMap;

use ufathom_engine::limits::FIXED_ARRAY_CHUNK_ITEMS;
use ufathom_engine::{EngineError, ObjectArrayView, ObjectHeader, ObjectSlot, read_header};
use ufathom_mem::Addr;
use ufathom_model::Severity;
use ufathom_model::layout::{ObjectArrayKind, ObjectArrayLayout};

use crate::ctx::{Ctx, Tally, describe, diagnostic};
use crate::error::ReflectError;

/// Objects whose headers one worker reads before handing back its results.
const HEADER_BATCH: usize = 1024;

/// A validated object: it sits in the slot whose index it stores and has a
/// class.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Entry {
    pub(crate) header: ObjectHeader,
}

/// Every valid object, by slot and by address.
#[derive(Debug, Default)]
pub(crate) struct ObjectTable {
    /// In slot order.
    pub(crate) entries: Vec<Entry>,
    /// Address to position in [`entries`](Self::entries). When two slots claim
    /// one address, the lower slot wins.
    pub(crate) by_addr: HashMap<Addr, usize>,
    /// Occupied slots seen, valid or not.
    pub(crate) occupied: u64,
    /// Free slots.
    pub(crate) null_slots: u64,
}

impl ObjectTable {
    pub(crate) fn entry_at(&self, addr: Addr) -> Option<&Entry> {
        self.by_addr
            .get(&addr)
            .and_then(|&position| self.entries.get(position))
    }
}

#[derive(Default)]
struct Batch {
    entries: Vec<Entry>,
    tally: Tally,
}

/// Items one chunk of the array holds.
fn items_per_chunk(layout: &ObjectArrayLayout) -> u32 {
    match layout.kind {
        ObjectArrayKind::Fixed => FIXED_ARRAY_CHUNK_ITEMS,
        ObjectArrayKind::Chunked { elements_per_chunk } => elements_per_chunk,
    }
}

/// Reads every occupied slot's header.
///
/// A chunk that cannot be read fails as a whole: one diagnostic, and every
/// slot it would have held counts as failed. A header that is unreadable,
/// stores another index or has no class is read once more without the cache
/// before it is given up on.
pub(crate) fn read_table(
    ctx: &Ctx<'_>,
    view: &ObjectArrayView<'_>,
    tally: &mut Tally,
) -> Result<ObjectTable, ReflectError> {
    let chunk_ids: Vec<u32> = (0..view.chunk_count()).collect();
    let results = ctx.exec.map_chunks(&chunk_ids, 1, |ids| {
        ids.iter()
            .map(|&chunk| {
                if ctx.cancelled() {
                    (chunk, None)
                } else {
                    (chunk, Some(view.objects_in_chunk(chunk)))
                }
            })
            .collect::<Vec<_>>()
    });
    ctx.check()?;

    let per_chunk = u64::from(items_per_chunk(&ctx.layout.objects));
    let mut table = ObjectTable::default();
    let mut slots: Vec<ObjectSlot> = Vec::new();
    for (chunk, result) in results.into_iter().flatten() {
        match result {
            Some(Ok(objects)) => {
                table.null_slots += u64::from(objects.null_slots);
                slots.extend(objects.objects);
            }
            Some(Err(error)) => {
                let first = u64::from(chunk) * per_chunk;
                let count = u64::from(view.num()).saturating_sub(first).min(per_chunk);
                table.occupied += count;
                tally.failures += count;
                tally.diagnostics.push(diagnostic(
                    "",
                    Severity::Error,
                    format!(
                        "object array chunk {chunk} cannot be read, its {count} slots are lost: {error}"
                    ),
                ));
            }
            None => {}
        }
    }
    table.occupied += slots.len() as u64;

    let batches = ctx.exec.map_chunks(&slots, HEADER_BATCH, |part| {
        let mut batch = Batch::default();
        for slot in part {
            if ctx.cancelled() {
                break;
            }
            match load_header(ctx, slot, &mut batch.tally.rereads) {
                Ok(header) => batch.entries.push(Entry { header }),
                Err(error) => batch
                    .tally
                    .failure(describe(slot.index, slot.object), error.to_string()),
            }
        }
        batch
    });
    ctx.check()?;

    table.entries.reserve(slots.len());
    for batch in batches {
        table.entries.extend(batch.entries);
        tally.merge(batch.tally);
    }
    table.by_addr.reserve(table.entries.len());
    for (position, entry) in table.entries.iter().enumerate() {
        table.by_addr.entry(entry.header.addr).or_insert(position);
    }
    Ok(table)
}

/// Reads the header of `slot`, through the cache first and then, if that does
/// not validate, directly.
fn load_header(
    ctx: &Ctx<'_>,
    slot: &ObjectSlot,
    rereads: &mut u64,
) -> Result<ObjectHeader, EngineError> {
    let layout = ctx.layout;
    let read = |mem: &dyn ufathom_mem::MemoryReader| {
        let header = read_header(mem, &layout.uobject, &layout.fname, slot.object)?;
        header.expect_index(slot.index)?;
        if header.class == 0 {
            return Err(EngineError::Null {
                addr: slot.object,
                what: "object class",
            });
        }
        Ok(header)
    };
    match read(ctx.cached) {
        Ok(header) => Ok(header),
        Err(_) => {
            *rereads += 1;
            read(ctx.raw)
        }
    }
}
