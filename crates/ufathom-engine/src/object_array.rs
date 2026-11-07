//! The global object array.

use ufathom_mem::{Addr, MemoryReader, MemoryReaderExt};
use ufathom_model::layout::{ObjectArrayKind, ObjectArrayLayout};

use crate::address::resolve_location;
use crate::error::EngineError;
use crate::limits::{FIXED_ARRAY_CHUNK_ITEMS, MAX_ITEM_SIZE, MAX_ITEMS_PER_CHUNK, MAX_OBJECTS};
use crate::window::Window;

/// One occupied slot of the object array.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ObjectSlot {
    /// Position in the array.
    pub index: u32,
    /// Address of the object.
    pub object: Addr,
    /// Serial number, when the layout has one.
    pub serial: Option<u32>,
}

/// The occupied slots of one bulk read.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ChunkObjects {
    /// Slots with a non-null object, in index order.
    pub objects: Vec<ObjectSlot>,
    /// Slots in the chunk whose object pointer is null.
    pub null_slots: u32,
}

/// A view of `GUObjectArray`.
///
/// The element count is read once by [`ObjectArrayView::new`] and again only
/// by [`ObjectArrayView::refresh`]. Every count and pointer is checked
/// against the capacity the target states, and a chunk count above the chunk
/// capacity is refused rather than read past.
///
/// `pointer_transform` of the layout is applied to exactly one pointer: the
/// `Objects` field read at `layout.objects`, for both kinds. For a chunked
/// array that gives the address of the chunk table, for a fixed array the
/// item storage. Entries of the chunk table and pointers to objects are used
/// as stored.
pub struct ObjectArrayView<'a> {
    mem: &'a dyn MemoryReader,
    layout: &'a ObjectArrayLayout,
    base: Addr,
    items_per_chunk: u32,
    num: u32,
    num_chunks: u32,
}

impl<'a> ObjectArrayView<'a> {
    /// Reads the array header and validates it.
    ///
    /// # Errors
    ///
    /// - [`EngineError::InvalidLayout`] for a layout that cannot describe an
    ///   array.
    /// - [`EngineError::Read`] when the header is unreadable.
    /// - [`EngineError::Limit`] when a count is above its capacity or a named
    ///   limit, including a chunk count above the chunk capacity.
    pub fn new(
        mem: &'a dyn MemoryReader,
        layout: &'a ObjectArrayLayout,
    ) -> Result<Self, EngineError> {
        let invalid = |reason: String| EngineError::InvalidLayout {
            what: "object array",
            reason,
        };
        if layout.item_size == 0 || layout.item_size > MAX_ITEM_SIZE {
            return Err(invalid(format!("item size {:#x}", layout.item_size)));
        }
        if layout.item_object.saturating_add(8) > layout.item_size
            || layout
                .item_serial
                .is_some_and(|serial| serial.saturating_add(4) > layout.item_size)
        {
            return Err(invalid("item fields lie outside the item".to_owned()));
        }
        let items_per_chunk = match layout.kind {
            ObjectArrayKind::Fixed => FIXED_ARRAY_CHUNK_ITEMS,
            ObjectArrayKind::Chunked { elements_per_chunk } => {
                if elements_per_chunk == 0 || elements_per_chunk > MAX_ITEMS_PER_CHUNK {
                    return Err(invalid(format!("{elements_per_chunk} items per chunk")));
                }
                elements_per_chunk
            }
        };
        let base = resolve_location(mem, &layout.location)?;
        let mut view = Self {
            mem,
            layout,
            base,
            items_per_chunk,
            num: 0,
            num_chunks: 0,
        };
        view.refresh()?;
        Ok(view)
    }

