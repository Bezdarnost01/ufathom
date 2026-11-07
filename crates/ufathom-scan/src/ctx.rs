//! The state shared by every solving step: the reader, the module, local
//! copies of writable data, and the evidence recorder.

use std::cell::RefCell;

use ufathom_mem::{MemoryReader, MemoryRegion, PAGE_SIZE, PageCache};

use crate::error::ScanError;
use crate::limits::{LOCAL_READ_CHUNK, MAX_CANDIDATE_SLOTS, MAX_LOCAL_DATA_BYTES};
use crate::options::DiscoverOptions;
use crate::result::Evidence;

/// Highest user-mode address a pointer may hold on 64-bit Windows and Linux.
const USER_ADDRESS_LIMIT: u64 = 1 << 47;

/// Lowest address a valid pointer may hold. The first 64 KiB are never mapped.
const USER_ADDRESS_FLOOR: u64 = 0x1_0000;

/// A span of writable module data copied out of the target once.
#[derive(Debug)]
pub(crate) struct LocalRange {
    pub base: u64,
    pub bytes: Vec<u8>,
}

/// Everything a solving step needs to read the target and record results.
pub(crate) struct Ctx<'a> {
    cache: PageCache<&'a dyn MemoryReader>,
    pub options: &'a DiscoverOptions<'a>,
    pub module_name: String,
    pub module_base: u64,
    pub module_size: u64,
    /// Address ranges of every loaded module.
    images: Vec<(u64, u64)>,
    /// Merged readable intervals of the target.
    readable: Vec<(u64, u64)>,
    /// Writable data of the module, in address order.
    pub locals: Vec<LocalRange>,
    /// Regions of the target that are not part of the module and are writable.
    pub other_writable: Vec<MemoryRegion>,
    evidence: RefCell<Vec<Evidence>>,
    warnings: RefCell<Vec<String>>,
}

fn file_name(path: &str) -> &str {
    path.rsplit(['\\', '/']).next().unwrap_or(path)
}

fn merge(mut spans: Vec<(u64, u64)>) -> Vec<(u64, u64)> {
    spans.sort_unstable();
    let mut merged: Vec<(u64, u64)> = Vec::with_capacity(spans.len());
    for (start, end) in spans {
        match merged.last_mut() {
            Some((_, last)) if start <= *last => *last = (*last).max(end),
            _ => merged.push((start, end)),
        }
    }
    merged
}

impl<'a> Ctx<'a> {
    /// Selects the module, lists the regions and copies the module's writable
    /// data into local buffers.
    pub(crate) fn new(
        mem: &'a dyn MemoryReader,
        options: &'a DiscoverOptions<'a>,
    ) -> Result<Self, ScanError> {
        let modules = mem.modules().map_err(|source| ScanError::Target {
            what: "module list",
            source,
        })?;
        let wanted = options.module.as_deref().map(file_name);
        let module = match wanted {
            Some(name) => modules
                .iter()
                .find(|m| file_name(&m.name).eq_ignore_ascii_case(name)),
            None => modules
                .iter()
                .find(|m| file_name(&m.name).to_ascii_lowercase().ends_with(".exe")),
        }
        .ok_or_else(|| ScanError::ModuleNotFound {
            requested: options.module.clone(),
        })?;
        let regions = mem.regions().map_err(|source| ScanError::Target {
            what: "region list",
            source,
        })?;
        let module_end = module.base.saturating_add(module.size);
        let readable = merge(
            regions
                .iter()
                .filter(|r| r.perms.read)
                .map(|r| (r.base, r.base.saturating_add(r.size)))
                .collect(),
        );
        let images = modules
            .iter()
            .map(|m| (m.base, m.base.saturating_add(m.size)))
            .collect();
        let other_writable = regions
            .iter()
            .filter(|r| r.perms.read && r.perms.write)
            .filter(|r| r.base >= module_end || r.base.saturating_add(r.size) <= module.base)
            .cloned()
            .collect();
        let mut ctx = Self {
            cache: PageCache::new(mem),
            options,
            module_name: file_name(&module.name).to_owned(),
            module_base: module.base,
            module_size: module.size,
            images,
            readable,
            locals: Vec::new(),
            other_writable,
            evidence: RefCell::new(Vec::new()),
            warnings: RefCell::new(Vec::new()),
        };
        ctx.load_locals(mem, &regions, module_end);
        if ctx.locals.is_empty() {
            return Err(ScanError::NoWritableData {
                module: ctx.module_name.clone(),
            });
        }
        Ok(ctx)
    }

