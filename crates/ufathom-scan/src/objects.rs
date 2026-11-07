//! Step 1: the global object array.
//!
//! The search is layout free. Every pointer-aligned slot of writable module
//! data that holds a pointer is a possible `Objects` field. The 32-bit
//! fields around it are tried in every order against the shape rules of the
//! array: `num <= max`, `num_chunks <= max_chunks`, `max % max_chunks == 0`
//! and `num_chunks == ceil(num / per_chunk)`. A shape is only a candidate.
//! It is confirmed by inferring where an item keeps its object pointer and
//! where an object keeps its own index, with the same answer for items spread
//! over the whole array. Decoys that pass the shape rules fail here.

use std::collections::HashMap;

use ufathom_engine::ObjectArrayView;
use ufathom_model::layout::{Location, ObjectArrayKind, ObjectArrayLayout};

use crate::ctx::{Ctx, le_u32, le_u64};
use crate::error::ScanError;
use crate::limits::{
    ARRAY_SAMPLE_COUNT, ARRAY_SAMPLE_EDGE, ITEM_FAILURES_TO_GIVE_UP, ITEM_SIZE_RANGE,
    LAST_LIVE_PROBE, MAX_ARRAY_CHUNKS, MAX_ITEMS_PER_CHUNK, MAX_OBJECT_COUNT, MAX_TUPLES_PER_SLOT,
    MIN_CONFIRMING_ITEMS, MIN_INDEX_AGREEMENT, MIN_ITEMS_PER_CHUNK, MIN_OBJECTS,
    OBJECT_ARRAY_WINDOW, OBJECT_PROBE_BYTES, QUICK_FIT_MATCHES,
};

/// A candidate set of 32-bit fields around an `Objects` pointer. Offsets are
/// signed distances from the pointer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Tuple {
    pub max: i64,
    pub num: i64,
    pub max_chunks: Option<i64>,
    pub num_chunks: Option<i64>,
    pub max_value: u32,
    pub num_value: u32,
    pub max_chunks_value: u32,
    pub num_chunks_value: u32,
}

/// A pointer slot with the shapes found around it.
#[derive(Debug, Clone)]
pub(crate) struct ShapeCandidate {
    /// Address of the `Objects` field.
    pub slot: u64,
    /// The pointer after the user's transform.
    pub pointer: u64,
    pub tuples: Vec<Tuple>,
}

/// Where the items of an array live.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Source {
    Fixed { base: u64 },
    Chunked { table: u64, per_chunk: u32 },
}

impl Source {
    /// Address of item `index` for items of `size` bytes.
    fn item(&self, ctx: &Ctx<'_>, index: u32, size: u32) -> Option<u64> {
        match *self {
            Self::Fixed { base } => {
                base.checked_add(u64::from(index).checked_mul(u64::from(size))?)
            }
            Self::Chunked { table, per_chunk } => {
                let chunk = index / per_chunk;
                let within = index % per_chunk;
                let at = table.checked_add(u64::from(chunk) * 8)?;
                let storage = ctx.u64(at)?;
                if storage == 0 {
                    return None;
                }
                storage.checked_add(u64::from(within).checked_mul(u64::from(size))?)
            }
        }
    }
}

/// How items are laid out and where objects keep their index.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ItemFit {
    pub item_size: u32,
    pub item_object: u32,
    pub index_offset: u32,
    pub occupied: usize,
    pub matched: usize,
}

/// A confirmed object array.
#[derive(Debug, Clone)]
pub(crate) struct ObjectArrayFound {
    pub layout: ObjectArrayLayout,
    pub index_offset: u32,
}

/// The 32-bit values within [`OBJECT_ARRAY_WINDOW`] of `slot`, outside the
/// pointer itself, as `(distance, value)`.
fn window_values(ctx: &Ctx<'_>, slot: u64) -> Vec<(i64, u32)> {
    let start = slot.wrapping_sub(OBJECT_ARRAY_WINDOW as u64);
    let len = usize::try_from(2 * OBJECT_ARRAY_WINDOW + 8).unwrap_or(0);
    let Some(bytes) = ctx.read(start, len) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut at = -OBJECT_ARRAY_WINDOW;
    while at < OBJECT_ARRAY_WINDOW + 8 {
        let inside_pointer = (0..8).contains(&at);
        if !inside_pointer {
            let index = usize::try_from(at + OBJECT_ARRAY_WINDOW).unwrap_or(0);
            if let Some(value) = le_u32(&bytes, index) {
                out.push((at, value));
            }
        }
        at += 4;
    }
    out
}