    /// Reads the element count again and returns it.
    ///
    /// # Errors
    ///
    /// The same as [`ObjectArrayView::new`] for the count checks. On error the
    /// view keeps its previous count.
    pub fn refresh(&mut self) -> Result<u32, EngineError> {
        let layout = self.layout;
        let mut spans = vec![(layout.max_elements, 4), (layout.num_elements, 4)];
        if let (Some(max_chunks), Some(num_chunks)) = (layout.max_chunks, layout.num_chunks) {
            spans.push((max_chunks, 4));
            spans.push((num_chunks, 4));
        }
        let window = Window::read(self.mem, self.base, "object array header", &spans)?;
        let max = window.u32(layout.max_elements)?;
        let num = window.u32(layout.num_elements)?;
        let limit_error = |what: &'static str, value: u32, limit: u64| EngineError::Limit {
            addr: self.base,
            what,
            value: u64::from(value),
            limit,
        };
        if num > max {
            return Err(limit_error(
                "object count above capacity",
                num,
                u64::from(max),
            ));
        }
        if num > MAX_OBJECTS {
            return Err(limit_error("object count", num, u64::from(MAX_OBJECTS)));
        }
        let num_chunks = match layout.kind {
            ObjectArrayKind::Fixed => 1,
            ObjectArrayKind::Chunked { .. } => {
                let (Some(max_chunks), Some(num_chunks)) = (layout.max_chunks, layout.num_chunks)
                else {
                    return Err(EngineError::InvalidLayout {
                        what: "object array",
                        reason: "a chunked array needs chunk count offsets".to_owned(),
                    });
                };
                let max_chunks = window.u32(max_chunks)?;
                let num_chunks = window.u32(num_chunks)?;
                if num_chunks > max_chunks {
                    return Err(limit_error(
                        "chunk count above chunk capacity",
                        num_chunks,
                        u64::from(max_chunks),
                    ));
                }
                let room = u64::from(num_chunks) * u64::from(self.items_per_chunk);
                if u64::from(num) > room {
                    return Err(limit_error("object count above chunk room", num, room));
                }
                num_chunks
            }
        };
        self.num = num;
        self.num_chunks = num_chunks;
        Ok(num)
    }

    /// The element count read at construction or by the last
    /// [`refresh`](Self::refresh).
    #[must_use]
    pub fn num(&self) -> u32 {
        self.num
    }

    /// Number of bulk-read chunks: the array's own chunks, or blocks of
    /// [`FIXED_ARRAY_CHUNK_ITEMS`] items for a fixed array.
    #[must_use]
    pub fn chunk_count(&self) -> u32 {
        u32::try_from(u64::from(self.num).div_ceil(u64::from(self.items_per_chunk)))
            .unwrap_or(u32::MAX)
    }

    /// Address of the storage of `chunk`.
    fn chunk_storage(&self, chunk: u32) -> Result<Addr, EngineError> {
        let table_at = self
            .base
            .checked_add(u64::from(self.layout.objects))
            .ok_or(EngineError::Inconsistent {
                addr: self.base,
                what: "object array",
                reason: "storage pointer address overflows".to_owned(),
            })?;
        let stored = self
            .mem
            .read_u64(table_at)
            .map_err(EngineError::read(table_at, "object array storage pointer"))?;
        // The one pointer the layout's transform decodes.
        let table = self.layout.apply_pointer_transform(stored);
        if table == 0 {
            return Err(EngineError::Null {
                addr: table_at,
                what: "object array storage",
            });
        }
        let entry_at = match self.layout.kind {
            ObjectArrayKind::Fixed => {
                if chunk != 0 {
                    return Err(EngineError::Limit {
                        addr: table_at,
                        what: "fixed array chunk",
                        value: u64::from(chunk),
                        limit: 0,
                    });
                }
                return Ok(table);
            }
            ObjectArrayKind::Chunked { .. } => {
                table
                    .checked_add(u64::from(chunk) * 8)
                    .ok_or(EngineError::Inconsistent {
                        addr: table,
                        what: "chunk table",
                        reason: "chunk pointer address overflows".to_owned(),
                    })?
            }
        };
        let chunk_ptr = self
            .mem
            .read_u64(entry_at)
            .map_err(EngineError::read(entry_at, "chunk pointer"))?;
        if chunk_ptr == 0 {
            return Err(EngineError::Null {
                addr: entry_at,
                what: "chunk",
            });
        }
        Ok(chunk_ptr)
    }

    /// Address of the item for slot `index`.
    ///
    /// # Errors
    ///
    /// [`EngineError::Limit`] for an index at or above [`num`](Self::num),
    /// otherwise the read errors of the chunk table.
    pub fn slot_address(&self, index: u32) -> Result<Addr, EngineError> {
        if index >= self.num {
            return Err(EngineError::Limit {
                addr: self.base,
                what: "object index",
                value: u64::from(index),
                limit: u64::from(self.num).saturating_sub(1),
            });
        }
        let chunk = index / self.items_per_chunk;
        let within = index % self.items_per_chunk;
        let storage = self.chunk_storage(chunk)?;
        let offset = u64::from(within) * u64::from(self.layout.item_size);
        storage
            .checked_add(offset)
            .ok_or(EngineError::Inconsistent {
                addr: storage,
                what: "object array item",
                reason: "item address overflows".to_owned(),
            })
    }

    /// The object in slot `index`: `None` for a null slot.
    ///
    /// Reads the chunk pointer and the item. Prefer
    /// [`objects_in_chunk`](Self::objects_in_chunk) for many slots.
    ///
    /// # Errors
    ///
    /// As [`slot_address`](Self::slot_address), and [`EngineError::Read`] for
    /// an unreadable item.
    pub fn object_at(&self, index: u32) -> Result<Option<Addr>, EngineError> {
        let item = self.slot_address(index)?;
        let at = item.checked_add(u64::from(self.layout.item_object)).ok_or(
            EngineError::Inconsistent {
                addr: item,
                what: "object array item",
                reason: "object pointer address overflows".to_owned(),
            },
        )?;
        let object = self
            .mem
            .read_u64(at)
            .map_err(EngineError::read(at, "object pointer"))?;
        Ok((object != 0).then_some(object))
    }

    /// Reads every slot of one chunk with a single request.
    ///
    /// Null slots are counted, not returned. The chunk holds at most
    /// [`MAX_ITEMS_PER_CHUNK`] items of at most [`MAX_ITEM_SIZE`] bytes, so
    /// the buffer is bounded.
    ///
    /// # Errors
    ///
    /// [`EngineError::Limit`] for a chunk at or above
    /// [`chunk_count`](Self::chunk_count), otherwise the read errors of the
    /// chunk pointer and the items. A failure affects this chunk only.
    pub fn objects_in_chunk(&self, chunk: u32) -> Result<ChunkObjects, EngineError> {
        if chunk >= self.chunk_count() {
            return Err(EngineError::Limit {
                addr: self.base,
                what: "object array chunk",
                value: u64::from(chunk),
                limit: u64::from(self.chunk_count()).saturating_sub(1),
            });
        }
        let first = u64::from(chunk) * u64::from(self.items_per_chunk);
        let count = (u64::from(self.num) - first).min(u64::from(self.items_per_chunk)) as usize;
        let storage = self.chunk_storage(chunk)?;
        let item_size = self.layout.item_size as usize;
        let mut data = vec![0u8; count * item_size];
        self.mem
            .read_exact(storage, &mut data)
            .map_err(EngineError::read(storage, "object array chunk"))?;
        let mut out = ChunkObjects::default();
        for (n, item) in data.chunks_exact(item_size).enumerate() {
            let object = u64_at(item, self.layout.item_object as usize);
            if object == 0 {
                out.null_slots += 1;
                continue;
            }
            let serial = self
                .layout
                .item_serial
                .map(|offset| u32_at(item, offset as usize));
            out.objects.push(ObjectSlot {
                index: (first + n as u64) as u32,
                object,
                serial,
            });
        }
        Ok(out)
    }
}

fn u64_at(bytes: &[u8], at: usize) -> u64 {
    bytes
        .get(at..at + 8)
        .and_then(|slice| <[u8; 8]>::try_from(slice).ok())
        .map_or(0, u64::from_le_bytes)
}

fn u32_at(bytes: &[u8], at: usize) -> u32 {
    bytes
        .get(at..at + 4)
        .and_then(|slice| <[u8; 4]>::try_from(slice).ok())
        .map_or(0, u32::from_le_bytes)
}

impl std::fmt::Debug for ObjectArrayView<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ObjectArrayView")
            .field("base", &self.base)
            .field("num", &self.num)
            .finish_non_exhaustive()
    }
}
