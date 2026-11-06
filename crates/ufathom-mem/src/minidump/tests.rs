//! Tests for the minidump reader, using dumps assembled byte by byte.

use super::*;
use crate::protect::{MEM_COMMIT, MEM_IMAGE, MEM_PRIVATE, PAGE_EXECUTE_READ, PAGE_READWRITE};
use crate::{MemoryReaderExt, Perms, RegionKind};

const HEADER: usize = 32;
const FREE: u32 = 0x1_0000;

fn put_u32(buf: &mut Vec<u8>, value: u32) {
    buf.extend_from_slice(&value.to_le_bytes());
}

fn put_u64(buf: &mut Vec<u8>, value: u64) {
    buf.extend_from_slice(&value.to_le_bytes());
}

/// Assembles a dump: header and directory first, then blobs appended in order.
struct DumpBuilder {
    buf: Vec<u8>,
    streams: Vec<(u32, u32, u32)>,
    capacity: usize,
}

impl DumpBuilder {
    fn new(stream_capacity: usize) -> Self {
        Self {
            buf: vec![0; HEADER + 12 * stream_capacity],
            streams: Vec::new(),
            capacity: stream_capacity,
        }
    }

    /// Appends a blob and returns its file offset.
    fn blob(&mut self, bytes: &[u8]) -> u32 {
        let rva = self.buf.len() as u32;
        self.buf.extend_from_slice(bytes);
        rva
    }

    fn stream(&mut self, kind: u32, bytes: &[u8]) {
        let rva = self.blob(bytes);
        self.streams.push((kind, bytes.len() as u32, rva));
    }

    /// `MINIDUMP_STRING`.
    fn name(&mut self, text: &str) -> u32 {
        let units: Vec<u16> = text.encode_utf16().collect();
        let mut bytes = Vec::new();
        put_u32(&mut bytes, (units.len() * 2) as u32);
        for unit in units {
            bytes.extend_from_slice(&unit.to_le_bytes());
        }
        self.blob(&bytes)
    }

    fn memory_list(&mut self, ranges: &[(u64, &[u8])]) {
        let mut table = Vec::new();
        put_u32(&mut table, ranges.len() as u32);
        for (start, data) in ranges {
            let rva = self.blob(data);
            put_u64(&mut table, *start);
            put_u32(&mut table, data.len() as u32);
            put_u32(&mut table, rva);
        }
        self.stream(5, &table);
    }

    fn memory64_list(&mut self, ranges: &[(u64, &[u8])]) {
        let base_rva = self.buf.len() as u64;
        for (_, data) in ranges {
            self.blob(data);
        }
        let mut table = Vec::new();
        put_u64(&mut table, ranges.len() as u64);
        put_u64(&mut table, base_rva);
        for (start, data) in ranges {
            put_u64(&mut table, *start);
            put_u64(&mut table, data.len() as u64);
        }
        self.stream(9, &table);
    }

    fn module_list(&mut self, modules: &[(u64, u32, &str)]) {
        let mut table = Vec::new();
        put_u32(&mut table, modules.len() as u32);
        for (base, size, name) in modules {
            let name_rva = self.name(name);
            let mut entry = Vec::new();
            put_u64(&mut entry, *base);
            put_u32(&mut entry, *size);
            put_u32(&mut entry, 0); // checksum
            put_u32(&mut entry, 0); // timestamp
            put_u32(&mut entry, name_rva);
            entry.resize(108, 0); // version info, records, reserved
            table.extend_from_slice(&entry);
        }
        self.stream(4, &table);
    }

    /// `(base, size, state, protect, type)` per entry.
    fn memory_info_list(&mut self, entries: &[(u64, u64, u32, u32, u32)]) {
        let mut table = Vec::new();
        put_u32(&mut table, 16);
        put_u32(&mut table, 48);
        put_u64(&mut table, entries.len() as u64);
        for (base, size, state, protect, kind) in entries {
            put_u64(&mut table, *base);
            put_u64(&mut table, *base); // allocation base
            put_u32(&mut table, *protect); // allocation protect
            put_u32(&mut table, 0);
            put_u64(&mut table, *size);
            put_u32(&mut table, *state);
            put_u32(&mut table, *protect);
            put_u32(&mut table, *kind);
            put_u32(&mut table, 0);
        }
        self.stream(16, &table);
    }

