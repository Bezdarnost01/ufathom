//! The address space an image is written into: image sections and a heap made
//! of separate segments with unmapped gaps between them.
//!
//! All writes are by absolute address. Because every pointer inside an image
//! is an absolute address, the final [`Placement`] must be known before bytes
//! are written. Sizes never depend on addresses, so a caller that needs to
//! learn the sizes first (the fake host) builds once with a dummy placement.

use ufathom_mem::{Perms, RegionKind};

/// Page size used for region rounding.
pub const PAGE: u64 = 0x1000;

/// Allocation granularity of the operating system. Heap segments are spaced
/// by multiples of it, so each can be a separate allocation.
pub const GRANULARITY: u64 = 0x1_0000;

/// Capacity of `.text`.
pub const TEXT_CAP: usize = 0x2_0000;
/// Capacity of `.rdata`.
pub const RDATA_CAP: usize = 0x4_0000;
/// Capacity of `.data`.
pub const DATA_CAP: usize = 0x4_0000;

/// RVA of `.text` in a fake PE module.
pub const TEXT_RVA: u64 = 0x1000;
/// RVA of `.rdata` in a fake PE module.
pub const RDATA_RVA: u64 = TEXT_RVA + TEXT_CAP as u64;
/// RVA of `.data` in a fake PE module.
pub const DATA_RVA: u64 = RDATA_RVA + RDATA_CAP as u64;
/// RVA of `.rsrc` in a fake PE module.
pub const RSRC_RVA: u64 = DATA_RVA + DATA_CAP as u64;

/// Reserved size of an ordinary heap segment.
const SEGMENT_CAP: usize = 0x4_0000;

/// Where the parts of an image go.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Placement {
    /// A fake PE module at a seed-derived base plus a heap of `Private`
    /// regions, all described by a [`ufathom_mem::Snapshot`].
    Snapshot,
    /// Addresses chosen by a host process that owns the memory. There are no
    /// PE headers: the host's own executable provides them.
    Hosted(HostedPlacement),
}

/// Addresses supplied by a hosting process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostedPlacement {
    /// File name of the real module that contains the three image parts.
    pub module_name: String,
    /// Load address of that module.
    pub module_base: u64,
    /// Address of at least [`TEXT_CAP`] bytes standing in for `.text`.
    pub text: u64,
    /// Address of at least [`RDATA_CAP`] bytes standing in for `.rdata`.
    pub rdata: u64,
    /// Address of at least [`DATA_CAP`] bytes standing in for `.data`.
    pub data: u64,
    /// Base of the reserved heap range of `Fixture::heap_span` bytes.
    pub heap: u64,
}

/// A resolved placement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Addresses {
    /// Module file name.
    pub module_name: String,
    /// Module load address.
    pub module_base: u64,
    /// Start of `.text`.
    pub text: u64,
    /// Start of `.rdata`.
    pub rdata: u64,
    /// Start of `.data`.
    pub data: u64,
    /// Start of `.rsrc` (zero when hosted).
    pub rsrc: u64,
    /// Start of the heap range.
    pub heap: u64,
}

impl Addresses {
    /// The RVA of an address inside the module.
    #[must_use]
    pub fn rva(&self, addr: u64) -> u64 {
        addr.wrapping_sub(self.module_base)
    }
}

/// What a part of the image is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum PartKind {
    /// PE headers (snapshot mode only).
    Headers,
    /// Code.
    Text,
    /// Read-only data.
    Rdata,
    /// Writable globals.
    Data,
    /// Resources (snapshot mode only).
    Rsrc,
    /// One heap segment.
    Heap,
}

/// A contiguous block of the finished image.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Part {
    /// What it is.
    pub kind: PartKind,
    /// Absolute address.
    pub addr: u64,
    /// Contents. The length is a multiple of the page size.
    pub bytes: Vec<u8>,
    /// Access rights.
    pub perms: Perms,
    /// Backing kind.
    pub region_kind: RegionKind,
}

/// A bump allocator over one fixed range (an image section).
#[derive(Debug)]
pub struct Arena {
    base: u64,
    cap: usize,
    bytes: Vec<u8>,
}

impl Arena {
    fn new(base: u64, cap: usize) -> Self {
        Self {
            base,
            cap,
            bytes: Vec::new(),
        }
    }

    /// Reserves `size` zeroed bytes aligned to `align` and returns the address.
    ///
    /// # Panics
    ///
    /// If the section capacity is exceeded; that is a build-time mistake.
    #[allow(
        clippy::expect_used,
        reason = "exceeding a fixed capacity is a build-time mistake"
    )]
    pub fn alloc(&mut self, size: usize, align: usize) -> u64 {
        let start = self.bytes.len().next_multiple_of(align.max(1));
        let end = start + size;
        assert!(
            end <= self.cap,
            "section at {:#x} is full: needs {end:#x} of {:#x}",
            self.base,
            self.cap
        );
        self.bytes.resize(end, 0);
        self.base + u64::try_from(start).expect("usize fits u64")
    }

    /// The address the next allocation with this alignment would get.
    #[must_use]
    pub fn next_address(&self, align: usize) -> u64 {
        self.base + self.bytes.len().next_multiple_of(align.max(1)) as u64
    }

    /// Current used length.
    #[must_use]
    pub fn len(&self) -> usize {
        self.bytes.len()
    }

    fn contains(&self, addr: u64) -> bool {
        addr >= self.base && addr - self.base < self.cap as u64
    }
}

