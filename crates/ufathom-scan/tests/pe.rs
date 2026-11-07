//! PE parsing and file loading: layout, permissions, truncation policy and
//! every way a header can lie.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "test code may panic on failure; clippy.toml only exempts #[test] functions, not helpers"
)]

mod common;

use std::io::Cursor;

use common::*;
use ufathom_mem::{MemoryReader, MemoryReaderExt, Perms, RegionKind};
use ufathom_scan::limits::{MAX_LOAD_BYTES, MAX_SECTIONS};
use ufathom_scan::pe::{DIRECTORY_RESOURCE, SCN_MEM_READ};
use ufathom_scan::{PeImage, ScanError, load_pe_reader};

/// A change to the bytes of a file.
type Mutation = Box<dyn Fn(&mut Vec<u8>)>;

fn small_pe() -> PeSpec {
    standard_pe(
        (0u8..=255).cycle().take(0x300).collect(),
        vec![7u8; 0x80],
        None,
    )
}

fn image_of(bytes: &[u8]) -> Result<PeImage, ScanError> {
    let (snapshot, module) = load(bytes)?;
    PeImage::parse_from_memory(&snapshot, module.base)
}

#[test]
fn parses_sections_flags_and_directories_of_a_64_bit_image() {
    let mut spec = standard_pe(vec![1; 0x300], vec![2; 0x80], Some(vec![3; 0x40]));
    spec.entry = TEXT_RVA + 8;
    let (snapshot, module) = load(&spec.to_bytes()).unwrap();
    let image = PeImage::parse_from_memory(&snapshot, module.base).unwrap();

    assert!(image.is_64_bit);
    assert_eq!(image.machine, 0x8664);
    assert_eq!(image.image_base, IMAGE_BASE);
    assert_eq!(image.base, IMAGE_BASE);
    assert_eq!(image.entry_point_rva, TEXT_RVA + 8);
    assert_eq!(image.size_of_image, 0x5000);
    assert_eq!(image.sections.len(), 4);

    let names: Vec<_> = image.sections.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, [".text", ".rdata", ".data", ".rsrc"]);
    let rdata = image.section_named(".rdata").unwrap();
    assert_eq!(rdata.rva, RDATA_RVA);
    assert_eq!(rdata.virtual_size, 0x300);
    assert_eq!(rdata.characteristics, RDATA);
    assert!(rdata.is_read_only_data());
    assert!(!image.section_named(".data").unwrap().is_read_only_data());
    assert!(!image.section_named(".text").unwrap().is_read_only_data());
    assert_eq!(
        image.section_at_rva(RDATA_RVA + 0x2FF).map(|s| &*s.name),
        Some(".rdata")
    );
    assert!(image.section_at_rva(RDATA_RVA + 0x300).is_none());

    let resource = image.directory(DIRECTORY_RESOURCE).unwrap();
    assert_eq!((resource.rva, resource.size), (RSRC_RVA, 0x40));
    assert!(image.directory(0).is_none(), "an empty directory is absent");
    assert!(image.directory(15).is_none());
    assert_eq!(image.directories.len(), 16);
    assert_eq!(image.address_of(0x10, 4), Some(IMAGE_BASE + 0x10));
    assert_eq!(image.address_of(0x4FFF, 2), None);
    assert_eq!(image.address_of(u64::MAX, 2), None);
}

#[test]
fn parses_a_32_bit_image() {
    let mut spec = small_pe();
    spec.pe32 = true;
    spec.image_base = 0x40_0000;
    let (snapshot, module) = load(&spec.to_bytes()).unwrap();
    assert_eq!(module.base, 0x40_0000);
    let image = PeImage::parse_from_memory(&snapshot, module.base).unwrap();
    assert!(!image.is_64_bit);
    assert_eq!(image.machine, 0x14C);
    assert_eq!(image.image_base, 0x40_0000);
    assert_eq!(image.sections.len(), 3);
    assert_eq!(image.directories.len(), 16);
}

