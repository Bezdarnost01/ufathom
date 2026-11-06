//! Minidump structure parsing. See the Windows `MINIDUMP_*` structures.

use super::DumpRange;
use super::bytes::Bytes;
use crate::error::MemError;
use crate::protect::{MEM_COMMIT, kind_from_type, perms_from_protect};
use crate::types::{MemoryRegion, ModuleInfo, Perms, RegionKind, file_name};

/// `'MDMP'` read as a little-endian `u32`.
const SIGNATURE: u32 = 0x504d_444d;
/// `sizeof(MINIDUMP_HEADER)`.
const HEADER_SIZE: u64 = 32;
/// `sizeof(MINIDUMP_DIRECTORY)`.
const DIRECTORY_ENTRY_SIZE: u64 = 12;

/// `ModuleListStream`.
const STREAM_MODULE_LIST: u32 = 4;
/// `MemoryListStream`.
const STREAM_MEMORY_LIST: u32 = 5;
/// `Memory64ListStream`.
const STREAM_MEMORY64_LIST: u32 = 9;
/// `MemoryInfoListStream`.
const STREAM_MEMORY_INFO_LIST: u32 = 16;

/// `sizeof(MINIDUMP_MEMORY_DESCRIPTOR)` and `sizeof(MINIDUMP_MEMORY_DESCRIPTOR64)`.
const MEMORY_DESCRIPTOR_SIZE: u64 = 16;
/// `sizeof(MINIDUMP_MODULE)`.
const MODULE_ENTRY_SIZE: u64 = 108;
/// `sizeof(MINIDUMP_MEMORY_INFO_LIST)`, the minimum header size.
const MEMORY_INFO_HEADER_MIN: u32 = 16;
/// `sizeof(MINIDUMP_MEMORY_INFO)`, the minimum entry size.
const MEMORY_INFO_ENTRY_MIN: u32 = 48;

/// Longest module name accepted, in bytes of UTF-16.
const MAX_MODULE_NAME_BYTES: u32 = 1 << 16;

/// Everything the reader needs from a dump.
pub(super) struct Parsed {
    pub(super) ranges: Vec<DumpRange>,
    pub(super) regions: Vec<MemoryRegion>,
    pub(super) modules: Vec<ModuleInfo>,
}

/// One entry of the stream directory.
#[derive(Clone, Copy)]
struct Stream {
    kind: u32,
    size: u32,
    rva: u32,
}

pub(super) fn parse(file: &[u8]) -> Result<Parsed, MemError> {
    let file = Bytes(file);
    if file.len() < HEADER_SIZE {
        return Err(MemError::invalid(
            "minidump",
            format!("file is {:#x} bytes, shorter than the header", file.len()),
        ));
    }
    let signature = file.u32_at(0, "signature")?;
    if signature != SIGNATURE {
        return Err(MemError::invalid(
            "minidump",
            format!("bad signature {signature:#010x}"),
        ));
    }
    let stream_count = u64::from(file.u32_at(8, "stream count")?);
    let directory_rva = u64::from(file.u32_at(12, "stream directory rva")?);
    let directory_len = stream_count
        .checked_mul(DIRECTORY_ENTRY_SIZE)
        .ok_or_else(|| MemError::invalid("minidump", "stream directory size overflows"))?;
    // Checks the whole directory before any entry is read, so a huge count is
    // rejected here and cannot drive a long loop.
    let directory = file.sub(directory_rva, directory_len, "stream directory")?;

    let mut memory_list = None;
    let mut memory64_list = None;
    let mut module_list = None;
    let mut memory_info_list = None;
    for index in 0..stream_count {
        let at = index * DIRECTORY_ENTRY_SIZE;
        let entry = Stream {
            kind: directory.u32_at(at, "stream directory entry")?,
            size: directory.u32_at(at + 4, "stream directory entry")?,
            rva: directory.u32_at(at + 8, "stream directory entry")?,
        };
        let slot = match entry.kind {
            STREAM_MEMORY_LIST => &mut memory_list,
            STREAM_MEMORY64_LIST => &mut memory64_list,
            STREAM_MODULE_LIST => &mut module_list,
            STREAM_MEMORY_INFO_LIST => &mut memory_info_list,
            _ => continue,
        };
        // The first stream of a kind wins.
        slot.get_or_insert(entry);
    }

    let mut raw_ranges = Vec::new();
    if let Some(stream) = memory_list {
        parse_memory_list(file, stream, &mut raw_ranges)?;
    }
    if let Some(stream) = memory64_list {
        parse_memory64_list(file, stream, &mut raw_ranges)?;
    }
    let ranges = normalize(raw_ranges);

    let regions = match memory_info_list {
        Some(stream) => parse_memory_info(file, stream)?,
        None => regions_from_ranges(&ranges),
    };
    let modules = match module_list {
        Some(stream) => parse_modules(file, stream)?,
        None => Vec::new(),
    };
    Ok(Parsed {
        ranges,
        regions,
        modules,
    })
}