    /// Copies each writable span of the module once, with a few large reads.
    fn load_locals(&mut self, mem: &dyn MemoryReader, regions: &[MemoryRegion], module_end: u64) {
        let mut budget = MAX_LOCAL_DATA_BYTES;
        for region in regions.iter().filter(|r| r.perms.read && r.perms.write) {
            let start = region.base.max(self.module_base);
            let end = region.base.saturating_add(region.size).min(module_end);
            if start >= end || budget == 0 {
                continue;
            }
            let end = end.min(start.saturating_add(budget));
            let len = usize::try_from(end - start).unwrap_or(0);
            let mut bytes = vec![0u8; len];
            let mut done = 0usize;
            while done < len {
                let take = LOCAL_READ_CHUNK.min(len - done);
                let at = start + done as u64;
                let Some(slice) = bytes.get_mut(done..done + take) else {
                    break;
                };
                if mem.read_exact(at, slice).is_err() {
                    // A guarded or unmapped page inside the span: read page by
                    // page and leave the unreadable ones zero.
                    for (n, page) in slice.chunks_mut(PAGE_SIZE).enumerate() {
                        let _ = mem.read_exact(at + (n * PAGE_SIZE) as u64, page);
                    }
                }
                done += take;
            }
            budget = budget.saturating_sub(len as u64);
            self.locals.push(LocalRange { base: start, bytes });
        }
        self.locals.sort_by_key(|range| range.base);
    }

    /// The reader the typed views of `ufathom-engine` should use.
    pub(crate) fn reader(&self) -> &dyn MemoryReader {
        &self.cache
    }

    // ----- memory access -----

    fn local_slice(&self, addr: u64, len: usize) -> Option<&[u8]> {
        let after = self.locals.partition_point(|range| range.base <= addr);
        let range = self.locals.get(after.checked_sub(1)?)?;
        let offset = usize::try_from(addr.checked_sub(range.base)?).ok()?;
        range.bytes.get(offset..offset.checked_add(len)?)
    }

    /// Reads `len` bytes. `None` when any part is unreadable.
    pub(crate) fn read(&self, addr: u64, len: usize) -> Option<Vec<u8>> {
        if let Some(slice) = self.local_slice(addr, len) {
            return Some(slice.to_vec());
        }
        let mut buf = vec![0u8; len];
        self.cache.read_exact(addr, &mut buf).ok()?;
        Some(buf)
    }

    /// Reads as many leading bytes as are readable, up to `max`, a page at a
    /// time. Used for blocks whose length is learned while parsing them.
    pub(crate) fn read_prefix(&self, addr: u64, max: usize) -> Vec<u8> {
        let mut out = Vec::new();
        let mut cursor = addr;
        while out.len() < max {
            let to_page = PAGE_SIZE - (cursor as usize % PAGE_SIZE);
            let take = to_page.min(max - out.len());
            match self.read(cursor, take) {
                Some(bytes) => out.extend_from_slice(&bytes),
                None => break,
            }
            cursor = cursor.wrapping_add(take as u64);
        }
        out
    }

    pub(crate) fn u32(&self, addr: u64) -> Option<u32> {
        le_u32(&self.read(addr, 4)?, 0)
    }

    pub(crate) fn u64(&self, addr: u64) -> Option<u64> {
        le_u64(&self.read(addr, 8)?, 0)
    }

    // ----- address classification -----