#[test]
fn loading_maps_sections_at_their_rvas_with_flag_permissions() {
    let spec = small_pe();
    let (snapshot, module) = load(&spec.to_bytes()).unwrap();
    assert_eq!(module.name, "test.exe");
    assert_eq!(module.base, IMAGE_BASE);
    assert_eq!(module.size, 0x4000);
    assert_eq!(module.path, None);
    assert_eq!(snapshot.modules().unwrap(), vec![module.clone()]);

    let regions = snapshot.regions().unwrap();
    let at = |rva: u64| {
        regions
            .iter()
            .find(|r| r.base == IMAGE_BASE + rva)
            .unwrap_or_else(|| panic!("no region at RVA {rva:#x}"))
    };
    assert_eq!(at(0).perms, Perms::R, "headers");
    assert_eq!(at(0).size, 0x1000);
    assert_eq!(at(0x1000).perms, Perms::RX);
    assert_eq!(at(0x2000).perms, Perms::R);
    assert_eq!(at(0x3000).perms, Perms::RW);
    assert!(regions.iter().all(|r| r.kind == RegionKind::Image));

    // The bytes are the file's, and the memory past the section data is zero.
    let rdata = snapshot
        .read_vec::<u8>(IMAGE_BASE + 0x2000, 0x1000)
        .unwrap();
    assert_eq!(&rdata[..0x300], &spec.sections[1].data[..]);
    assert!(rdata[0x300..].iter().all(|b| *b == 0));
    assert_eq!(snapshot.read_u16(IMAGE_BASE).unwrap(), 0x5A4D);
    // A hole between the image and anything else stays unreadable.
    assert!(snapshot.read_u8(IMAGE_BASE + 0x4000).is_err());
}

#[test]
fn a_section_with_no_flags_is_mapped_but_unreadable() {
    let spec = PeSpec::new()
        .section(".text", 0x1000, vec![1; 16], CODE)
        .section(".none", 0x2000, vec![2; 16], 0);
    let (snapshot, module) = load(&spec.to_bytes()).unwrap();
    assert!(snapshot.read_u8(module.base + 0x1000).is_ok());
    assert!(snapshot.read_u8(module.base + 0x2000).is_err());
    let image = PeImage::parse_from_memory(&snapshot, module.base).unwrap();
    assert_eq!(image.section_named(".none").unwrap().perms(), Perms::NONE);
    assert_eq!(
        image.section_named(".none").unwrap().characteristics & SCN_MEM_READ,
        0
    );
}

#[test]
fn zero_virtual_size_uses_the_raw_size() {
    let mut spec = PeSpec::new().section(".text", 0x1000, vec![5; 0x200], CODE);
    spec.sections[0].virtual_size = Some(0);
    let (snapshot, module) = load(&spec.to_bytes()).unwrap();
    let image = PeImage::parse_from_memory(&snapshot, module.base).unwrap();
    assert_eq!(image.sections[0].mapped_size(), 0x200);
    assert_eq!(snapshot.read_u8(module.base + 0x11FF).unwrap(), 5);
}

#[test]
fn bss_like_sections_are_zero_filled_and_empty_ones_are_skipped() {
    let mut spec = PeSpec::new()
        .section(".text", 0x1000, vec![5; 8], CODE)
        .section(".bss", 0x2000, Vec::new(), DATA);
    spec.sections[1].virtual_size = Some(0x3000);
    let (snapshot, module) = load(&spec.to_bytes()).unwrap();
    assert_eq!(snapshot.read_u64(module.base + 0x2000).unwrap(), 0);
    assert_eq!(snapshot.read_u8(module.base + 0x4FFF).unwrap(), 0);
    assert!(snapshot.read_u8(module.base + 0x5000).is_err());

    // No data and no size: nothing to map, and not an error.
    let empty = PeSpec::new()
        .section(".text", 0x1000, vec![5; 8], CODE)
        .section(".empty", 0x2000, Vec::new(), DATA);
    let (snapshot, module) = load(&empty.to_bytes()).unwrap();
    assert!(snapshot.read_u8(module.base + 0x2000).is_err());
}

#[test]
fn a_section_past_the_end_of_the_file_is_an_error_but_missing_padding_is_not() {
    let bytes = small_pe().to_bytes();
    // `.data` has 0x80 bytes of data and 0x180 bytes of file-alignment
    // padding at the end of the file. Cut inside the data.
    let cut = bytes.len() - 0x1C0;
    match load(&bytes[..cut]) {
        Err(ScanError::TruncatedFile {
            section,
            needed_end,
            file_len,
        }) => {
            assert_eq!(section, ".data");
            assert_eq!(file_len, cut as u64);
            assert!(needed_end > file_len);
        }
        other => panic!("expected TruncatedFile, got {other:?}"),
    }
    // Cutting only the padding leaves every mapped byte present.
    let (snapshot, module) = load(&bytes[..bytes.len() - 0x180]).unwrap();
    assert_eq!(snapshot.read_u8(module.base + 0x3000 + 0x7F).unwrap(), 7);
    assert_eq!(snapshot.read_u8(module.base + 0x3000 + 0x80).unwrap(), 0);
}