fn is_power_of_two_in(value: u32, low: u32, high: u32) -> bool {
    value.is_power_of_two() && (low..=high).contains(&value)
}

/// Shapes of a chunked array: all assignments of four distinct window values
/// to `(max, max_chunks, num, num_chunks)` that satisfy the shape rules.
pub(crate) fn chunked_tuples(values: &[(i64, u32)]) -> Vec<Tuple> {
    let mut out = Vec::new();
    for &(max_at, max) in values {
        if !(MIN_OBJECTS..=MAX_OBJECT_COUNT).contains(&max) {
            continue;
        }
        for &(mc_at, max_chunks) in values {
            if mc_at == max_at
                || max_chunks == 0
                || max_chunks > MAX_ARRAY_CHUNKS
                || max % max_chunks != 0
            {
                continue;
            }
            let per_chunk = max / max_chunks;
            if !is_power_of_two_in(per_chunk, MIN_ITEMS_PER_CHUNK, MAX_ITEMS_PER_CHUNK) {
                continue;
            }
            for &(num_at, num) in values {
                if [max_at, mc_at].contains(&num_at) || num < MIN_OBJECTS || num > max {
                    continue;
                }
                let num_chunks = num.div_ceil(per_chunk);
                for &(nc_at, nc) in values {
                    if [max_at, mc_at, num_at].contains(&nc_at)
                        || nc != num_chunks
                        || nc > max_chunks
                    {
                        continue;
                    }
                    out.push(Tuple {
                        max: max_at,
                        num: num_at,
                        max_chunks: Some(mc_at),
                        num_chunks: Some(nc_at),
                        max_value: max,
                        num_value: num,
                        max_chunks_value: max_chunks,
                        num_chunks_value: nc,
                    });
                    if out.len() >= MAX_TUPLES_PER_SLOT {
                        return out;
                    }
                }
            }
        }
    }
    out
}

/// Shapes of a fixed array: a capacity and a count, in either order.
pub(crate) fn fixed_tuples(values: &[(i64, u32)]) -> Vec<Tuple> {
    let mut out = Vec::new();
    for &(max_at, max) in values {
        if !(MIN_OBJECTS..=MAX_OBJECT_COUNT).contains(&max) {
            continue;
        }
        for &(num_at, num) in values {
            if num_at == max_at || num < MIN_OBJECTS || num > max {
                continue;
            }
            out.push(Tuple {
                max: max_at,
                num: num_at,
                max_chunks: None,
                num_chunks: None,
                max_value: max,
                num_value: num,
                max_chunks_value: 0,
                num_chunks_value: 0,
            });
            if out.len() >= MAX_TUPLES_PER_SLOT {
                return out;
            }
        }
    }
    out
}

/// Every pointer slot with a shape around it. This is the check a naive
/// scanner would stop at.
pub(crate) fn shape_candidates(ctx: &Ctx<'_>, slots: &[(u64, u64)]) -> Vec<ShapeCandidate> {
    let mut out = Vec::new();
    for &(slot, pointer) in slots {
        let values = window_values(ctx, slot);
        if values.is_empty() {
            continue;
        }
        let mut tuples = chunked_tuples(&values);
        tuples.extend(fixed_tuples(&values));
        if !tuples.is_empty() {
            out.push(ShapeCandidate {
                slot,
                pointer,
                tuples,
            });
        }
    }
    out
}

/// Indices sampled across an array of `num` items: both ends and evenly
/// spaced in between.
fn sample_indices(num: u32) -> Vec<u32> {
    let mut out: Vec<u32> = Vec::new();
    let edge = u32::try_from(ARRAY_SAMPLE_EDGE).unwrap_or(24).min(num);
    out.extend(0..edge);
    out.extend(num.saturating_sub(edge)..num);
    let spread = ARRAY_SAMPLE_COUNT
        .saturating_sub(2 * ARRAY_SAMPLE_EDGE)
        .max(1);
    let step = (u64::from(num) / spread as u64).max(1);
    let mut at = step / 2;
    while at < u64::from(num) && out.len() < ARRAY_SAMPLE_COUNT {
        out.push(u32::try_from(at).unwrap_or(0));
        at += step;
    }
    out.sort_unstable();
    out.dedup();
    out
}

