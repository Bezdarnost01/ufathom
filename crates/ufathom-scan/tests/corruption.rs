//! Damaged and hostile input: resource trees that cycle or nest, lengths that
//! lie, and byte-flip sweeps over the headers and the resource section.
//! Nothing here may panic, loop without bound or allocate on a lie.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "test code may panic on failure; clippy.toml only exempts #[test] functions, not helpers"
)]

mod common;

use common::*;
use ufathom_mem::{MemoryReaderExt, Snapshot};
use ufathom_scan::limits::{
    MAX_VERSION_BLOCKS, MAX_VERSION_RESOURCE_BYTES, MAX_VERSION_VALUE_UNITS,
};
use ufathom_scan::{PeImage, ScanError, VersionFacts, detect_version};

/// Image with a hand-built `.rsrc` section holding `rsrc` verbatim.
fn pe_with_rsrc(rsrc: Vec<u8>) -> Vec<u8> {
    standard_pe(vec![0; 0x100], vec![0; 0x100], Some(rsrc)).to_bytes()
}

/// A version data entry at offset 0x60 pointing at `blob_rva`/`size`, under
/// a regular three-level tree.
fn tree_with_data_entry(blob_rva: u32, size: u32) -> Vec<u8> {
    let mut rsrc = directory(&[(16, SUBDIR | 0x18)]);
    rsrc.extend(directory(&[(1, SUBDIR | 0x30)]));
    rsrc.extend(directory(&[(0x409, 0x48)]));
    assert_eq!(rsrc.len(), 0x48);
    rsrc.extend_from_slice(&blob_rva.to_le_bytes());
    rsrc.extend_from_slice(&size.to_le_bytes());
    rsrc.extend_from_slice(&[0; 8]);
    rsrc
}

fn note_of(facts: &VersionFacts) -> String {
    facts.notes.join("\n")
}

#[test]
fn cyclic_resource_directories_are_refused() {
    let self_loop = {
        let mut rsrc = directory(&[(16, SUBDIR | 0x18)]);
        rsrc.extend(directory(&[(1, SUBDIR | 0x18)]));
        rsrc
    };
    let back_to_root = {
        let mut rsrc = directory(&[(16, SUBDIR | 0x18)]);
        rsrc.extend(directory(&[(1, SUBDIR)]));
        rsrc
    };
    let three_cycle = {
        let mut rsrc = directory(&[(16, SUBDIR | 0x18)]);
        rsrc.extend(directory(&[(1, SUBDIR | 0x30)]));
        rsrc.extend(directory(&[(1, SUBDIR | 0x18)]));
        rsrc
    };
    let root_self = directory(&[(16, SUBDIR)]);
    for (what, rsrc) in [
        ("self loop", self_loop),
        ("back to the root", back_to_root),
        ("three directories", three_cycle),
        ("root contains itself", root_self),
    ] {
        let facts = facts_of(&pe_with_rsrc(rsrc));
        assert!(facts.resource.is_none(), "{what}");
        assert!(
            note_of(&facts).contains("contains itself"),
            "{what}: {:?}",
            facts.notes
        );
        assert_eq!(facts.engine.claimed, None, "{what}");
    }
}

#[test]
fn deeply_nested_resource_directories_are_refused() {
    // root -> chain of directories, each with one entry to the next.
    let mut rsrc = directory(&[(16, SUBDIR | 0x18)]);
    for level in 1..12u32 {
        rsrc.extend(directory(&[(1, SUBDIR | (0x18 * (level + 1)))]));
    }
    let facts = facts_of(&pe_with_rsrc(rsrc));
    assert!(facts.resource.is_none());
    assert!(
        note_of(&facts).contains("nested deeper"),
        "{:?}",
        facts.notes
    );
}

#[test]
fn a_version_entry_that_is_not_a_directory_is_refused() {
    let facts = facts_of(&pe_with_rsrc(directory(&[(16, 0x18)])));
    assert!(
        note_of(&facts).contains("not a directory"),
        "{:?}",
        facts.notes
    );
}