    /// Whether the byte at `addr` is readable.
    pub(crate) fn is_mapped(&self, addr: u64) -> bool {
        let after = self.readable.partition_point(|&(start, _)| start <= addr);
        after
            .checked_sub(1)
            .and_then(|i| self.readable.get(i))
            .is_some_and(|&(_, end)| addr < end)
    }

    /// Contiguous readable bytes starting at `addr`.
    pub(crate) fn readable_extent(&self, addr: u64) -> u64 {
        let after = self.readable.partition_point(|&(start, _)| start <= addr);
        after
            .checked_sub(1)
            .and_then(|i| self.readable.get(i))
            .map_or(0, |&(_, end)| end.saturating_sub(addr))
    }

    /// Whether `value` looks like a pointer to readable user memory.
    pub(crate) fn is_pointer(&self, value: u64) -> bool {
        (USER_ADDRESS_FLOOR..USER_ADDRESS_LIMIT).contains(&value) && self.is_mapped(value)
    }

    /// Whether `addr` is inside any loaded module.
    pub(crate) fn in_image(&self, addr: u64) -> bool {
        self.images
            .iter()
            .any(|&(start, end)| addr >= start && addr < end)
    }

    /// Whether `addr` is inside the solved module.
    pub(crate) fn in_module(&self, addr: u64) -> bool {
        addr >= self.module_base && addr - self.module_base < self.module_size
    }

    /// A layout location for `addr`: an RVA inside the module, an absolute
    /// address elsewhere.
    pub(crate) fn location(&self, addr: u64) -> ufathom_model::layout::Location {
        if self.in_module(addr) {
            ufathom_model::layout::Location::Rva {
                module: self.module_name.clone(),
                rva: self.rva(addr),
            }
        } else {
            ufathom_model::layout::Location::Absolute(addr)
        }
    }

    /// The RVA of an address inside the module.
    pub(crate) fn rva(&self, addr: u64) -> u64 {
        addr.wrapping_sub(self.module_base)
    }

