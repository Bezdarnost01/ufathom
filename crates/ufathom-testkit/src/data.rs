//! The `.data` section: the object array, the name table, the `GWorld` and
//! `GEngine` slots, decoys and noise.
//!
//! Decoys exist because a solver that only works on a clean image proves
//! nothing. Each one passes a naive shape check and fails a deeper one.

use ufathom_model::layout::{NameTableLayout, ObjectArrayKind};

use crate::code::Code;
use crate::names::{NameAnchors, NameTable};
use crate::profile::Profile;
use crate::reflect::{Reflection, Slot};
use crate::rng::Rng;
use crate::space::{Mem, Pool};

/// Where the pieces of an object array's items live.
#[derive(Debug, Clone, Default)]
pub struct ItemAnchors {
    /// The chunk pointer table (chunked) or the item array (fixed).
    pub table: u64,
    /// Address of each chunk (chunked only).
    pub chunks: Vec<u64>,
    /// Chunk capacity of the table, as written.
    pub max_chunks: u32,
    /// Items per chunk.
    pub per_chunk: u32,
}

/// Addresses of everything written to `.data`, kept as ground truth.
#[derive(Debug, Clone, Default)]
pub struct DataAnchors {
    /// Start of the object array structure.
    pub object_array: u64,
    /// The real object array's items.
    pub items: ItemAnchors,
    /// Start of the name table: the pool, or the `GNames` slot.
    pub name_anchor: u64,
    /// Name table addresses.
    pub names: NameAnchors,
    /// The `GWorld` slot.
    pub gworld: u64,
    /// The `GEngine` slot.
    pub gengine: u64,
    /// A counter slot the fake host increments.
    pub frame_counter: u64,
    /// Decoy object array: plausible numbers, items fail the self-index check.
    pub decoy_array: u64,
    /// Decoy array's items.
    pub decoy_items: ItemAnchors,
    /// Decoy counters preceding a run of heap pointers (start of the struct).
    pub decoy_counters: u64,
    /// The pointers in the decoy run.
    pub decoy_blocks: Vec<u64>,
    /// A slot pointing to a buffer that starts with `None` but is not followed
    /// by `ByteProperty`.
    pub decoy_name_slot: u64,
    /// Start of the decoy name table structure (pool or array object).
    pub decoy_name_target: u64,
    /// A slot pointing to a stale `World`-looking object.
    pub stale_world_slot: u64,
    /// A slot pointing to the `World` default object.
    pub world_cdo_slot: u64,
    /// A slot pointing to the `GameEngine` default object.
    pub engine_cdo_slot: u64,
}

/// Pointers noise can contain.
struct NoisePool {
    heap: Vec<u64>,
    image: Vec<u64>,
}

fn noise(mem: &mut Mem, rng: &mut Rng, pool: &NoisePool, addr: u64, len: usize) {
    for word in 0..len / 8 {
        let at = addr + word as u64 * 8;
        let value = match rng.below(20) {
            0..=5 => 0,
            6..=10 => rng.pick(&pool.heap).copied().unwrap_or(0),
            11..=13 => rng.pick(&pool.image).copied().unwrap_or(0),
            14..=16 => {
                u64::from(rng.next_u32() & 0xFFFF) | (u64::from(rng.next_u32() & 0xFF) << 32)
            }
            17 => u64::from(f32::to_bits(rng.below(10_000) as f32 / 7.0)),
            _ => rng.next_u64() >> rng.below(40),
        };
        mem.put_u64(at, value);
    }
}