/// Bytes at the start of objects, read once per object.
struct ObjectBytes<'c, 'a> {
    ctx: &'c Ctx<'a>,
    cache: HashMap<u64, Option<Vec<u8>>>,
}

impl<'c, 'a> ObjectBytes<'c, 'a> {
    fn new(ctx: &'c Ctx<'a>) -> Self {
        Self {
            ctx,
            cache: HashMap::new(),
        }
    }

    fn get(&mut self, addr: u64) -> Option<&Vec<u8>> {
        let ctx = self.ctx;
        self.cache
            .entry(addr)
            .or_insert_with(|| {
                let mut bytes = ctx.read(addr, OBJECT_PROBE_BYTES);
                if bytes.is_none() {
                    bytes = ctx.read(addr, 0x40);
                }
                bytes
            })
            .as_ref()
    }
}

/// Whether the start of the item storage holds something that looks like
/// items: a pointer, outside every image, to an object whose first word (the
/// vtable) points into an image. Most pointer slots of a module's data fail
/// this at once, which keeps the item inspection away from them.
fn looks_like_items(ctx: &Ctx<'_>, source: Source) -> bool {
    let Some(start) = source.item(ctx, 0, 0) else {
        return false;
    };
    let Some(bytes) = ctx.read(start, 0x80).or_else(|| ctx.read(start, 0x40)) else {
        return false;
    };
    (0..bytes.len().saturating_sub(7)).step_by(8).any(|at| {
        le_u64(&bytes, at).is_some_and(|p| {
            ctx.is_pointer(p) && !ctx.in_image(p) && ctx.u64(p).is_some_and(|v| ctx.in_image(v))
        })
    })
}

/// Infers the item size, the object pointer offset and the index offset so
/// that the object in sampled slot `i` stores `i`.
///
/// The first slots decide the item size and the two offsets, so storage that
/// is not an object array is dropped after a few reads. Only the winning
/// combination is then checked against slots spread over the whole array.
pub(crate) fn find_fit(ctx: &Ctx<'_>, source: Source, num: u32) -> Option<ItemFit> {
    let all = sample_indices(num);
    let first: Vec<u32> = all.iter().copied().take(ARRAY_SAMPLE_EDGE).collect();
    let quick = score_fits(ctx, source, &first, None)?;
    if quick.matched < QUICK_FIT_MATCHES.min(first.len() / 2) {
        return None;
    }
    let fit = score_fits(
        ctx,
        source,
        &all,
        Some((quick.item_size, quick.item_object)),
    )?;
    // The occupied count includes only objects with an image vtable. Null
    // slots are tolerated, but the sample must not be mostly empty.
    let enough = fit.occupied >= MIN_CONFIRMING_ITEMS.min(all.len() / 2);
    let agree = fit.matched as f64 >= MIN_INDEX_AGREEMENT * fit.occupied as f64;
    (enough && agree && fit.matched >= MIN_CONFIRMING_ITEMS.min(all.len() / 2)).then_some(fit)
}

/// The best `(item size, object offset, index offset)` over `samples`, or
/// only the one combination named by `only`.
fn score_fits(
    ctx: &Ctx<'_>,
    source: Source,
    samples: &[u32],
    only: Option<(u32, u32)>,
) -> Option<ItemFit> {
    let mut objects = ObjectBytes::new(ctx);
    let mut best: Option<ItemFit> = None;
    let (lowest, highest) = ITEM_SIZE_RANGE;
    for item_size in (lowest..=highest).step_by(8) {
        if only.is_some_and(|(size, _)| size != item_size) {
            continue;
        }
        // A wrong item size runs off the storage: give up on it once the
        // first samples show nothing readable, instead of probing every page.
        let mut items: Vec<(u32, Vec<u8>)> = Vec::new();
        let mut failures = 0usize;
        for &i in samples {
            let item = source
                .item(ctx, i, item_size)
                .and_then(|at| ctx.read(at, usize::try_from(item_size).ok()?));
            match item {
                Some(bytes) => items.push((i, bytes)),
                None => failures += 1,
            }
            if failures >= ITEM_FAILURES_TO_GIVE_UP && items.is_empty() {
                break;
            }
        }
        for item_object in (0..=item_size - 8).step_by(8) {
            if only.is_some_and(|(_, object)| object != item_object) {
                continue;
            }
            let mut occupied = 0usize;
            let mut matches_at: HashMap<u32, usize> = HashMap::new();
            for (index, item) in &items {
                let Some(pointer) = le_u64(item, item_object as usize) else {
                    continue;
                };
                if !ctx.is_pointer(pointer) || ctx.in_image(pointer) {
                    continue;
                }
                let Some(bytes) = objects.get(pointer) else {
                    continue;
                };
                let in_image = le_u64(bytes, 0).is_some_and(|vtable| ctx.in_image(vtable));
                if !in_image {
                    continue;
                }
                occupied += 1;
                for offset in (8..bytes.len().saturating_sub(3)).step_by(4) {
                    if le_u32(bytes, offset) == Some(*index) {
                        *matches_at.entry(offset as u32).or_default() += 1;
                    }
                }
            }
            let Some((&index_offset, &matched)) = matches_at
                .iter()
                .max_by_key(|&(offset, count)| (*count, std::cmp::Reverse(*offset)))
            else {
                continue;
            };
            let fit = ItemFit {
                item_size,
                item_object,
                index_offset,
                occupied,
                matched,
            };
            let better = best.is_none_or(|current| {
                (fit.matched, std::cmp::Reverse(fit.item_size))
                    > (current.matched, std::cmp::Reverse(current.item_size))
            });
            if better {
                best = Some(fit);
            }
        }
    }
    best
}