    /// Every 8-aligned slot of the module's writable data that holds a pointer
    /// to readable memory, as `(slot address, pointer)`. With `transform` the
    /// user's pointer transform is applied to each stored value first.
    pub(crate) fn pointer_slots(&self, transform: bool) -> Vec<(u64, u64)> {
        let mut slots = Vec::new();
        'ranges: for range in &self.locals {
            let mut offset = 0usize;
            while let Some(stored) = le_u64(&range.bytes, offset) {
                if stored != 0 {
                    let pointer = if transform {
                        self.options
                            .pointer_transform
                            .iter()
                            .fold(stored, |value, op| op.apply(value))
                    } else {
                        stored
                    };
                    if self.is_pointer(pointer) {
                        slots.push((range.base + offset as u64, pointer));
                        if slots.len() >= MAX_CANDIDATE_SLOTS {
                            break 'ranges;
                        }
                    }
                }
                offset += 8;
            }
        }
        slots
    }

    // ----- recording -----

    /// Records how a value was solved.
    pub(crate) fn evidence(
        &self,
        field: &str,
        value: u64,
        method: impl Into<String>,
        samples: usize,
        matched: usize,
    ) {
        let record = Evidence {
            field: field.to_owned(),
            value,
            method: method.into(),
            samples: u32::try_from(samples).unwrap_or(u32::MAX),
            matched: u32::try_from(matched).unwrap_or(u32::MAX),
        };
        self.evidence.borrow_mut().push(record);
    }

    /// Records a weak result.
    pub(crate) fn warn(&self, message: impl Into<String>) {
        let message = message.into();
        tracing::warn!("{message}");
        self.warnings.borrow_mut().push(message);
    }

    /// Takes the records out of the context.
    pub(crate) fn take_records(&self) -> (Vec<Evidence>, Vec<String>) {
        (
            std::mem::take(&mut *self.evidence.borrow_mut()),
            std::mem::take(&mut *self.warnings.borrow_mut()),
        )
    }

    // ----- overrides -----

    /// Combines a solved offset with the caller's override for `field`.
    ///
    /// - A contradiction is an error naming the field, the given value and
    ///   the value found.
    /// - An override with nothing solved is used, with a warning.
    /// - Nothing solved and no override is a [`ScanError::MissingEvidence`].
    pub(crate) fn resolve(
        &self,
        field: &'static str,
        solved: Option<u32>,
        missing: impl FnOnce() -> String,
    ) -> Result<u32, ScanError> {
        match (self.options.overrides.get(field), solved) {
            (Some(given), Some(found)) if given != found => Err(ScanError::OverrideContradiction {
                field,
                given: u64::from(given),
                found: u64::from(found),
            }),
            (Some(given), None) => {
                self.warn(format!(
                    "{field} = {given:#x} comes from an override; the target gave no evidence for it"
                ));
                self.evidence(field, u64::from(given), "override, unconfirmed", 0, 0);
                Ok(given)
            }
            (_, Some(found)) => Ok(found),
            (None, None) => Err(ScanError::missing(field, missing())),
        }
    }

    /// Like [`resolve`](Self::resolve) for a field that may be absent.
    ///
    /// Nothing solved and no override is `Ok(None)`. A contradiction is an
    /// error, and an override with nothing solved is used with a warning.
    pub(crate) fn resolve_optional(
        &self,
        field: &'static str,
        solved: Option<u32>,
    ) -> Result<Option<u32>, ScanError> {
        match (self.options.overrides.get(field), solved) {
            (Some(given), Some(found)) if given != found => Err(ScanError::OverrideContradiction {
                field,
                given: u64::from(given),
                found: u64::from(found),
            }),
            (Some(given), None) => {
                self.warn(format!(
                    "{field} = {given:#x} comes from an override; the target gave no evidence for it"
                ));
                self.evidence(field, u64::from(given), "override, unconfirmed", 0, 0);
                Ok(Some(given))
            }
            (_, found) => Ok(found),
        }
    }

    /// Like [`resolve`](Self::resolve) for several equally good candidates: an
    /// override picks one of them, and an override outside the set is a
    /// contradiction.
    pub(crate) fn resolve_among(
        &self,
        field: &'static str,
        candidates: &[u32],
    ) -> Result<u32, ScanError> {
        match (self.options.overrides.get(field), candidates) {
            (Some(given), found) if found.contains(&given) => Ok(given),
            (Some(given), found) => Err(ScanError::OverrideContradiction {
                field,
                given: u64::from(given),
                found: u64::from(found.first().copied().unwrap_or(0)),
            }),
            (None, [only]) => Ok(*only),
            (None, []) => Err(ScanError::missing(field, "no candidate")),
            (None, many) => Err(ScanError::Ambiguous {
                field,
                candidates: many
                    .iter()
                    .map(|c| format!("{c:#x}"))
                    .collect::<Vec<_>>()
                    .join(", "),
            }),
        }
    }
}

/// Little-endian `u32` at `offset` in `bytes`.
pub(crate) fn le_u32(bytes: &[u8], offset: usize) -> Option<u32> {
    let slice = bytes.get(offset..offset.checked_add(4)?)?;
    <[u8; 4]>::try_from(slice).ok().map(u32::from_le_bytes)
}

/// Little-endian `u64` at `offset` in `bytes`.
pub(crate) fn le_u64(bytes: &[u8], offset: usize) -> Option<u64> {
    let slice = bytes.get(offset..offset.checked_add(8)?)?;
    <[u8; 8]>::try_from(slice).ok().map(u64::from_le_bytes)
}

/// Little-endian `u16` at `offset` in `bytes`.
pub(crate) fn le_u16(bytes: &[u8], offset: usize) -> Option<u16> {
    let slice = bytes.get(offset..offset.checked_add(2)?)?;
    <[u8; 2]>::try_from(slice).ok().map(u16::from_le_bytes)
}