#[test]
fn directory_entry_counts_that_lie_are_bounded_or_unreadable() {
    // 0xFFFF named + 0xFFFF id entries: above the entry limit.
    let mut huge = vec![0u8; 16];
    huge[12..14].copy_from_slice(&0xFFFFu16.to_le_bytes());
    huge[14..16].copy_from_slice(&0xFFFFu16.to_le_bytes());
    let facts = facts_of(&pe_with_rsrc(huge));
    assert!(
        note_of(&facts).contains("exceeds its limit"),
        "{:?}",
        facts.notes
    );

    // Within the limit, but the entries would run past the image.
    let mut past_image = vec![0u8; 16];
    past_image[14..16].copy_from_slice(&0x3FFFu16.to_le_bytes());
    let facts = facts_of(&pe_with_rsrc(past_image));
    assert!(
        note_of(&facts).contains("outside the image"),
        "{:?}",
        facts.notes
    );
}

#[test]
fn a_resource_directory_outside_the_image_is_refused() {
    for rva in [0xFFFF_0000u32, 0xFFFF_FFFF, 0x10_0000] {
        let spec = standard_pe(vec![0; 16], vec![0; 16], None).resource_dir(rva, 0x100);
        let facts = facts_of(&spec.to_bytes());
        assert!(
            note_of(&facts).contains("outside the image"),
            "{rva:#x}: {:?}",
            facts.notes
        );
    }
}

#[test]
fn data_entries_that_lie_are_refused() {
    for (what, rsrc, expect) in [
        (
            "size zero",
            tree_with_data_entry(RSRC_RVA + 0x60, 0),
            "size is zero",
        ),
        (
            "size above the limit",
            tree_with_data_entry(RSRC_RVA + 0x60, MAX_VERSION_RESOURCE_BYTES + 1),
            "exceeds its limit",
        ),
        (
            "size 2 GiB",
            tree_with_data_entry(RSRC_RVA + 0x60, 0x7FFF_FFFF),
            "exceeds its limit",
        ),
        (
            "rva outside the image",
            tree_with_data_entry(0xFFFF_FFF0, 0x40),
            "outside the image",
        ),
        (
            "runs past the image",
            tree_with_data_entry(0x4FF0, 0x100),
            "outside the image",
        ),
    ] {
        let facts = facts_of(&pe_with_rsrc(rsrc));
        assert!(facts.resource.is_none(), "{what}");
        assert!(
            note_of(&facts).contains(expect),
            "{what}: {:?}",
            facts.notes
        );
    }
}

// ---- version block damage ---------------------------------------------------

fn claimed_and_problems(blob: Vec<u8>) -> (VersionFacts, Option<String>) {
    let rsrc = rsrc_with_versions(RSRC_RVA, &[(0x409, blob)]);
    let facts = facts_of(&pe_with_rsrc(rsrc));
    let claimed = facts.engine.claimed.clone();
    (facts, claimed)
}

#[test]
fn a_root_block_that_lies_is_refused() {
    let good = version_blob(None, &[("ProductVersion", "4.21.1")]);
    let mut cases: Vec<(&str, Vec<u8>)> = Vec::new();
    for (what, length) in [
        ("past the data", 0xFFFFu16),
        ("a few bytes too long", good.len() as u16 + 4),
        ("below the header", 3),
        ("zero", 0),
    ] {
        let mut blob = good.clone();
        blob[0..2].copy_from_slice(&length.to_le_bytes());
        cases.push((what, blob));
    }
    // A key that is never terminated.
    let mut unterminated = vec![0u8; 6];
    unterminated.extend(std::iter::repeat_n(0x41, 100));
    unterminated[0..2].copy_from_slice(&106u16.to_le_bytes());
    cases.push(("unterminated key", unterminated));
    // A key that is far too long.
    let mut long_key = vec![0u8; 6];
    long_key.extend(utf16z(&"K".repeat(400)));
    let len = long_key.len() as u16;
    long_key[0..2].copy_from_slice(&len.to_le_bytes());
    cases.push(("long key", long_key));
    // A root value larger than its block.
    let mut value = good.clone();
    value[2..4].copy_from_slice(&0x7000u16.to_le_bytes());
    value[4..6].copy_from_slice(&0u16.to_le_bytes());
    cases.push(("value past the block", value));
    // The wrong root key.
    cases.push((
        "wrong root key",
        block("NOT_VERSION_INFO", &[], 0, false, &[]),
    ));
    for (what, blob) in cases {
        let (facts, claimed) = claimed_and_problems(blob);
        assert_eq!(claimed, None, "{what}");
        assert!(facts.resource.is_none(), "{what}");
        assert!(!note_of(&facts).is_empty(), "{what}");
    }
}