    fn finish(mut self) -> Vec<u8> {
        assert!(self.streams.len() <= self.capacity);
        self.buf[0..4].copy_from_slice(b"MDMP");
        self.buf[4..8].copy_from_slice(&0xa793u32.to_le_bytes());
        self.buf[8..12].copy_from_slice(&(self.streams.len() as u32).to_le_bytes());
        self.buf[12..16].copy_from_slice(&(HEADER as u32).to_le_bytes());
        for (index, (kind, size, rva)) in self.streams.iter().enumerate() {
            let at = HEADER + index * 12;
            self.buf[at..at + 4].copy_from_slice(&kind.to_le_bytes());
            self.buf[at + 4..at + 8].copy_from_slice(&size.to_le_bytes());
            self.buf[at + 8..at + 12].copy_from_slice(&rva.to_le_bytes());
        }
        self.buf
    }
}

fn marker(len: usize, seed: u8) -> Vec<u8> {
    (0..len).map(|i| seed.wrapping_add(i as u8)).collect()
}

/// A dump with both memory list kinds, two modules and a memory info list.
fn full_dump() -> Vec<u8> {
    let a = marker(0x40, 1);
    let b = marker(0x20, 100);
    let c = marker(0x30, 200);
    let mut dump = DumpBuilder::new(4);
    dump.memory_list(&[(0x1000, &a), (0x2000, &b)]);
    dump.memory64_list(&[(0x1040, &c), (0x9000_0000_0000, &a)]);
    dump.module_list(&[
        (0x7ff6_0000_0000, 0x5000, r"C:\Games\Game\game.exe"),
        (
            0x7ffa_0000_0000,
            0x1000,
            r"C:\Windows\System32\kernel32.dll",
        ),
    ]);
    dump.memory_info_list(&[
        (0x1000, 0x1000, MEM_COMMIT, PAGE_READWRITE, MEM_PRIVATE),
        (0x2000, 0x1000, FREE, 0, 0),
        (
            0x7ff6_0000_0000,
            0x5000,
            MEM_COMMIT,
            PAGE_EXECUTE_READ,
            MEM_IMAGE,
        ),
    ]);
    dump.finish()
}

fn error_text(result: Result<MinidumpMemory, MemError>) -> String {
    match result {
        Ok(_) => panic!("expected the dump to be rejected"),
        Err(err) => {
            assert!(matches!(err, MemError::InvalidFormat { .. }), "{err}");
            err.to_string()
        }
    }
}

#[test]
fn reads_from_both_memory_list_kinds_and_spans_between_them() {
    let dump = MinidumpMemory::from_bytes(full_dump()).unwrap();
    let mut buf = [0u8; 4];
    dump.read_exact(0x1000, &mut buf).unwrap();
    assert_eq!(buf, [1, 2, 3, 4]);
    // 0x1000..0x1040 is in the 32-bit list, 0x1040..0x1070 in the 64-bit list.
    let mut span = [0u8; 0x20];
    dump.read_exact(0x1030, &mut span).unwrap();
    assert_eq!(&span[..0x10], marker(0x40, 1).get(0x30..).unwrap());
    assert_eq!(&span[0x10..], marker(0x30, 200).get(..0x10).unwrap());
    assert_eq!(dump.read_u8(0x2000).unwrap(), 100);
    assert_eq!(dump.read_u8(0x9000_0000_0000).unwrap(), 1);
}

#[test]
fn holes_and_edges_are_errors() {
    let dump = MinidumpMemory::from_bytes(full_dump()).unwrap();
    let mut buf = [0u8; 0x20];
    assert!(dump.read_exact(0x1060, &mut buf).is_err()); // runs past 0x1070
    assert!(dump.read_exact(0x0fff, &mut buf[..2]).is_err());
    assert!(dump.read_exact(0x1500, &mut buf[..1]).is_err());
    assert!(matches!(
        dump.read_exact(u64::MAX, &mut buf[..2]),
        Err(MemError::AddressOverflow { .. })
    ));
    assert!(dump.read_exact(0, &mut buf[..1]).is_err());
    assert!(dump.read_exact(0x1000, &mut []).is_ok());
}

