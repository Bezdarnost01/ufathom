//! The state shared by every solving step: the reader, the module, local
//! copies of writable data, and the evidence recorder.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

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

/// A loaded module.
#[derive(Debug, Clone)]
pub(crate) struct ModuleEntry {
    pub name: String,
    pub base: u64,
    pub size: u64,
}

impl ModuleEntry {
    fn end(&self) -> u64 {
        self.base.saturating_add(self.size)
    }
}

/// Everything a solving step needs to read the target and record results.
///
/// A target may be one executable or, in a modular build, hundreds of
/// modules with the engine's globals spread over a few of them. The context
/// lists every module in search order (the selected or main module first, then
/// those whose name says `Core`, then the rest) and holds the writable data of
/// one module at a time: [`use_module`](Self::use_module) switches it.
pub(crate) struct Ctx<'a> {
    raw: &'a dyn MemoryReader,
    cache: PageCache<&'a dyn MemoryReader>,
    pub options: &'a DiscoverOptions<'a>,
    /// Every module, in search order. Index 0 is the primary module.
    modules: Vec<ModuleEntry>,
    primary: ModuleEntry,
    /// `(base, end, index into modules)` sorted by base.
    by_base: Vec<(u64, u64, usize)>,
    /// Merged readable intervals of the target.
    readable: Vec<(u64, u64)>,
    regions: Vec<MemoryRegion>,
    /// The module whose writable data `locals` holds.
    current: Cell<usize>,
    locals: RefCell<Rc<Vec<LocalRange>>>,
    /// Modules whose data has been copied, so that a second pass over the
    /// modules does not read them again.
    loaded: RefCell<HashMap<usize, Rc<Vec<LocalRange>>>>,
    /// Bytes of module data that may still be copied.
    load_budget: Cell<u64>,
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
    /// Selects the primary module and orders the others behind it. No module
    /// data is read yet.
    pub(crate) fn new(
        mem: &'a dyn MemoryReader,
        options: &'a DiscoverOptions<'a>,
    ) -> Result<Self, ScanError> {
        let listed = mem.modules().map_err(|source| ScanError::Target {
            what: "module list",
            source,
        })?;
        let wanted = options.module.as_deref().map(file_name);
        let primary = match wanted {
            Some(name) => listed
                .iter()
                .position(|m| file_name(&m.name).eq_ignore_ascii_case(name)),
            None => listed
                .iter()
                .position(|m| file_name(&m.name).to_ascii_lowercase().ends_with(".exe")),
        }
        .ok_or_else(|| ScanError::ModuleNotFound {
            requested: options.module.clone(),
        })?;
        let regions = mem.regions().map_err(|source| ScanError::Target {
            what: "region list",
            source,
        })?;
        let readable = merge(
            regions
                .iter()
                .filter(|r| r.perms.read)
                .map(|r| (r.base, r.base.saturating_add(r.size)))
                .collect(),
        );
        let entry = |m: &ufathom_mem::ModuleInfo| ModuleEntry {
            name: file_name(&m.name).to_owned(),
            base: m.base,
            size: m.size,
        };
        let mut others: Vec<ModuleEntry> = listed
            .iter()
            .enumerate()
            .filter(|&(i, _)| i != primary)
            .map(|(_, m)| entry(m))
            .collect();
        // `Core` and `CoreUObject` hold the object array and the name table
        // in a modular build, so they come before the rest.
        others.sort_by_key(|m| (!m.name.to_ascii_lowercase().contains("core"), m.base));
        let primary_entry =
            listed
                .get(primary)
                .map(entry)
                .ok_or_else(|| ScanError::ModuleNotFound {
                    requested: options.module.clone(),
                })?;
        let mut modules = Vec::with_capacity(listed.len());
        modules.push(primary_entry.clone());
        modules.extend(others);
        let mut by_base: Vec<(u64, u64, usize)> = modules
            .iter()
            .enumerate()
            .map(|(i, m)| (m.base, m.end(), i))
            .collect();
        by_base.sort_unstable();
        Ok(Self {
            raw: mem,
            cache: PageCache::new(mem),
            options,
            modules,
            primary: primary_entry,
            by_base,
            readable,
            regions,
            current: Cell::new(0),
            locals: RefCell::new(Rc::new(Vec::new())),
            loaded: RefCell::new(HashMap::new()),
            load_budget: Cell::new(MAX_LOCAL_DATA_BYTES),
            evidence: RefCell::new(Vec::new()),
            warnings: RefCell::new(Vec::new()),
        })
    }

    /// Number of modules, in search order.
    pub(crate) fn module_count(&self) -> usize {
        self.modules.len()
    }

    /// The module at `index` in search order.
    pub(crate) fn module(&self, index: usize) -> Option<&ModuleEntry> {
        self.modules.get(index)
    }

    /// The primary module: the one the caller selected, or the main
    /// executable.
    pub(crate) fn primary(&self) -> &ModuleEntry {
        &self.primary
    }

    /// Loads the writable data of module `index` and makes it the one the
    /// slot searches look at. Returns `false` when the module has none or the
    /// budget for copying module data is spent.
    pub(crate) fn use_module(&self, index: usize) -> bool {
        let Some(module) = self.modules.get(index) else {
            return false;
        };
        self.current.set(index);
        if let Some(cached) = self.loaded.borrow().get(&index) {
            let any = !cached.is_empty();
            *self.locals.borrow_mut() = Rc::clone(cached);
            return any;
        }
        let mut ranges = Vec::new();
        let mut budget = self.load_budget.get();
        for region in self
            .regions
            .iter()
            .filter(|r| r.perms.read && r.perms.write)
        {
            let start = region.base.max(module.base);
            let end = region.base.saturating_add(region.size).min(module.end());
            if start >= end || budget == 0 {
                continue;
            }
            let end = end.min(start.saturating_add(budget));
            let bytes = self.read_scan(start, usize::try_from(end - start).unwrap_or(0));
            budget = budget.saturating_sub(bytes.len() as u64);
            ranges.push(LocalRange { base: start, bytes });
        }
        self.load_budget.set(budget);
        ranges.sort_by_key(|range| range.base);
        let any = !ranges.is_empty();
        let ranges = Rc::new(ranges);
        self.loaded.borrow_mut().insert(index, Rc::clone(&ranges));
        *self.locals.borrow_mut() = ranges;
        any
    }

    /// Writable, readable ranges of the whole target as `(base, size)`, in
    /// address order.
    pub(crate) fn writable_regions(&self) -> impl Iterator<Item = (u64, u64)> + '_ {
        self.regions
            .iter()
            .filter(|r| r.perms.read && r.perms.write)
            .map(|r| (r.base, r.size))
    }

    /// Writable ranges of module `index` as `(base, size)`.
    pub(crate) fn module_writable(&self, index: usize) -> Vec<(u64, u64)> {
        let Some(module) = self.modules.get(index) else {
            return Vec::new();
        };
        self.regions
            .iter()
            .filter(|r| r.perms.read && r.perms.write)
            .filter_map(|r| {
                let start = r.base.max(module.base);
                let end = r.base.saturating_add(r.size).min(module.end());
                (start < end).then(|| (start, end - start))
            })
            .collect()
    }

    /// Reads `len` bytes straight from the target, in large requests that
    /// bypass the page cache: a scan touches every page once, and caching them
    /// would only evict the pages the solver reads again. Pages that cannot be
    /// read stay zero, so a span with a guarded page inside it is still
    /// scanned.
    pub(crate) fn read_scan(&self, addr: u64, len: usize) -> Vec<u8> {
        let mut bytes = vec![0u8; len];
        let mut done = 0usize;
        while done < len {
            let take = LOCAL_READ_CHUNK.min(len - done);
            let at = addr + done as u64;
            let Some(slice) = bytes.get_mut(done..done + take) else {
                break;
            };
            if self.raw.read_exact(at, slice).is_err() {
                for (n, page) in slice.chunks_mut(PAGE_SIZE).enumerate() {
                    let _ = self.raw.read_exact(at + (n * PAGE_SIZE) as u64, page);
                }
            }
            done += take;
        }
        bytes
    }

    /// The reader the typed views of `ufathom-engine` should use.
    pub(crate) fn reader(&self) -> &dyn MemoryReader {
        &self.cache
    }

    // ----- memory access -----

    fn local_copy(&self, addr: u64, len: usize) -> Option<Vec<u8>> {
        let locals = self.locals.borrow();
        let after = locals.partition_point(|range| range.base <= addr);
        let range = locals.get(after.checked_sub(1)?)?;
        let offset = usize::try_from(addr.checked_sub(range.base)?).ok()?;
        range
            .bytes
            .get(offset..offset.checked_add(len)?)
            .map(<[u8]>::to_vec)
    }

    /// Reads `len` bytes. `None` when any part is unreadable.
    pub(crate) fn read(&self, addr: u64, len: usize) -> Option<Vec<u8>> {
        if let Some(copy) = self.local_copy(addr, len) {
            return Some(copy);
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

    /// The module that contains `addr`.
    pub(crate) fn module_of(&self, addr: u64) -> Option<&ModuleEntry> {
        let after = self.by_base.partition_point(|&(base, _, _)| base <= addr);
        let &(_, end, index) = self.by_base.get(after.checked_sub(1)?)?;
        if addr < end {
            self.modules.get(index)
        } else {
            None
        }
    }

    /// Whether `addr` is inside any loaded module.
    pub(crate) fn in_image(&self, addr: u64) -> bool {
        self.module_of(addr).is_some()
    }

    /// A layout location for `addr`: an RVA inside the module that contains
    /// it, an absolute address elsewhere.
    pub(crate) fn location(&self, addr: u64) -> ufathom_model::layout::Location {
        match self.module_of(addr) {
            Some(module) => ufathom_model::layout::Location::Rva {
                module: module.name.clone(),
                rva: addr - module.base,
            },
            None => ufathom_model::layout::Location::Absolute(addr),
        }
    }

    /// Every 8-aligned slot of the current module's writable data that holds a
    /// pointer to readable memory, as `(slot address, pointer)`. With
    /// `transform` the user's pointer transform is applied to each stored
    /// value first.
    pub(crate) fn pointer_slots(&self, transform: bool) -> Vec<(u64, u64)> {
        let mut slots = Vec::new();
        let locals = self.locals.borrow();
        'ranges: for range in locals.iter() {
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
        tracing::debug!(
            field = %record.field,
            value = record.value,
            samples = record.samples,
            matched = record.matched,
            method = %record.method,
            "solved"
        );
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