/// Whether slot `index` holds a live object that stores `index`.
fn is_live(ctx: &Ctx<'_>, source: Source, fit: &ItemFit, index: u32) -> bool {
    let Some(item) = source.item(ctx, index, fit.item_size) else {
        return false;
    };
    let Some(pointer) = ctx.u64(item + u64::from(fit.item_object)) else {
        return false;
    };
    if !ctx.is_pointer(pointer) {
        return false;
    }
    ctx.u32(pointer + u64::from(fit.index_offset)) == Some(index)
        && ctx.u64(pointer).is_some_and(|vtable| ctx.in_image(vtable))
}

/// One past the highest live slot at or below `limit`, found by probing
/// backwards. `None` when nothing live is found within the bound.
fn live_extent(ctx: &Ctx<'_>, source: Source, fit: &ItemFit, limit: u32) -> Option<u32> {
    let mut index = limit;
    for _ in 0..LAST_LIVE_PROBE {
        if index == 0 {
            return is_live(ctx, source, fit, 0).then_some(1);
        }
        index -= 1;
        if is_live(ctx, source, fit, index) {
            return index.checked_add(1);
        }
    }
    None
}

/// The chunk table must start with `num_chunks` pointers and continue with
/// nulls up to the capacity.
fn chunk_table_fits(ctx: &Ctx<'_>, table: u64, tuple: &Tuple) -> bool {
    if u64::from(tuple.max_chunks_value) * 8 > ctx.readable_extent(table) {
        return false;
    }
    let check = tuple.max_chunks_value.min(tuple.num_chunks_value + 8);
    (0..check).all(|i| {
        let Some(entry) = ctx.u64(table + u64::from(i) * 8) else {
            return false;
        };
        if i < tuple.num_chunks_value {
            ctx.is_pointer(entry)
        } else {
            entry == 0
        }
    })
}

/// A scored interpretation of one slot.
#[derive(Debug, Clone)]
struct Interpretation {
    candidate: ShapeCandidate,
    tuple: Tuple,
    source: Source,
    fit: ItemFit,
    extent: u32,
}