#[test]
fn headers_cut_short_are_errors_at_every_length() {
    let bytes = small_pe().to_bytes();
    for len in 0..=HEADERS_SIZE {
        // Must not panic; anything shorter than the headers plus data is an error.
        let result = load(&bytes[..len]);
        assert!(result.is_err(), "length {len} unexpectedly loaded");
    }
    assert!(load(&bytes).is_ok());
}

#[test]
fn not_a_pe_is_named() {
    assert!(matches!(load(&[]), Err(ScanError::NotPe { .. })));
    assert!(matches!(
        load(&vec![0u8; 0x1000]),
        Err(ScanError::NotPe { .. })
    ));
    let mut bytes = small_pe().to_bytes();
    bytes[NT_OFFSET] = b'X';
    assert!(matches!(load(&bytes), Err(ScanError::NotPe { .. })));
}

#[test]
fn section_count_that_lies_is_an_error() {
    let base = small_pe().to_bytes();
    let count_at = NT_OFFSET + 6;
    for (count, expect_limit) in [(MAX_SECTIONS + 1, true), (0xFFFF, true), (0, false)] {
        let mut bytes = base.clone();
        put16(&mut bytes, count_at, count);
        match load(&bytes) {
            Err(ScanError::LimitExceeded { what, .. }) if expect_limit => {
                assert_eq!(what, "section count");
            }
            Err(ScanError::Malformed { .. }) if !expect_limit => {}
            other => panic!("count {count}: {other:?}"),
        }
    }
    // More sections than the table holds: the extra entries read as zeroed
    // sections, which the file cannot have, or as data that is not a table.
    let mut bytes = base.clone();
    put16(&mut bytes, count_at, 30);
    let _ = load(&bytes);
    // Fewer sections than the file has data for is a valid, smaller image.
    let mut bytes = base;
    put16(&mut bytes, count_at, 1);
    let (snapshot, module) = load(&bytes).unwrap();
    let image = PeImage::parse_from_memory(&snapshot, module.base).unwrap();
    assert_eq!(image.sections.len(), 1);
}

#[test]
fn section_ranges_that_overflow_or_leave_the_image_are_errors() {
    let base = small_pe().to_bytes();
    let table = NT_OFFSET + 4 + 20 + 0xF0;
    // `.rdata` is the second section header.
    let rdata = table + 40;
    for (what, field, value) in [
        ("rva near u32::MAX", 12, 0xFFFF_F000u32),
        ("virtual size near u32::MAX", 8, 0xFFFF_FFF0),
        ("rva past the image", 12, 0x10_0000),
    ] {
        let mut bytes = base.clone();
        put32(&mut bytes, rdata + field, value);
        assert!(
            matches!(load(&bytes), Err(ScanError::Malformed { .. })),
            "{what}"
        );
    }
    // Both fields at their maximum: the sum overflows 32 bits.
    let mut bytes = base.clone();
    put32(&mut bytes, rdata + 8, 0xFFFF_FFFF);
    put32(&mut bytes, rdata + 12, 0xFFFF_FFFF);
    assert!(matches!(load(&bytes), Err(ScanError::Malformed { .. })));
    // Two sections on the same RVA.
    let mut bytes = base.clone();
    put32(&mut bytes, rdata + 12, TEXT_RVA);
    assert!(matches!(load(&bytes), Err(ScanError::Malformed { .. })));
    // SizeOfImage of zero.
    let mut bytes = base;
    put32(&mut bytes, NT_OFFSET + 4 + 20 + 56, 0);
    assert!(matches!(load(&bytes), Err(ScanError::Malformed { .. })));
}