/// Which heap pool an allocation belongs to. Pools are kept in separate
/// segments, like separate allocators would.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Pool {
    /// Name table blocks and entries.
    Names,
    /// Object array chunks.
    Items,
    /// Reflection objects and instances.
    Objects,
}

const POOL_COUNT: usize = 3;

impl Pool {
    fn slot(self) -> usize {
        match self {
            Self::Names => 0,
            Self::Items => 1,
            Self::Objects => 2,
        }
    }
}

#[derive(Debug)]
struct Segment {
    offset: u64,
    cap: usize,
    bytes: Vec<u8>,
}

/// The finished set of heap segments.
#[derive(Debug)]
pub struct Heap {
    base: u64,
    segments: Vec<Segment>,
    next_offset: u64,
    open: [Option<usize>; POOL_COUNT],
}

impl Heap {
    fn new(base: u64) -> Self {
        Self {
            base,
            segments: Vec::new(),
            // A leading gap, so address `base` itself is never mapped.
            next_offset: GRANULARITY,
            open: [None; POOL_COUNT],
        }
    }

    fn open_segment(&mut self, cap: usize) -> usize {
        let offset = self.next_offset;
        self.next_offset = offset + (cap as u64).next_multiple_of(GRANULARITY) + GRANULARITY;
        self.segments.push(Segment {
            offset,
            cap,
            bytes: Vec::new(),
        });
        self.segments.len() - 1
    }

    /// Allocates `size` zeroed bytes in a pool.
    #[allow(clippy::expect_used, reason = "arithmetic on build-time constants")]
    pub fn alloc(&mut self, pool: Pool, size: usize, align: usize) -> u64 {
        let align = align.max(1);
        let large = size > SEGMENT_CAP / 2;
        let existing = if large {
            None
        } else {
            self.open.get(pool.slot()).copied().flatten()
        };
        let mut index = existing;
        if let Some(i) = existing {
            let segment = self.segments.get(i).expect("open segment exists");
            if segment.bytes.len().next_multiple_of(align) + size > segment.cap {
                index = None;
            }
        }
        let index = match index {
            Some(i) => i,
            None => {
                let cap = if large {
                    size.next_multiple_of(PAGE as usize) + PAGE as usize
                } else {
                    SEGMENT_CAP
                };
                let i = self.open_segment(cap);
                if !large {
                    if let Some(slot) = self.open.get_mut(pool.slot()) {
                        *slot = Some(i);
                    }
                }
                i
            }
        };
        let base = self.base;
        let segment = self.segments.get_mut(index).expect("segment exists");
        let start = segment.bytes.len().next_multiple_of(align);
        segment.bytes.resize(start + size, 0);
        base + segment.offset + u64::try_from(start).expect("usize fits u64")
    }

    /// Allocates `size` bytes in a segment of their own that ends exactly at a
    /// page boundary, so reading just past the object leaves mapped memory.
    #[allow(clippy::expect_used, reason = "arithmetic on build-time constants")]
    pub fn alloc_at_page_end(&mut self, size: usize) -> u64 {
        let cap = size.next_multiple_of(PAGE as usize);
        let index = self.open_segment(cap);
        let base = self.base;
        let segment = self.segments.get_mut(index).expect("segment exists");
        segment.bytes.resize(cap, 0);
        base + segment.offset + u64::try_from(cap - size).expect("usize fits u64")
    }

    /// Total bytes of address space to reserve for the heap, including an
    /// unmapped tail.
    #[must_use]
    pub fn span(&self) -> u64 {
        self.next_offset + 2 * GRANULARITY
    }

    /// An address inside the reserved range that no segment covers.
    #[must_use]
    pub fn unmapped_address(&self) -> u64 {
        self.base + self.next_offset + GRANULARITY
    }
}

/// The writable address space of an image under construction.
#[derive(Debug)]
pub struct Mem {
    /// Resolved placement.
    pub addrs: Addresses,
    /// `.text` allocator.
    pub text: Arena,
    /// `.rdata` allocator.
    pub rdata: Arena,
    /// `.data` allocator.
    pub data: Arena,
    /// The heap.
    pub heap: Heap,
}