fn interpret(ctx: &Ctx<'_>, candidate: &ShapeCandidate) -> Vec<Interpretation> {
    let mut groups: Vec<(Source, Vec<Tuple>)> = Vec::new();
    for tuple in &candidate.tuples {
        let source = match tuple.max_chunks {
            Some(_) => {
                if !chunk_table_fits(ctx, candidate.pointer, tuple) {
                    continue;
                }
                Source::Chunked {
                    table: candidate.pointer,
                    per_chunk: tuple.max_value / tuple.max_chunks_value,
                }
            }
            None => Source::Fixed {
                base: candidate.pointer,
            },
        };
        match groups.iter_mut().find(|(existing, _)| *existing == source) {
            Some((_, tuples)) => tuples.push(*tuple),
            None => groups.push((source, vec![*tuple])),
        }
    }
    let mut out = Vec::new();
    for (source, tuples) in groups {
        let Some(largest) = tuples.iter().map(|t| t.num_value).max() else {
            continue;
        };
        if !looks_like_items(ctx, source) {
            continue;
        }
        let Some(fit) = find_fit(ctx, source, largest) else {
            continue;
        };
        let Some(extent) = live_extent(ctx, source, &fit, largest) else {
            continue;
        };
        // The count must cover the live slots. Among the counts that do, the
        // smallest is the one the engine keeps: noise values far above it
        // satisfy the shape rules too, but never lie closer to the live
        // extent than the real count.
        let mut fitting: Vec<Tuple> = tuples
            .into_iter()
            .filter(|t| t.num_value >= extent)
            .collect();
        fitting.sort_by_key(|t| {
            (
                t.num_value,
                std::cmp::Reverse(t.max_chunks_value),
                t.max.abs() + t.num.abs(),
            )
        });
        if let Some(&tuple) = fitting.first() {
            out.push(Interpretation {
                candidate: candidate.clone(),
                tuple,
                source,
                fit,
                extent,
            });
        }
    }
    out
}

/// Finds the serial number offset inside an item: a 32-bit field that is
/// non-zero and different in nearly every occupied item.
fn find_serial(ctx: &Ctx<'_>, source: Source, fit: &ItemFit, num: u32) -> Option<u32> {
    let items: Vec<Vec<u8>> = sample_indices(num)
        .into_iter()
        .filter_map(|i| {
            let at = source.item(ctx, i, fit.item_size)?;
            let bytes = ctx.read(at, usize::try_from(fit.item_size).ok()?)?;
            let pointer = le_u64(&bytes, fit.item_object as usize)?;
            ctx.is_pointer(pointer).then_some(bytes)
        })
        .collect();
    if items.len() < MIN_CONFIRMING_ITEMS {
        return None;
    }
    let mut found = Vec::new();
    for offset in (0..fit.item_size.saturating_sub(3)).step_by(4) {
        let overlaps = offset < fit.item_object + 8 && offset + 4 > fit.item_object;
        if overlaps {
            continue;
        }
        let values: Vec<u32> = items
            .iter()
            .filter_map(|bytes| le_u32(bytes, offset as usize))
            .collect();
        let nonzero = values.iter().filter(|&&v| v != 0).count();
        let mut distinct = values.clone();
        distinct.sort_unstable();
        distinct.dedup();
        if nonzero * 100 >= values.len() * 95 && distinct.len() * 10 >= values.len() * 9 {
            found.push(offset);
        }
    }
    match found.as_slice() {
        [only] => Some(*only),
        _ => None,
    }
}

/// Finds and confirms the object array.
pub(crate) fn find_object_array(ctx: &Ctx<'_>) -> Result<ObjectArrayFound, ScanError> {
    let slots = ctx.pointer_slots(true);
    let candidates = shape_candidates(ctx, &slots);
    let mut interpretations: Vec<Interpretation> = Vec::new();
    for candidate in &candidates {
        interpretations.extend(interpret(ctx, candidate));
    }
    // Strongest agreement first, then the larger array.
    interpretations.sort_by(|a, b| {
        let share = |i: &Interpretation| i.fit.matched as f64 / i.fit.occupied.max(1) as f64;
        share(b)
            .total_cmp(&share(a))
            .then(b.tuple.num_value.cmp(&a.tuple.num_value))
            .then(a.candidate.slot.cmp(&b.candidate.slot))
    });
    let confirmed = interpretations.len();
    let Some(best) = interpretations.first() else {
        let hint = if ctx.options.pointer_transform.is_empty() {
            "; if the Objects pointer is stored scrambled, supply a pointer_transform"
        } else {
            "; the supplied pointer_transform produced no usable pointer"
        };
        return Err(ScanError::ObjectArrayNotFound {
            slots: slots.len(),
            shape_candidates: candidates.len(),
            confirmed,
            hint,
        });
    };
    if let Some(other) = interpretations.iter().find(|other| {
        other.candidate.slot != best.candidate.slot
            && other.fit.matched as f64 >= 0.9 * best.fit.matched as f64
    }) {
        ctx.warn(format!(
            "a second object array shape was confirmed at {:#x}; using {:#x}",
            other.candidate.slot, best.candidate.slot
        ));
    }
    build_result(ctx, best, slots.len(), candidates.len(), confirmed)
}