/// Bytes of a stream, bounds-checked against the file.
fn stream_bytes<'a>(file: Bytes<'a>, stream: Stream, what: &str) -> Result<Bytes<'a>, MemError> {
    file.sub(u64::from(stream.rva), u64::from(stream.size), what)
}

/// `MINIDUMP_MEMORY_LIST`: `u32` count, then `{ u64 start, u32 size, u32 rva }`.
fn parse_memory_list(
    file: Bytes<'_>,
    stream: Stream,
    out: &mut Vec<DumpRange>,
) -> Result<(), MemError> {
    let data = stream_bytes(file, stream, "memory list stream")?;
    let count = u64::from(data.u32_at(0, "memory list count")?);
    let table_len = count.checked_mul(MEMORY_DESCRIPTOR_SIZE);
    let table = table_len
        .and_then(|len| data.sub(4, len, "memory list descriptors").ok())
        .ok_or_else(|| {
            MemError::invalid(
                "minidump",
                format!("memory list of {count} descriptors does not fit its stream"),
            )
        })?;
    out.reserve(usize::try_from(count).unwrap_or(0));
    for index in 0..count {
        let at = index * MEMORY_DESCRIPTOR_SIZE;
        let start = table.u64_at(at, "memory descriptor")?;
        let size = u64::from(table.u32_at(at + 8, "memory descriptor")?);
        let rva = u64::from(table.u32_at(at + 12, "memory descriptor")?);
        push_range(file, out, start, size, rva)?;
    }
    Ok(())
}

/// `MINIDUMP_MEMORY64_LIST`: `u64` count, `u64` base rva, then `{ u64 start, u64 size }`.
/// The bytes of all ranges follow one another from the base rva.
fn parse_memory64_list(
    file: Bytes<'_>,
    stream: Stream,
    out: &mut Vec<DumpRange>,
) -> Result<(), MemError> {
    let data = stream_bytes(file, stream, "memory64 list stream")?;
    let count = data.u64_at(0, "memory64 list count")?;
    let mut rva = data.u64_at(8, "memory64 list base rva")?;
    let table = count
        .checked_mul(MEMORY_DESCRIPTOR_SIZE)
        .and_then(|len| data.sub(16, len, "memory64 list descriptors").ok())
        .ok_or_else(|| {
            MemError::invalid(
                "minidump",
                format!("memory64 list of {count} descriptors does not fit its stream"),
            )
        })?;
    out.reserve(usize::try_from(count).unwrap_or(0));
    for index in 0..count {
        let at = index * MEMORY_DESCRIPTOR_SIZE;
        let start = table.u64_at(at, "memory64 descriptor")?;
        let size = table.u64_at(at + 8, "memory64 descriptor")?;
        push_range(file, out, start, size, rva)?;
        rva = rva
            .checked_add(size)
            .ok_or_else(|| MemError::invalid("minidump", "memory64 data offsets overflow"))?;
    }
    Ok(())
}

/// Validates one range against the file and the address space, then records it.
fn push_range(
    file: Bytes<'_>,
    out: &mut Vec<DumpRange>,
    start: u64,
    size: u64,
    rva: u64,
) -> Result<(), MemError> {
    if size == 0 {
        return Ok(());
    }
    if start.checked_add(size).is_none() {
        return Err(MemError::invalid(
            "minidump",
            format!("memory range at {start:#x} with size {size:#x} overflows the address space"),
        ));
    }
    // The bytes must lie inside the file.
    file.slice(rva, size, "memory range data")?;
    out.push(DumpRange {
        start,
        len: size,
        file_offset: rva,
    });
    Ok(())
}

/// Sorts ranges and trims overlaps so that lookups can binary search. Where two
/// ranges cover the same address, the one with the lower start wins.
fn normalize(mut raw: Vec<DumpRange>) -> Vec<DumpRange> {
    raw.sort_by_key(|range| (range.start, range.file_offset));
    let mut ranges: Vec<DumpRange> = Vec::with_capacity(raw.len());
    let mut covered_end = 0u64;
    for mut range in raw {
        // `push_range` guarantees `start + len` does not overflow.
        let end = range.start + range.len;
        if end <= covered_end {
            continue;
        }
        if range.start < covered_end {
            let trim = covered_end - range.start;
            range.start = covered_end;
            range.file_offset = range.file_offset.saturating_add(trim);
            range.len -= trim;
        }
        covered_end = end;
        ranges.push(range);
    }
    ranges
}