#[test]
fn modules_and_regions_are_decoded() {
    let dump = MinidumpMemory::from_bytes(full_dump()).unwrap();
    let modules = dump.modules().unwrap();
    assert_eq!(modules.len(), 2);
    assert_eq!(modules[0].name, "game.exe");
    assert_eq!(modules[0].path.as_deref(), Some(r"C:\Games\Game\game.exe"));
    assert_eq!(modules[0].base, 0x7ff6_0000_0000);
    assert_eq!(modules[0].size, 0x5000);
    assert_eq!(modules[1].name, "kernel32.dll");

    let regions = dump.regions().unwrap();
    // The free entry is not listed.
    assert_eq!(regions.len(), 2);
    assert_eq!(regions[0].base, 0x1000);
    assert_eq!(regions[0].perms, Perms::RW);
    assert_eq!(regions[0].kind, RegionKind::Private);
    assert_eq!(regions[1].perms, Perms::RX);
    assert_eq!(regions[1].kind, RegionKind::Image);
}

#[test]
fn without_a_memory_info_list_regions_are_the_dumped_ranges() {
    let mut dump = DumpBuilder::new(1);
    dump.memory_list(&[
        (0x1000, &[1u8; 0x10]),
        (0x1010, &[2u8; 0x10]),
        (0x3000, &[3u8; 4]),
    ]);
    let dump = MinidumpMemory::from_bytes(dump.finish()).unwrap();
    let regions = dump.regions().unwrap();
    assert_eq!(regions.len(), 2);
    assert_eq!((regions[0].base, regions[0].size), (0x1000, 0x20));
    assert_eq!(regions[0].perms, Perms::R);
    assert_eq!(regions[0].kind, RegionKind::Unknown);
    assert!(dump.modules().unwrap().is_empty());
}

#[test]
fn overlapping_ranges_are_trimmed_and_first_wins() {
    let first = marker(0x20, 10);
    let second = marker(0x20, 50);
    let mut dump = DumpBuilder::new(1);
    dump.memory_list(&[(0x1000, &first), (0x1010, &second)]);
    let dump = MinidumpMemory::from_bytes(dump.finish()).unwrap();
    let mut buf = [0u8; 0x30];
    dump.read_exact(0x1000, &mut buf).unwrap();
    assert_eq!(&buf[..0x20], &first[..]);
    assert_eq!(&buf[0x20..], &second[0x10..]);
}

#[test]
fn a_dump_with_no_memory_streams_is_valid_and_empty() {
    let dump = MinidumpMemory::from_bytes(DumpBuilder::new(0).finish()).unwrap();
    assert!(dump.read_u8(0x1000).is_err());
    assert!(dump.regions().unwrap().is_empty());
}

#[test]
fn bad_signature_and_short_files_are_rejected() {
    let mut bytes = full_dump();
    bytes[0] = b'X';
    assert!(error_text(MinidumpMemory::from_bytes(bytes)).contains("signature"));
    assert!(error_text(MinidumpMemory::from_bytes(Vec::new())).contains("header"));
    assert!(error_text(MinidumpMemory::from_bytes(b"MDMP".to_vec())).contains("header"));
}

#[test]
fn every_truncation_is_rejected() {
    let bytes = full_dump();
    for len in 0..bytes.len() {
        let result = MinidumpMemory::from_bytes(bytes[..len].to_vec());
        assert!(
            matches!(result, Err(MemError::InvalidFormat { .. })),
            "truncation to {len} bytes was accepted"
        );
    }
    assert!(MinidumpMemory::from_bytes(bytes).is_ok());
}

fn patch_u32(bytes: &mut [u8], at: usize, value: u32) {
    bytes[at..at + 4].copy_from_slice(&value.to_le_bytes());
}