#[allow(
    clippy::too_many_lines,
    reason = "one pass assigning and recording every solved array field"
)]
fn build_result(
    ctx: &Ctx<'_>,
    best: &Interpretation,
    slots: usize,
    shapes: usize,
    confirmed: usize,
) -> Result<ObjectArrayFound, ScanError> {
    let tuple = best.tuple;
    let slot = best.candidate.slot;
    let relative = [
        Some(0i64),
        Some(tuple.max),
        Some(tuple.num),
        tuple.max_chunks,
        tuple.num_chunks,
    ];
    let lowest = relative.iter().flatten().copied().min().unwrap_or(0);
    let base = slot.wrapping_add(lowest as u64);
    let offset = |at: i64| u32::try_from(at - lowest).unwrap_or(0);
    let shape_method = format!(
        "shape rules over the window around the Objects pointer; {shapes} shape candidates in {slots} pointer slots, {confirmed} confirmed by item inspection"
    );
    let objects = ctx.resolve("objects.objects", Some(offset(0)), String::new)?;
    let max_elements = ctx.resolve("objects.max_elements", Some(offset(tuple.max)), String::new)?;
    let num_elements = ctx.resolve("objects.num_elements", Some(offset(tuple.num)), String::new)?;
    let (kind, max_chunks, num_chunks) = match (tuple.max_chunks, tuple.num_chunks, best.source) {
        (Some(mc), Some(nc), Source::Chunked { per_chunk, .. }) => {
            let max_chunks = ctx.resolve("objects.max_chunks", Some(offset(mc)), String::new)?;
            let num_chunks = ctx.resolve("objects.num_chunks", Some(offset(nc)), String::new)?;
            ctx.evidence(
                "objects.max_chunks",
                u64::from(max_chunks),
                shape_method.clone(),
                1,
                1,
            );
            ctx.evidence(
                "objects.num_chunks",
                u64::from(num_chunks),
                format!("num_chunks == ceil(num / {per_chunk:#x})"),
                1,
                1,
            );
            (
                ObjectArrayKind::Chunked {
                    elements_per_chunk: per_chunk,
                },
                Some(max_chunks),
                Some(num_chunks),
            )
        }
        _ => (ObjectArrayKind::Fixed, None, None),
    };
    ctx.evidence(
        "objects.objects",
        u64::from(objects),
        "pointer to a chunk table or items whose objects store their own index",
        best.fit.occupied,
        best.fit.matched,
    );
    ctx.evidence(
        "objects.max_elements",
        u64::from(tuple.max_value),
        shape_method.clone(),
        1,
        1,
    );
    ctx.evidence(
        "objects.num_elements",
        u64::from(tuple.num_value),
        format!(
            "smallest count at or above the live extent {} that fits the shape rules",
            best.extent
        ),
        1,
        1,
    );
    if let ObjectArrayKind::Chunked { elements_per_chunk } = kind {
        ctx.evidence(
            "objects.per_chunk",
            u64::from(elements_per_chunk),
            "max_elements / max_chunks",
            1,
            1,
        );
    }

    let fit = best.fit;
    let item_size = ctx.resolve("objects.item_size", Some(fit.item_size), String::new)?;
    let item_object = ctx.resolve("objects.item_object", Some(fit.item_object), String::new)?;
    let index_offset = ctx.resolve("uobject.index", Some(fit.index_offset), String::new)?;
    let fit_method = "object in sampled slot i stores i at one offset, vtable in an image";
    ctx.evidence(
        "objects.item_size",
        u64::from(item_size),
        fit_method,
        fit.occupied,
        fit.matched,
    );
    ctx.evidence(
        "objects.item_object",
        u64::from(item_object),
        fit_method,
        fit.occupied,
        fit.matched,
    );
    ctx.evidence(
        "uobject.index",
        u64::from(index_offset),
        fit_method,
        fit.occupied,
        fit.matched,
    );
    let serial_solved = find_serial(ctx, best.source, &fit, tuple.num_value);
    let item_serial = ctx.resolve_optional("objects.item_serial", serial_solved)?;
    match item_serial {
        Some(serial) if serial_solved.is_some() => ctx.evidence(
            "objects.item_serial",
            u64::from(serial),
            "non-zero 32-bit item field that differs in nearly every item",
            fit.occupied,
            fit.occupied,
        ),
        Some(_) => {}
        None => ctx.warn("objects.item_serial: no item field is unique per item"),
    }

    let rva = ctx.rva(base);
    let layout = ObjectArrayLayout {
        location: Location::Rva {
            module: ctx.module_name.clone(),
            rva,
        },
        kind,
        objects,
        max_elements,
        num_elements,
        max_chunks,
        num_chunks,
        item_size,
        item_object,
        item_serial,
        pointer_transform: ctx.options.pointer_transform.clone(),
    };
    Ok(ObjectArrayFound {
        layout,
        index_offset,
    })
}