impl Mem {
    /// Creates an empty address space at `addrs`.
    #[must_use]
    pub fn new(addrs: Addresses) -> Self {
        Self {
            text: Arena::new(addrs.text, TEXT_CAP),
            rdata: Arena::new(addrs.rdata, RDATA_CAP),
            data: Arena::new(addrs.data, DATA_CAP),
            heap: Heap::new(addrs.heap),
            addrs,
        }
    }

    /// Mutable bytes at `addr..addr + len`.
    ///
    /// # Panics
    ///
    /// If the range is not inside one allocated block. The builder only writes
    /// memory it allocated, so this is a build-time mistake.
    #[allow(
        clippy::expect_used,
        reason = "writing outside allocated memory is a build-time mistake"
    )]
    pub fn bytes_mut(&mut self, addr: u64, len: usize) -> &mut [u8] {
        for arena in [&mut self.text, &mut self.rdata, &mut self.data] {
            if arena.contains(addr) {
                let start = usize::try_from(addr - arena.base).expect("offset fits usize");
                return arena.bytes.get_mut(start..start + len).unwrap_or_else(|| {
                    panic!("write of {len} bytes at {addr:#x} is past the section")
                });
            }
        }
        let heap_base = self.heap.base;
        let after = self
            .heap
            .segments
            .partition_point(|segment| heap_base + segment.offset <= addr);
        let segment = after
            .checked_sub(1)
            .and_then(|i| self.heap.segments.get_mut(i))
            .unwrap_or_else(|| panic!("address {addr:#x} is not in the image"));
        let start = usize::try_from(addr - heap_base - segment.offset).expect("offset fits usize");
        segment
            .bytes
            .get_mut(start..start + len)
            .unwrap_or_else(|| panic!("write of {len} bytes at {addr:#x} is past its segment"))
    }

    /// Copies `bytes` to `addr`.
    pub fn put(&mut self, addr: u64, bytes: &[u8]) {
        self.bytes_mut(addr, bytes.len()).copy_from_slice(bytes);
    }

    /// Writes one byte.
    pub fn put_u8(&mut self, addr: u64, value: u8) {
        self.put(addr, &[value]);
    }

    /// Writes a little-endian `u16`.
    pub fn put_u16(&mut self, addr: u64, value: u16) {
        self.put(addr, &value.to_le_bytes());
    }

    /// Writes a little-endian `u32`.
    pub fn put_u32(&mut self, addr: u64, value: u32) {
        self.put(addr, &value.to_le_bytes());
    }

    /// Writes a little-endian `u64`.
    pub fn put_u64(&mut self, addr: u64, value: u64) {
        self.put(addr, &value.to_le_bytes());
    }

    /// Reads `len` bytes.
    pub fn get(&mut self, addr: u64, len: usize) -> Vec<u8> {
        self.bytes_mut(addr, len).to_vec()
    }

    /// Reads a little-endian `u32`.
    pub fn get_u32(&mut self, addr: u64) -> u32 {
        let mut raw = [0u8; 4];
        raw.copy_from_slice(self.bytes_mut(addr, 4));
        u32::from_le_bytes(raw)
    }

    /// Reads a little-endian `u64`.
    pub fn get_u64(&mut self, addr: u64) -> u64 {
        let mut raw = [0u8; 8];
        raw.copy_from_slice(self.bytes_mut(addr, 8));
        u64::from_le_bytes(raw)
    }

    /// Appends bytes to `.text` and returns their address.
    pub fn emit_text(&mut self, bytes: &[u8], align: usize) -> u64 {
        let addr = self.text.alloc(bytes.len(), align);
        self.put(addr, bytes);
        addr
    }

    /// Splits the finished space into parts. Section parts are rounded up to a
    /// whole page.
    #[must_use]
    pub fn into_parts(self) -> (Vec<Part>, u64, u64) {
        let span = self.heap.span();
        let unmapped = self.heap.unmapped_address();
        let mut parts = Vec::new();
        let sections = [
            (PartKind::Text, self.text, Perms::RX),
            (PartKind::Rdata, self.rdata, Perms::R),
            (PartKind::Data, self.data, Perms::RW),
        ];
        for (kind, arena, perms) in sections {
            let mut bytes = arena.bytes;
            let rounded = bytes
                .len()
                .next_multiple_of(PAGE as usize)
                .max(PAGE as usize);
            bytes.resize(rounded, 0);
            parts.push(Part {
                kind,
                addr: arena.base,
                bytes,
                perms,
                region_kind: RegionKind::Image,
            });
        }
        let base = self.heap.base;
        for segment in self.heap.segments {
            let mut bytes = segment.bytes;
            let rounded = bytes
                .len()
                .next_multiple_of(PAGE as usize)
                .max(PAGE as usize);
            bytes.resize(rounded, 0);
            parts.push(Part {
                kind: PartKind::Heap,
                addr: base + segment.offset,
                bytes,
                perms: Perms::RW,
                region_kind: RegionKind::Private,
            });
        }
        (parts, span, unmapped)
    }
}