/// Regions for a dump without a memory info list: the dumped ranges, merged
/// where adjacent, assumed readable.
fn regions_from_ranges(ranges: &[DumpRange]) -> Vec<MemoryRegion> {
    let mut regions: Vec<MemoryRegion> = Vec::new();
    for range in ranges {
        match regions.last_mut() {
            Some(last) if last.base + last.size == range.start => last.size += range.len,
            _ => regions.push(MemoryRegion {
                base: range.start,
                size: range.len,
                perms: Perms::R,
                kind: RegionKind::Unknown,
            }),
        }
    }
    regions
}

/// `MINIDUMP_MEMORY_INFO_LIST`: header sizes and count, then fixed-size entries.
fn parse_memory_info(file: Bytes<'_>, stream: Stream) -> Result<Vec<MemoryRegion>, MemError> {
    let data = stream_bytes(file, stream, "memory info list stream")?;
    let header_size = data.u32_at(0, "memory info header size")?;
    let entry_size = data.u32_at(4, "memory info entry size")?;
    let count = data.u64_at(8, "memory info count")?;
    if header_size < MEMORY_INFO_HEADER_MIN || entry_size < MEMORY_INFO_ENTRY_MIN {
        return Err(MemError::invalid(
            "minidump",
            format!("memory info sizes too small: header {header_size}, entry {entry_size}"),
        ));
    }
    let table = count
        .checked_mul(u64::from(entry_size))
        .and_then(|len| {
            data.sub(u64::from(header_size), len, "memory info entries")
                .ok()
        })
        .ok_or_else(|| {
            MemError::invalid(
                "minidump",
                format!("memory info list of {count} entries does not fit its stream"),
            )
        })?;
    let mut regions = Vec::with_capacity(usize::try_from(count).unwrap_or(0));
    for index in 0..count {
        let at = index * u64::from(entry_size);
        let base = table.u64_at(at, "memory info entry")?;
        let size = table.u64_at(at + 24, "memory info entry")?;
        let state = table.u32_at(at + 32, "memory info entry")?;
        let protect = table.u32_at(at + 36, "memory info entry")?;
        let kind = table.u32_at(at + 40, "memory info entry")?;
        if state != MEM_COMMIT || size == 0 {
            continue;
        }
        regions.push(MemoryRegion {
            base,
            size,
            perms: perms_from_protect(protect),
            kind: kind_from_type(kind),
        });
    }
    regions.sort_by_key(|region| region.base);
    Ok(regions)
}

/// `MINIDUMP_MODULE_LIST`: `u32` count, then 108-byte entries with a name rva.
fn parse_modules(file: Bytes<'_>, stream: Stream) -> Result<Vec<ModuleInfo>, MemError> {
    let data = stream_bytes(file, stream, "module list stream")?;
    let count = u64::from(data.u32_at(0, "module list count")?);
    let table = count
        .checked_mul(MODULE_ENTRY_SIZE)
        .and_then(|len| data.sub(4, len, "module list entries").ok())
        .ok_or_else(|| {
            MemError::invalid(
                "minidump",
                format!("module list of {count} entries does not fit its stream"),
            )
        })?;
    let mut modules = Vec::with_capacity(usize::try_from(count).unwrap_or(0));
    for index in 0..count {
        let at = index * MODULE_ENTRY_SIZE;
        let base = table.u64_at(at, "module entry")?;
        let size = u64::from(table.u32_at(at + 8, "module entry")?);
        let name_rva = u64::from(table.u32_at(at + 20, "module entry")?);
        let path = read_name(file, name_rva)?;
        modules.push(ModuleInfo {
            name: file_name(&path).to_owned(),
            path: Some(path),
            base,
            size,
        });
    }
    modules.sort_by_key(|module| module.base);
    Ok(modules)
}

/// `MINIDUMP_STRING`: `u32` byte length, then UTF-16LE without terminator.
fn read_name(file: Bytes<'_>, rva: u64) -> Result<String, MemError> {
    let byte_len = file.u32_at(rva, "module name length")?;
    if byte_len > MAX_MODULE_NAME_BYTES || byte_len % 2 != 0 {
        return Err(MemError::invalid(
            "minidump",
            format!("module name at {rva:#x} has invalid length {byte_len}"),
        ));
    }
    let raw = file.slice(rva + 4, u64::from(byte_len), "module name")?;
    let units: Vec<u16> = raw
        .chunks_exact(2)
        .filter_map(|pair| <[u8; 2]>::try_from(pair).ok())
        .map(u16::from_le_bytes)
        .collect();
    Ok(String::from_utf16_lossy(&units))
}
