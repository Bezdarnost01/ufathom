//! The name table: `FNamePool` or `TNameEntryArray`, written the way the
//! profile's layout describes it.

use std::collections::BTreeMap;

use ufathom_model::layout::{FNameLayout, NameArrayLayout, NamePoolLayout, NameTableLayout};

use crate::profile::Shape;
use crate::rng::Rng;
use crate::space::{Mem, Pool};

/// One interned name, ready to be written.
#[derive(Debug, Clone)]
struct Entry {
    /// Characters, ANSI or UTF-16 code units already encoded as bytes.
    chars: Vec<u8>,
    /// Number of characters (not bytes).
    len: usize,
    wide: bool,
    /// Byte offset of the entry inside its pool block (pool mode).
    block: usize,
    offset: usize,
}

/// Addresses produced by [`NameTable::write`], kept as ground truth.
#[derive(Debug, Clone, Default)]
pub struct NameAnchors {
    /// Pool start (pool mode) or the `GNames` slot holding a pointer to the
    /// array (array mode).
    pub anchor: u64,
    /// Array mode: the heap `TNameEntryArray` object.
    pub table_object: u64,
    /// Pool mode: the block pointers' slots. Array mode: the chunk pointers'
    /// slots inside the `TNameEntryArray`.
    pub block_slots: Vec<u64>,
    /// Address of each block (pool) or chunk table (array).
    pub blocks: Vec<u64>,
    /// Address of each entry, by name index.
    pub entries: Vec<u64>,
}

/// Names interned so far, in index order.
#[derive(Debug)]
pub struct NameTable {
    layout: NameTableLayout,
    fname: FNameLayout,
    max_blocks: u32,
    max_chunks: u32,
    entries: Vec<Entry>,
    lookup: BTreeMap<String, u32>,
    ordinals: BTreeMap<String, usize>,
    /// Pool mode: bytes used in the current block.
    cursor: usize,
    blocks: usize,
    /// Array mode: the hash bucket heads, to chain entries like the engine.
    hash_heads: BTreeMap<u64, u64>,
}

fn fnv(text: &str) -> u64 {
    let mut hash = 0xCBF2_9CE4_8422_2325u64;
    for byte in text.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01B3);
    }
    hash
}

impl NameTable {
    /// Creates an empty table for a layout.
    #[must_use]
    pub fn new(layout: &NameTableLayout, fname: &FNameLayout, shape: &Shape) -> Self {
        Self {
            layout: layout.clone(),
            fname: fname.clone(),
            max_blocks: shape.name_pool_max_blocks,
            max_chunks: shape.name_array_max_chunks,
            entries: Vec::new(),
            lookup: BTreeMap::new(),
            ordinals: BTreeMap::new(),
            cursor: 0,
            blocks: 1,
            hash_heads: BTreeMap::new(),
        }
    }

    /// Number of pool blocks or array chunks in use.
    #[must_use]
    pub fn blocks_in_use(&self) -> usize {
        match &self.layout {
            NameTableLayout::Pool(_) => self.blocks,
            NameTableLayout::Array(array) => self
                .entries
                .len()
                .div_ceil(usize::try_from(array.elements_per_chunk).unwrap_or(1)),
        }
    }

    /// Position of `text` in write order, which indexes
    /// [`NameAnchors::entries`].
    #[must_use]
    pub fn ordinal_of(&self, text: &str) -> Option<usize> {
        self.ordinals.get(text).copied()
    }

    /// Offset of the character data inside an entry.
    #[must_use]
    pub fn string_offset(&self) -> u32 {
        match &self.layout {
            NameTableLayout::Pool(pool) => pool.entry.string_offset,
            NameTableLayout::Array(array) => array.entry.string_offset,
        }
    }