/// Writes the items of an object array and returns where they went.
///
/// Slot `i` is written at chunk `i / per_chunk`, position `i % per_chunk`.
pub fn write_items(
    mem: &mut Mem,
    profile: &Profile,
    slots: &[Slot],
    per_chunk: u32,
    max_chunks: u32,
    fixed_capacity: u32,
) -> ItemAnchors {
    let layout = &profile.layout.objects;
    let shape = &profile.shape;
    let item_size = u64::from(layout.item_size);
    let write_item = |mem: &mut Mem, at: u64, slot: &Slot| {
        mem.put_u64(at + u64::from(layout.item_object), slot.object);
        mem.put_u32(at + u64::from(shape.item_flags), slot.flags);
        if let Some(cluster) = shape.item_cluster_root {
            mem.put_u32(at + u64::from(cluster), 0);
        }
        if let Some(serial) = layout.item_serial {
            mem.put_u32(at + u64::from(serial), slot.serial);
        }
    };
    if layout.kind == ObjectArrayKind::Fixed {
        let capacity = usize::try_from(fixed_capacity).unwrap_or(0);
        let bytes = capacity * usize::try_from(item_size).unwrap_or(0x18);
        let table = mem.heap.alloc_at_page_end(bytes);
        for (i, slot) in slots.iter().enumerate() {
            write_item(mem, table + i as u64 * item_size, slot);
        }
        return ItemAnchors {
            table,
            chunks: Vec::new(),
            max_chunks: 0,
            per_chunk: 0,
        };
    }
    let per = u64::from(per_chunk);
    let chunk_count = (slots.len() as u64).div_ceil(per).max(1);
    // The chunk table ends at a page boundary, so a read past it is unmapped.
    let table = mem
        .heap
        .alloc_at_page_end(usize::try_from(max_chunks).unwrap_or(0) * 8);
    let mut chunks = Vec::new();
    for chunk in 0..chunk_count {
        let bytes = usize::try_from(per * item_size).unwrap_or(0);
        let addr = mem.heap.alloc(Pool::Items, bytes, 16);
        mem.put_u64(table + chunk * 8, addr);
        chunks.push(addr);
    }
    for (i, slot) in slots.iter().enumerate() {
        let chunk = chunks
            .get(i / usize::try_from(per).unwrap_or(1))
            .copied()
            .unwrap_or(0);
        write_item(mem, chunk + (i as u64 % per) * item_size, slot);
    }
    ItemAnchors {
        table,
        chunks,
        max_chunks,
        per_chunk,
    }
}

/// Writes the six fields of an object array structure at `base`.
fn write_array_fields(
    mem: &mut Mem,
    profile: &Profile,
    base: u64,
    items: &ItemAnchors,
    max_elements: u32,
    num_elements: u32,
) {
    let layout = &profile.layout.objects;
    mem.put_u64(base + u64::from(layout.objects), items.table);
    if let Some(prealloc) = profile.shape.object_array_prealloc {
        mem.put_u64(base + u64::from(prealloc), 0);
    }
    mem.put_u32(base + u64::from(layout.max_elements), max_elements);
    mem.put_u32(base + u64::from(layout.num_elements), num_elements);
    if let (Some(max_chunks), Some(num_chunks)) = (layout.max_chunks, layout.num_chunks) {
        mem.put_u32(base + u64::from(max_chunks), items.max_chunks);
        mem.put_u32(
            base + u64::from(num_chunks),
            u32::try_from(items.chunks.len()).unwrap_or(0),
        );
    }
}

#[derive(Debug, Clone, Copy)]
enum Component {
    Array,
    Names,
    Globals,
    DecoyArray,
    DecoyCounters,
    DecoyNameSlot,
    StaleSlots,
    Noise,
}