/// Every occupied slot of the array, as `(index, object address)`.
pub(crate) fn load_slots(
    ctx: &Ctx<'_>,
    layout: &ObjectArrayLayout,
) -> Result<Vec<(u32, u64)>, ScanError> {
    let view = ObjectArrayView::new(ctx.reader(), layout)?;
    let mut out = Vec::new();
    for chunk in 0..view.chunk_count() {
        // A chunk that cannot be read is skipped: its objects are simply not
        // part of the solved set, and the validation counts what is left.
        match view.objects_in_chunk(chunk) {
            Ok(objects) => out.extend(objects.objects.iter().map(|s| (s.index, s.object))),
            Err(error) => ctx.warn(format!("object array chunk {chunk} unreadable: {error}")),
        }
    }
    Ok(out)
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

    use ufathom_testkit::{FixtureBuilder, Profile};

    use super::*;
    use crate::options::DiscoverOptions;

    /// Builds a fixture and returns the decoy's and the real array's slots.
    fn setup(name: &str, seed: u64) -> (ufathom_testkit::Fixture, u64, u64) {
        let fx = FixtureBuilder::new(Profile::by_name(name).unwrap())
            .seed(seed)
            .filler_objects(300)
            .build();
        let offset = u64::from(fx.layout.objects.objects);
        let decoy = fx.anchors.data.decoy_array + offset;
        let real = fx.anchors.data.object_array + offset;
        (fx, decoy, real)
    }

    #[test]
    fn the_decoy_passes_the_shape_check_and_fails_the_item_check() {
        let options = DiscoverOptions::default();
        for name in ["ue4.25-cp", "ue4.17"] {
            let (fx, decoy, real) = setup(name, 1);
            let ctx = Ctx::new(&fx.memory, &options).unwrap();
            let slots = ctx.pointer_slots(true);
            let shapes = shape_candidates(&ctx, &slots);
            let decoy_shape = shapes
                .iter()
                .find(|c| c.slot == decoy)
                .expect("decoy shape");
            let real_shape = shapes.iter().find(|c| c.slot == real).expect("real shape");
            // The shape rules alone cannot tell them apart.
            assert!(!decoy_shape.tuples.is_empty() && !real_shape.tuples.is_empty());
            assert!(
                interpret(&ctx, decoy_shape).is_empty(),
                "{name}: decoy confirmed"
            );
            assert!(
                !interpret(&ctx, real_shape).is_empty(),
                "{name}: real not confirmed"
            );
            let found = find_object_array(&ctx).unwrap();
            let Location::Rva { rva, .. } = found.layout.location else {
                panic!("location")
            };
            let base = ctx.module_base + rva;
            assert!(
                base <= real && real < base + 0x40,
                "{name}: the real array was chosen"
            );
        }
    }

    #[test]
    fn a_scanner_that_stops_at_the_first_shape_match_picks_the_decoy_for_some_layouts() {
        let options = DiscoverOptions::default();
        let mut decoy_first = 0;
        for seed in 0..24 {
            let (fx, decoy, real) = setup("ue4.25", seed);
            let ctx = Ctx::new(&fx.memory, &options).unwrap();
            let slots = ctx.pointer_slots(true);
            let shapes = shape_candidates(&ctx, &slots);
            // The naive scanner accepts the first slot whose neighbouring
            // counts fit the chunked shape and whose chunk table is readable.
            let naive = shapes
                .iter()
                .find(|c| {
                    c.tuples
                        .iter()
                        .any(|t| t.max_chunks.is_some() && chunk_table_fits(&ctx, c.pointer, t))
                })
                .map(|c| c.slot)
                .unwrap();
            if naive == decoy {
                decoy_first += 1;
                // The confirmation still picks the real one.
                assert!(find_object_array(&ctx).is_ok());
            } else {
                assert_eq!(naive, real, "an unrelated slot came first");
            }
        }
        assert!(
            decoy_first > 0,
            "no layout put the decoy first, so the test shows nothing"
        );
    }
}