#[test]
fn a_child_that_lies_keeps_what_was_read_before_it() {
    let blob = version_blob(
        None,
        &[
            ("FileVersion", "4.25.1"),
            ("ProductVersion", "4.21.1"),
            ("Comments", "after"),
        ],
    );
    let key = utf16z("ProductVersion");
    let at = blob
        .windows(key.len())
        .position(|w| w == key.as_slice())
        .unwrap()
        - 6;
    for (what, length) in [("oversized", 0xFFF0u16), ("zero", 0), ("tiny", 2)] {
        let mut damaged = blob.clone();
        damaged[at..at + 2].copy_from_slice(&length.to_le_bytes());
        let (facts, claimed) = claimed_and_problems(damaged);
        assert_eq!(claimed.as_deref(), Some("4.25.1"), "{what}");
        let resource = facts.resource.as_ref().unwrap();
        assert_eq!(resource.string("FileVersion"), Some("4.25.1"), "{what}");
        assert_eq!(resource.string("ProductVersion"), None, "{what}");
        assert_eq!(
            resource.problems.len(),
            1,
            "{what}: {:?}",
            resource.problems
        );
        assert!(
            facts.engine.evidence.iter().any(|l| l.contains("damaged")),
            "{what}"
        );
    }
}

#[test]
fn string_values_beyond_the_limit_are_dropped_not_truncated() {
    let long = "9".repeat(MAX_VERSION_VALUE_UNITS + 10);
    let blob = version_blob(
        None,
        &[("ProductVersion", &long), ("FileVersion", "4.22.1")],
    );
    // `string_block` writes the true length; the limit applies to the text.
    let (facts, claimed) = claimed_and_problems(blob);
    assert_eq!(claimed.as_deref(), Some("4.22.1"));
    let resource = facts.resource.unwrap();
    assert_eq!(resource.string("ProductVersion"), None);
    assert_eq!(resource.problems.len(), 1);
}

#[test]
fn a_stated_value_length_is_not_trusted() {
    // Real files state `wValueLength` in bytes as often as in characters,
    // and some state nonsense. The text still reads.
    let mut blob = version_blob(None, &[("ProductVersion", "4.21.1")]);
    let key = utf16z("ProductVersion");
    let at = blob
        .windows(key.len())
        .position(|w| w == key.as_slice())
        .unwrap()
        - 6;
    for value_length in [0x7FFFu16, 1, 100] {
        blob[at + 2..at + 4].copy_from_slice(&value_length.to_le_bytes());
        let (_, claimed) = claimed_and_problems(blob.clone());
        assert_eq!(
            claimed.as_deref(),
            Some("4.21.1"),
            "wValueLength {value_length}"
        );
    }
}

#[test]
fn many_blocks_stop_at_the_limit() {
    let strings: Vec<(String, String)> = (0..700)
        .map(|i| (format!("Key{i}"), "v".to_owned()))
        .collect();
    let refs: Vec<(&str, &str)> = strings
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    let blob = version_blob(None, &refs);
    assert!(
        blob.len() < MAX_VERSION_RESOURCE_BYTES as usize,
        "fixture fits one block"
    );
    let (facts, _) = claimed_and_problems(blob);
    let resource = facts.resource.unwrap();
    assert!(
        resource.strings.len() <= MAX_VERSION_BLOCKS,
        "{}",
        resource.strings.len()
    );
    assert!(resource.strings.len() > 400, "{}", resource.strings.len());
    assert!(!resource.problems.is_empty());
}

// ---- headers read directly from memory --------------------------------------------