/// Writes `.data`.
///
/// `names` must already contain every name; the table is written here.
#[allow(
    clippy::too_many_lines,
    reason = "one pass laying out the components in shuffled order"
)]
pub fn write_data(
    mem: &mut Mem,
    profile: &Profile,
    reflection: &Reflection,
    names: &mut NameTable,
    code: &Code,
    rng: &mut Rng,
) -> DataAnchors {
    let mut anchors = DataAnchors::default();
    let layout = profile.layout.objects.clone();
    let shape = profile.shape.clone();
    let pool = NoisePool {
        heap: reflection
            .records
            .iter()
            .step_by(7)
            .map(|r| r.address)
            .collect(),
        image: code.functions().to_vec(),
    };

    // The real items.
    let per_chunk = match layout.kind {
        ObjectArrayKind::Chunked { elements_per_chunk } => elements_per_chunk,
        ObjectArrayKind::Fixed => 0,
    };
    let max_chunks = shape
        .object_array_capacity
        .checked_div(per_chunk)
        .unwrap_or(0);
    anchors.items = write_items(
        mem,
        profile,
        &reflection.slots,
        per_chunk,
        max_chunks,
        shape.object_array_capacity,
    );

    // Decoy items: a smaller array whose slot i points at some other
    // object, so the self-index check fails for every item.
    let decoy_slots: Vec<Slot> = (0..0x400usize.min(reflection.slots.len()))
        .map(|i| {
            let other = (i * 7 + 3) % reflection.slots.len().max(1);
            let mut slot = reflection.slots.get(other).copied().unwrap_or_default();
            if slot.object == 0 {
                slot = reflection.slots.first().copied().unwrap_or_default();
            }
            slot
        })
        .collect();
    let decoy_per_chunk = if per_chunk == 0 { 0 } else { 0x1000 };
    let decoy_max_chunks = if per_chunk == 0 { 0 } else { 4 };
    anchors.decoy_items = write_items(
        mem,
        profile,
        &decoy_slots,
        decoy_per_chunk,
        decoy_max_chunks,
        0x1000,
    );

    // A decoy run of pointers to heap buffers that are not name blocks.
    for _ in 0..4 {
        let buffer = mem.heap.alloc(Pool::Names, 0x1000, 16);
        let mut bytes = vec![0u8; 0x1000];
        rng.fill(&mut bytes);
        // Printable-looking, but never starting with a name entry for `None`.
        for byte in bytes.iter_mut().take(64) {
            *byte = b'A' + (*byte % 26);
        }
        mem.put(buffer, &bytes);
        anchors.decoy_blocks.push(buffer);
    }

    // The decoy name table: a real structure whose first entry is `None` and
    // whose second is not `ByteProperty`.
    let mut decoy_names = NameTable::new(&profile.layout.names, &profile.layout.fname, &shape);
    let _ = decoy_names.intern("None");
    let _ = decoy_names.intern("ByteBanana");
    let decoy_anchor = match &profile.layout.names {
        NameTableLayout::Pool(_) => mem.heap.alloc(Pool::Names, decoy_names.anchor_size(), 16),
        NameTableLayout::Array(_) => mem.data.alloc(8, 8),
    };
    let decoy_written = decoy_names.write(mem, decoy_anchor);
    anchors.decoy_name_target = match &profile.layout.names {
        NameTableLayout::Pool(_) => decoy_written.blocks.first().copied().unwrap_or(0),
        NameTableLayout::Array(_) => decoy_written.table_object,
    };

    let mut order = vec![
        Component::Array,
        Component::Names,
        Component::Globals,
        Component::DecoyArray,
        Component::DecoyCounters,
        Component::DecoyNameSlot,
        Component::StaleSlots,
        Component::Noise,
        Component::Noise,
        Component::Noise,
    ];
    rng.shuffle(&mut order);

    let first = mem.data.alloc(0x100, 16);
    noise(mem, rng, &pool, first, 0x100);
    for component in order {
        let gap = usize::try_from(rng.range(1, 24) * 8).unwrap_or(8);
        let gap_addr = mem.data.alloc(gap, 8);
        noise(mem, rng, &pool, gap_addr, gap);
        match component {
            Component::Array => {
                // Prefix (GC bookkeeping), the array, then critical sections
                // and listener arrays.
                let prefix = 0x10usize;
                let size = prefix + usize::try_from(shape.object_array_size).unwrap_or(0x20) + 0x60;
                let start = mem.data.alloc(size, 16);
                let count = u32::try_from(reflection.slots.len()).unwrap_or(0);
                mem.put_u32(start, 0);
                mem.put_u32(start + 4, count / 3);
                mem.put_u32(start + 8, count / 3);
                let base = start + prefix as u64;
                anchors.object_array = base;
                write_array_fields(
                    mem,
                    profile,
                    base,
                    &anchors.items,
                    shape.object_array_capacity,
                    count,
                );
                let suffix = base + u64::from(shape.object_array_size);
                noise(mem, rng, &pool, suffix, 0x60);
            }
            Component::Names => {
                let size = names.anchor_size();
                let start = mem.data.alloc(size, 16);
                anchors.name_anchor = start;
                anchors.names = names.write(mem, start);
            }
            Component::Globals => {
                let start = mem.data.alloc(0x60, 16);
                noise(mem, rng, &pool, start, 0x60);
                let mut slots = [0u64, 0x18, 0x30];
                rng.shuffle(&mut slots);
                anchors.gworld = start + slots[0];
                anchors.gengine = start + slots[1];
                anchors.frame_counter = start + slots[2];
                mem.put_u64(anchors.gworld, reflection.world);
                mem.put_u64(anchors.gengine, reflection.engine);
                mem.put_u64(anchors.frame_counter, 0);
            }
            Component::DecoyArray => {
                let size = usize::try_from(shape.object_array_size).unwrap_or(0x20);
                let base = mem.data.alloc(size + 0x10, 16);
                anchors.decoy_array = base;
                let count = u32::try_from(decoy_slots.len()).unwrap_or(0);
                let max = if per_chunk == 0 { 0x1000 } else { 0x4000 };
                write_array_fields(mem, profile, base, &anchors.decoy_items, max, count);
            }
            Component::DecoyCounters => {
                // [lock][current_block][cursor][block pointers...]
                let start = mem.data.alloc(0x10 + 8 * 8, 16);
                anchors.decoy_counters = start;
                let blocks = anchors.decoy_blocks.clone();
                let (current, cursor, run) = match &profile.layout.names {
                    NameTableLayout::Pool(pool) => (pool.current_block, pool.cursor, pool.blocks),
                    NameTableLayout::Array(_) => (8, 0xC, 0x10),
                };
                mem.put_u32(
                    start + u64::from(current),
                    u32::try_from(blocks.len() - 1).unwrap_or(0),
                );
                mem.put_u32(start + u64::from(cursor), 0x2A40);
                for (i, block) in blocks.iter().enumerate() {
                    mem.put_u64(start + u64::from(run) + i as u64 * 8, *block);
                }
            }
            Component::DecoyNameSlot => {
                let slot = mem.data.alloc(8, 8);
                anchors.decoy_name_slot = slot;
                mem.put_u64(slot, anchors.decoy_name_target);
            }
            Component::StaleSlots => {
                let start = mem.data.alloc(0x30, 16);
                anchors.stale_world_slot = start;
                anchors.world_cdo_slot = start + 0x10;
                anchors.engine_cdo_slot = start + 0x20;
                mem.put_u64(start, reflection.stale_world);
                let world_cdo = reflection
                    .addr
                    .get("/Script/Engine.Default__World")
                    .copied()
                    .unwrap_or(0);
                let engine_cdo = reflection
                    .addr
                    .get("/Script/Engine.Default__GameEngine")
                    .copied()
                    .unwrap_or(0);
                mem.put_u64(start + 0x10, world_cdo);
                mem.put_u64(start + 0x20, engine_cdo);
            }
            Component::Noise => {
                let size = usize::try_from(rng.range(0x40, 0x400) & !7).unwrap_or(0x40);
                let start = mem.data.alloc(size, 16);
                noise(mem, rng, &pool, start, size);
            }
        }
    }
    anchors
}