fn patch_u64(bytes: &mut [u8], at: usize, value: u64) {
    bytes[at..at + 8].copy_from_slice(&value.to_le_bytes());
}

/// File offset of the stream of `kind` in a dump made by `full_dump`.
fn stream_rva(bytes: &[u8], kind: u32) -> usize {
    let count = u32::from_le_bytes(bytes[8..12].try_into().unwrap()) as usize;
    (0..count)
        .map(|i| HEADER + i * 12)
        .find(|at| u32::from_le_bytes(bytes[*at..*at + 4].try_into().unwrap()) == kind)
        .map(|at| u32::from_le_bytes(bytes[at + 8..at + 12].try_into().unwrap()) as usize)
        .unwrap()
}

#[test]
fn stream_directory_past_eof_is_rejected() {
    let mut bytes = full_dump();
    patch_u32(&mut bytes, 12, u32::MAX);
    error_text(MinidumpMemory::from_bytes(bytes));
    let mut bytes = full_dump();
    let len = bytes.len() as u32;
    patch_u32(&mut bytes, 12, len - 8);
    error_text(MinidumpMemory::from_bytes(bytes));
    // A stream entry whose data lies past the end of the file.
    let mut bytes = full_dump();
    patch_u32(&mut bytes, HEADER + 8, u32::MAX - 4);
    error_text(MinidumpMemory::from_bytes(bytes));
    // A stream that claims more bytes than the file holds.
    let mut bytes = full_dump();
    patch_u32(&mut bytes, HEADER + 4, u32::MAX);
    error_text(MinidumpMemory::from_bytes(bytes));
}

#[test]
fn huge_stream_count_is_rejected_without_looping() {
    let mut bytes = full_dump();
    patch_u32(&mut bytes, 8, u32::MAX);
    error_text(MinidumpMemory::from_bytes(bytes));
}

#[test]
fn huge_counts_inside_streams_are_rejected() {
    // MemoryListStream count.
    let mut bytes = full_dump();
    let at = stream_rva(&bytes, 5);
    patch_u32(&mut bytes, at, u32::MAX);
    error_text(MinidumpMemory::from_bytes(bytes));
    // Memory64ListStream count.
    let mut bytes = full_dump();
    let at = stream_rva(&bytes, 9);
    patch_u64(&mut bytes, at, u64::MAX);
    error_text(MinidumpMemory::from_bytes(bytes));
    let mut bytes = full_dump();
    patch_u64(&mut bytes, at, u64::MAX / 16 + 1);
    error_text(MinidumpMemory::from_bytes(bytes));
    // ModuleListStream count.
    let mut bytes = full_dump();
    let at = stream_rva(&bytes, 4);
    patch_u32(&mut bytes, at, u32::MAX);
    error_text(MinidumpMemory::from_bytes(bytes));
    // MemoryInfoListStream count.
    let mut bytes = full_dump();
    let at = stream_rva(&bytes, 16);
    patch_u64(&mut bytes, at + 8, u64::MAX);
    error_text(MinidumpMemory::from_bytes(bytes));
}

#[test]
fn memory_info_sizes_are_validated() {
    let mut bytes = full_dump();
    let at = stream_rva(&bytes, 16);
    patch_u32(&mut bytes, at + 4, 8);
    error_text(MinidumpMemory::from_bytes(bytes));
    let mut bytes = full_dump();
    patch_u32(&mut bytes, at, 4);
    error_text(MinidumpMemory::from_bytes(bytes));
    // An entry size larger than the stream allows.
    let mut bytes = full_dump();
    patch_u32(&mut bytes, at + 4, 0x4000_0000);
    error_text(MinidumpMemory::from_bytes(bytes));
}