    /// Interns `text` and returns its index as stored in an `FName`.
    ///
    /// # Panics
    ///
    /// If the table is full, which only happens with an absurd name count.
    #[allow(
        clippy::expect_used,
        reason = "overflowing the table is a build-time mistake"
    )]
    pub fn intern(&mut self, text: &str) -> u32 {
        if let Some(&index) = self.lookup.get(text) {
            return index;
        }
        let wide = !text.is_ascii();
        let (chars, len) = if wide {
            let units: Vec<u16> = text.encode_utf16().collect();
            let bytes = units.iter().flat_map(|u| u.to_le_bytes()).collect();
            (bytes, units.len())
        } else {
            (text.as_bytes().to_vec(), text.len())
        };
        let mut entry = Entry {
            chars,
            len,
            wide,
            block: 0,
            offset: 0,
        };
        let index = match &self.layout {
            NameTableLayout::Pool(pool) => {
                let pool = pool.clone();
                let stride = usize::try_from(pool.stride).expect("stride fits");
                let block_bytes = stride << pool.block_offset_bits;
                let size = self.pool_entry_size(&pool, &entry);
                if self.cursor + size > block_bytes {
                    self.blocks += 1;
                    self.cursor = 0;
                    assert!(
                        self.blocks <= usize::try_from(self.max_blocks).expect("fits"),
                        "name pool is full"
                    );
                }
                entry.block = self.blocks - 1;
                entry.offset = self.cursor;
                self.cursor += size;
                let slot = u32::try_from(entry.offset / stride).expect("slot fits");
                (u32::try_from(entry.block).expect("block fits") << pool.block_offset_bits) | slot
            }
            NameTableLayout::Array(array) => {
                let total = u64::from(self.max_chunks) * u64::from(array.elements_per_chunk);
                assert!((self.entries.len() as u64) < total, "name array is full");
                u32::try_from(self.entries.len()).expect("index fits")
            }
        };
        self.ordinals.insert(text.to_owned(), self.entries.len());
        self.entries.push(entry);
        self.lookup.insert(text.to_owned(), index);
        index
    }

    fn pool_entry_size(&self, pool: &NamePoolLayout, entry: &Entry) -> usize {
        let unit = if entry.wide { 2 } else { 1 };
        let raw = usize::try_from(pool.entry.string_offset).unwrap_or(0) + entry.len * unit;
        let stride = usize::try_from(pool.stride).unwrap_or(4);
        raw.next_multiple_of(stride)
    }

    /// Adds filler names until at least `blocks` blocks (pool) or chunks
    /// (array) are in use, so the table spans several of them.
    pub fn pad_to_blocks(&mut self, blocks: usize, rng: &mut Rng) {
        let mut serial = 0u64;
        while self.blocks_in_use() < blocks || self.last_block_is_empty() {
            serial += 1;
            let length = rng.range(10, 70);
            let mut name = format!("PadName{serial:08}_");
            for _ in 0..length {
                let letter = b'a' + u8::try_from(rng.below(26)).unwrap_or(0);
                name.push(char::from(letter));
            }
            let _ = self.intern(&name);
        }
    }

    fn last_block_is_empty(&self) -> bool {
        self.cursor == 0 && matches!(self.layout, NameTableLayout::Pool(_)) && self.blocks > 1
    }

    /// The `(comparison, number)` pair to store in an `FName` for `base` with
    /// instance number `number` (0 means no suffix, N means `base_{N-1}`).
    ///
    /// A build without a number field spells the suffix into the name itself.
    pub fn fname_for(&mut self, base: &str, number: u32) -> (u32, u32) {
        if self.fname.number.is_some() || number == 0 {
            (self.intern(base), number)
        } else {
            (self.intern(&format!("{base}_{}", number - 1)), 0)
        }
    }

    /// Bytes the table needs in `.data`: the whole pool, or the `GNames` slot.
    #[must_use]
    pub fn anchor_size(&self) -> usize {
        match &self.layout {
            NameTableLayout::Pool(pool) => {
                usize::try_from(pool.blocks + self.max_blocks * 8).unwrap_or(0)
            }
            NameTableLayout::Array(_) => 8,
        }
    }

    /// Writes the table. `anchor` is an address inside `.data` with at least
    /// [`anchor_size`](Self::anchor_size) free bytes.
    #[allow(clippy::expect_used, reason = "arithmetic on build-time constants")]
    pub fn write(&mut self, mem: &mut Mem, anchor: u64) -> NameAnchors {
        let mut out = NameAnchors {
            anchor,
            ..NameAnchors::default()
        };
        match self.layout.clone() {
            NameTableLayout::Pool(pool) => self.write_pool(mem, &pool, &mut out),
            NameTableLayout::Array(array) => self.write_array(mem, &array, &mut out),
        }
        out
    }

    fn write_pool(&mut self, mem: &mut Mem, pool: &NamePoolLayout, out: &mut NameAnchors) {
        let stride = u64::from(pool.stride);
        let block_bytes = usize::try_from(stride << pool.block_offset_bits).unwrap_or(0);
        let base = out.anchor;
        for block in 0..self.blocks {
            let addr = mem.heap.alloc(Pool::Names, block_bytes, 16);
            let slot = base + u64::from(pool.blocks) + block as u64 * 8;
            mem.put_u64(slot, addr);
            out.blocks.push(addr);
            out.block_slots.push(slot);
        }
        mem.put_u32(
            base + u64::from(pool.current_block),
            u32::try_from(self.blocks - 1).unwrap_or(0),
        );
        mem.put_u32(
            base + u64::from(pool.cursor),
            u32::try_from(self.cursor).unwrap_or(0),
        );
        for (index, entry) in self.entries.iter().enumerate() {
            let block_addr = out.blocks.get(entry.block).copied().unwrap_or(0);
            let addr = block_addr + entry.offset as u64;
            let id = self.stored_index(index);
            write_pool_entry(mem, pool, entry, addr, id);
            out.entries.push(addr);
        }
    }

    /// The index value of entry number `n`.
    fn stored_index(&self, n: usize) -> u32 {
        match &self.layout {
            NameTableLayout::Pool(pool) => {
                let entry = self.entries.get(n);
                let stride = usize::try_from(pool.stride).unwrap_or(4);
                let (block, offset) = entry.map_or((0, 0), |e| (e.block, e.offset));
                let slot = u32::try_from(offset / stride).unwrap_or(0);
                (u32::try_from(block).unwrap_or(0) << pool.block_offset_bits) | slot
            }
            NameTableLayout::Array(_) => u32::try_from(n).unwrap_or(0),
        }
    }

    fn write_array(&mut self, mem: &mut Mem, array: &NameArrayLayout, out: &mut NameAnchors) {
        let per_chunk = usize::try_from(array.elements_per_chunk).unwrap_or(1);
        let chunks = self.entries.len().div_ceil(per_chunk);
        let table_size = usize::try_from(array.num_chunks + 4).unwrap_or(0).max(8);
        let table = mem.heap.alloc(Pool::Names, table_size, 16);
        out.table_object = table;
        // GNames is a pointer to a heap allocation: the layout is indirect.
        mem.put_u64(out.anchor, table);
        mem.put_u32(
            table + u64::from(array.num_elements),
            u32::try_from(self.entries.len()).unwrap_or(0),
        );
        mem.put_u32(
            table + u64::from(array.num_chunks),
            u32::try_from(chunks).unwrap_or(0),
        );
        for chunk in 0..chunks {
            let addr = mem.heap.alloc(Pool::Names, per_chunk * 8, 16);
            let slot = table + chunk as u64 * 8;
            mem.put_u64(slot, addr);
            out.blocks.push(addr);
            out.block_slots.push(slot);
        }
        for index in 0..self.entries.len() {
            let Some(entry) = self.entries.get(index).cloned() else {
                continue;
            };
            let unit = if entry.wide { 2 } else { 1 };
            let terminated = (entry.len + 1) * unit;
            let size = usize::try_from(array.entry.string_offset).unwrap_or(0x10) + terminated;
            let addr = mem.heap.alloc(Pool::Names, size, 8);
            let text_key = fnv(&String::from_utf8_lossy(&entry.chars));
            let bucket = text_key % 4096;
            let next = self.hash_heads.insert(bucket, addr).unwrap_or(0);
            let stored = (u32::try_from(index).unwrap_or(0) << 1) | u32::from(entry.wide);
            mem.put_u32(addr + u64::from(array.entry.index_offset), stored);
            // FNameEntry::HashNext sits right after the 4-byte index, aligned to 8.
            mem.put_u64(addr + 8, next);
            mem.put(addr + u64::from(array.entry.string_offset), &entry.chars);
            let chunk_table = out.blocks.get(index / per_chunk).copied().unwrap_or(0);
            mem.put_u64(chunk_table + (index % per_chunk) as u64 * 8, addr);
            out.entries.push(addr);
        }
    }
}

fn write_pool_entry(
    mem: &mut Mem,
    pool: &NamePoolLayout,
    entry: &Entry,
    addr: u64,
    comparison_id: u32,
) {
    if pool.entry.header_offset >= 4 {
        // The comparison id of this entry: itself, since names here are
        // stored with their original spelling.
        mem.put_u32(addr, comparison_id);
    }
    let probe = if pool.entry.len_shift >= 6 {
        let text = String::from_utf8_lossy(&entry.chars).to_lowercase();
        ((fnv(&text) & 0x1F) as u16) << 1
    } else {
        0
    };
    let len = u16::try_from(entry.len).unwrap_or(0x3FF);
    let header = (len << pool.entry.len_shift) | probe | u16::from(entry.wide);
    mem.put_u16(addr + u64::from(pool.entry.header_offset), header);
    mem.put(addr + u64::from(pool.entry.string_offset), &entry.chars);
}