#[test]
fn headers_read_from_unmapped_or_overflowing_memory_are_errors() {
    let bytes = pe_with_version(&[("ProductVersion", "4.21.1")]).to_bytes();
    let (full, module) = load(&bytes).unwrap();

    // Too short for a DOS header.
    let short = Snapshot::builder()
        .region(0x1000, vec![b'M', b'Z'])
        .build()
        .unwrap();
    assert!(matches!(
        PeImage::parse_from_memory(&short, 0x1000),
        Err(ScanError::Memory(_))
    ));
    // Nothing mapped there at all.
    assert!(PeImage::parse_from_memory(&full, 0x10).is_err());
    assert!(PeImage::parse_from_memory(&full, u64::MAX).is_err());
    assert!(PeImage::parse_from_memory(&full, u64::MAX - 0x20).is_err());
    // NT headers beyond the mapped page.
    let mut head = full.read_vec::<u8>(module.base, 0x400).unwrap();
    put32(&mut head, 0x3C, 0x8000);
    let far = Snapshot::builder().region(0x1000, head).build().unwrap();
    assert!(matches!(
        PeImage::parse_from_memory(&far, 0x1000),
        Err(ScanError::Memory(_))
    ));
    // The image at the top of the address space: every address computation
    // overflows or fails, none panics.
    let mut head = full.read_vec::<u8>(module.base, 0x400).unwrap();
    put32(&mut head, 0x3C, 0x80);
    let top_base = u64::MAX - 0x3FF - 1;
    let top = Snapshot::builder().region(top_base, head).build().unwrap();
    let _ = PeImage::parse_from_memory(&top, top_base);
}

// ---- sweeps -----------------------------------------------------------------------

/// An image with a version resource, a build string and a marker.
fn sweep_image(pe32: bool) -> (Vec<u8>, std::ops::Range<usize>) {
    let blob = version_blob(
        Some(([4, 25, 1, 0], [4, 25, 1, 0])),
        &[
            ("FileVersion", "4.25.1"),
            ("ProductVersion", "4.25.1-1234+++UE4+Release-4.25"),
        ],
    );
    let rsrc = rsrc_with_versions(RSRC_RVA, &[(0x409, blob)]);
    let rdata = rdata_with(
        0x200,
        &[
            (0x10, utf16z("++Game+Live-CL-99")),
            (0x80, b"\0FieldPathProperty\0".to_vec()),
        ],
    );
    let mut spec = standard_pe(rdata, vec![0; 0x100], Some(rsrc.clone()));
    spec.pe32 = pe32;
    if pe32 {
        spec.image_base = 0x40_0000;
    }
    spec.entry = TEXT_RVA;
    let bytes = spec.to_bytes();
    let start = bytes.len() - rsrc.len().next_multiple_of(FILE_ALIGN);
    (bytes, start..start + rsrc.len())
}

/// Runs everything this crate does on `bytes`; must not panic.
fn exercise(bytes: &[u8]) -> bool {
    let Ok((snapshot, module)) = load(bytes) else {
        return false;
    };
    let Ok(image) = PeImage::parse_from_memory(&snapshot, module.base) else {
        return false;
    };
    let facts = detect_version(&snapshot, &image);
    let _ = facts.engine.evidence.len();
    true
}

#[test]
fn flipping_every_header_and_resource_byte_never_panics() {
    let mut ok = 0usize;
    let mut failed = 0usize;
    for pe32 in [false, true] {
        let (bytes, rsrc) = sweep_image(pe32);
        assert!(exercise(&bytes), "the sweep image itself must load");
        let positions = (0..HEADERS_SIZE).chain(rsrc);
        for at in positions {
            for mutate in [
                (|b: u8| b ^ 0xFF) as fn(u8) -> u8,
                |_| 0x00,
                |_| 0xFF,
                |b| b.wrapping_add(1),
                |b| b ^ 0x80,
            ] {
                let mut damaged = bytes.clone();
                damaged[at] = mutate(damaged[at]);
                if exercise(&damaged) {
                    ok += 1;
                } else {
                    failed += 1;
                }
            }
        }
    }
    // Both outcomes occur: some flips are harmless, many are caught.
    assert!(ok > 1000, "{ok} flips loaded");
    assert!(failed > 100, "{failed} flips failed");
}

#[test]
fn random_multi_byte_damage_never_panics() {
    let (bytes, rsrc) = sweep_image(false);
    let mut state = 0x9E37_79B9_7F4A_7C15u64;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    let span: Vec<usize> = (0..HEADERS_SIZE).chain(rsrc).collect();
    for _ in 0..4000 {
        let mut damaged = bytes.clone();
        for _ in 0..(1 + next() % 4) {
            let at = span[(next() % span.len() as u64) as usize];
            damaged[at] = next() as u8;
        }
        let _ = exercise(&damaged);
    }
}

#[test]
fn truncating_the_resource_section_never_panics() {
    let (bytes, rsrc) = sweep_image(false);
    for len in (rsrc.start..bytes.len()).step_by(7) {
        let _ = exercise(&bytes[..len]);
    }
}