#[test]
fn memory_ranges_past_eof_are_rejected() {
    // 32-bit list: descriptor is { start u64, size u32, rva u32 } after the count.
    let mut bytes = full_dump();
    let at = stream_rva(&bytes, 5) + 4;
    patch_u32(&mut bytes, at + 8, 0xffff_ff00);
    error_text(MinidumpMemory::from_bytes(bytes));
    let mut bytes = full_dump();
    patch_u32(&mut bytes, at + 12, u32::MAX);
    error_text(MinidumpMemory::from_bytes(bytes));
    // 64-bit list: sizes that push the running offset past the file or overflow it.
    let mut bytes = full_dump();
    let at = stream_rva(&bytes, 9) + 16;
    patch_u64(&mut bytes, at + 8, u64::MAX);
    error_text(MinidumpMemory::from_bytes(bytes));
    let mut bytes = full_dump();
    patch_u64(&mut bytes, at + 8, 1 << 40);
    error_text(MinidumpMemory::from_bytes(bytes));
    // Base rva near the end of the file.
    let mut bytes = full_dump();
    let base = stream_rva(&bytes, 9) + 8;
    patch_u64(&mut bytes, base, u64::MAX - 1);
    error_text(MinidumpMemory::from_bytes(bytes));
}

#[test]
fn ranges_that_overflow_the_address_space_are_rejected() {
    let mut bytes = full_dump();
    let at = stream_rva(&bytes, 5) + 4;
    patch_u64(&mut bytes, at, u64::MAX - 2);
    error_text(MinidumpMemory::from_bytes(bytes));
}

#[test]
fn bad_module_names_are_rejected() {
    let mut bytes = full_dump();
    let entry = stream_rva(&bytes, 4) + 4;
    // Name rva past the end of the file.
    patch_u32(&mut bytes, entry + 20, u32::MAX);
    error_text(MinidumpMemory::from_bytes(bytes));
    // Name rva with no room for the length prefix.
    let mut bytes = full_dump();
    let len = bytes.len() as u32;
    patch_u32(&mut bytes, entry + 20, len - 2);
    error_text(MinidumpMemory::from_bytes(bytes));
    // Odd and oversized name lengths.
    let bytes = full_dump();
    let name_rva = u32::from_le_bytes(bytes[entry + 20..entry + 24].try_into().unwrap()) as usize;
    let mut odd = bytes.clone();
    patch_u32(&mut odd, name_rva, 7);
    error_text(MinidumpMemory::from_bytes(odd));
    let mut huge = bytes;
    patch_u32(&mut huge, name_rva, u32::MAX - 1);
    error_text(MinidumpMemory::from_bytes(huge));
}

#[test]
fn single_byte_corruption_never_panics() {
    let original = full_dump();
    for position in 0..original.len() {
        for xor in [0x01u8, 0x80, 0xff] {
            let mut bytes = original.clone();
            bytes[position] ^= xor;
            if let Ok(dump) = MinidumpMemory::from_bytes(bytes) {
                // Whatever parsed must also be safe to use.
                let mut buf = [0u8; 64];
                let _ = dump.read_exact(0x1000, &mut buf);
                let _ = dump.read_exact(0x9000_0000_0000, &mut buf);
                let _ = dump.regions();
                let _ = dump.modules();
            }
        }
    }
}

#[test]
fn opens_a_file_through_a_memory_map() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("synthetic.dmp");
    std::fs::write(&path, full_dump()).unwrap();
    let dump = MinidumpMemory::open(&path).unwrap();
    assert_eq!(dump.read_u8(0x1000).unwrap(), 1);
    assert_eq!(dump.modules().unwrap().len(), 2);
}

#[test]
fn open_reports_empty_garbage_and_missing_files() {
    let dir = tempfile::tempdir().unwrap();
    let empty = dir.path().join("empty.dmp");
    std::fs::write(&empty, []).unwrap();
    assert!(matches!(
        MinidumpMemory::open(&empty),
        Err(MemError::InvalidFormat { .. })
    ));
    let garbage = dir.path().join("garbage.dmp");
    std::fs::write(&garbage, [0x42u8; 100]).unwrap();
    assert!(matches!(
        MinidumpMemory::open(&garbage),
        Err(MemError::InvalidFormat { .. })
    ));
    assert!(matches!(
        MinidumpMemory::open(dir.path().join("missing.dmp")),
        Err(MemError::Io(_))
    ));
}