#[test]
fn headers_that_lie_are_errors() {
    let base = small_pe().to_bytes();
    let optional = NT_OFFSET + 4 + 20;
    let cases: Vec<(&str, Mutation)> = vec![
        (
            "e_lfanew below the DOS header",
            Box::new(|b| put32(b, 0x3C, 0x10)),
        ),
        ("e_lfanew huge", Box::new(|b| put32(b, 0x3C, 0xFFFF_FFF0))),
        (
            "e_lfanew past the file",
            Box::new(|b| put32(b, 0x3C, 0xFFFF)),
        ),
        (
            "optional header too small",
            Box::new(move |b| put16(b, NT_OFFSET + 4 + 16, 0x10)),
        ),
        (
            "optional header absurd",
            Box::new(move |b| put16(b, NT_OFFSET + 4 + 16, 0xFFFF)),
        ),
        (
            "unknown optional magic",
            Box::new(move |b| put16(b, optional, 0x107)),
        ),
    ];
    for (what, mutate) in cases {
        let mut bytes = base.clone();
        mutate(&mut bytes);
        assert!(load(&bytes).is_err(), "{what}");
    }
    let mut bytes = base;
    put16(&mut bytes, optional, 0x107);
    assert!(matches!(load(&bytes), Err(ScanError::Unsupported { .. })));
}

#[test]
fn directory_count_beyond_the_optional_header_is_clamped() {
    let mut bytes = small_pe().to_bytes();
    let optional = NT_OFFSET + 4 + 20;
    put32(&mut bytes, optional + 108, 0xFFFF_FFFF);
    assert_eq!(image_of(&bytes).unwrap().directories.len(), 16);
    put32(&mut bytes, optional + 108, 3);
    assert_eq!(image_of(&bytes).unwrap().directories.len(), 3);
    put32(&mut bytes, optional + 108, 0);
    assert_eq!(image_of(&bytes).unwrap().directories.len(), 0);
}

#[test]
fn image_base_that_overflows_is_an_error() {
    let mut spec = small_pe();
    spec.image_base = u64::MAX - 0xFFF;
    assert!(matches!(
        load(&spec.to_bytes()),
        Err(ScanError::Malformed { .. })
    ));
}

#[test]
fn headers_overlapping_a_section_are_an_error() {
    let mut spec = PeSpec::new().section(".text", 0x100, vec![1; 16], CODE);
    spec.sections[0].rva = 0x100;
    assert!(matches!(
        load(&spec.to_bytes()),
        Err(ScanError::Malformed { .. })
    ));
}

#[test]
fn an_image_larger_than_the_load_limit_is_refused_before_allocating() {
    let mut spec = PeSpec::new().section(".text", 0x1000, vec![1; 16], CODE);
    spec.sections[0].virtual_size = Some(0x7000_0000);
    match load(&spec.to_bytes()) {
        Err(ScanError::LimitExceeded { limit, .. }) => assert_eq!(limit, MAX_LOAD_BYTES),
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_directory_of_zero_rva_is_absent() {
    let spec = standard_pe(vec![0; 16], vec![0; 16], None).resource_dir(0, 0x100);
    assert!(image_of(&spec.to_bytes()).unwrap().directory(2).is_none());
}

#[test]
fn load_pe_reader_does_not_read_past_what_it_needs() {
    // A reader that counts bytes read: each section is read once.
    struct Counting<R>(R, std::cell::Cell<u64>);
    impl<R: std::io::Read> std::io::Read for Counting<R> {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            let n = self.0.read(buf)?;
            self.1.set(self.1.get() + n as u64);
            Ok(n)
        }
    }
    impl<R: std::io::Seek> std::io::Seek for Counting<R> {
        fn seek(&mut self, pos: std::io::SeekFrom) -> std::io::Result<u64> {
            self.0.seek(pos)
        }
    }
    let bytes = PeSpec::new()
        .section(".text", 0x1000, vec![1; 0x100], CODE)
        .section(".rdata", 0x2000, vec![9; 0x10_0000], RDATA)
        .section(".data", 0x10_2000, vec![9; 0x1000], DATA)
        .to_bytes();
    let mut counting = Counting(Cursor::new(bytes.clone()), std::cell::Cell::new(0));
    let _ = load_pe_reader(&mut counting, "big.exe").unwrap();
    // The head (the whole small header area) plus each section once.
    let read = counting.1.get();
    assert!(
        read <= bytes.len() as u64 + 0x4_0000,
        "read {read} of {} bytes",
        bytes.len()
    );
}
