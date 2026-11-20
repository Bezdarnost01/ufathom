#![allow(
    clippy::indexing_slicing,
    clippy::unwrap_used,
    reason = "test fixture has fixed offsets and must fail on an unexpected result"
)]

use super::*;
use ufathom_engine::LoadOptions;
use ufathom_testkit::{FixtureBuilder, Profile};

const DECODING: [(u32, usize); 2] = [(0x18, 32), (0x1c, 32)];

fn samples(number_follows_indices: bool) -> (Vec<Sample>, HashSet<u64>) {
    let addresses: [u64; 2] = [0x0001_0002_0000, 0x0002_0003_0000];
    let samples = (0u32..32)
        .map(|index| {
            let mut bytes = vec![0; OBJECT_PROBE_BYTES];
            let pointer = addresses[index as usize % 2];
            bytes[0x10..0x18].copy_from_slice(&pointer.to_le_bytes());
            if number_follows_indices {
                bytes[0x20..0x24].copy_from_slice(&(index % 3 + 1).to_le_bytes());
            }
            Sample {
                index,
                addr: pointer,
                bytes,
            }
        })
        .collect();
    (samples, addresses.into_iter().collect())
}

#[test]
fn class_pointer_half_is_not_an_fname_instance_number() {
    let fixture = FixtureBuilder::new(Profile::ue4_25_case_preserving_measured()).build();
    let table = NameTable::load(
        &fixture.memory,
        &fixture.layout.names,
        &LoadOptions::default(),
    )
    .unwrap();
    let (samples, objects) = samples(false);
    let class_candidates = object_pointer_offsets(&samples, &objects, false);
    assert_eq!(class_candidates, vec![0x10]);
    let (fname, offset) = solve_fname(&table, &DECODING, &samples, 0xc, &class_candidates).unwrap();
    assert_eq!(offset, 0x18);
    assert_eq!(fname.number, None);
}

#[test]
fn a_real_instance_number_after_the_indices_is_still_recognized() {
    let fixture = FixtureBuilder::new(Profile::ue4_25_case_preserving_measured()).build();
    let table = NameTable::load(
        &fixture.memory,
        &fixture.layout.names,
        &LoadOptions::default(),
    )
    .unwrap();
    let (samples, objects) = samples(true);
    let class_candidates = object_pointer_offsets(&samples, &objects, false);
    let (fname, offset) = solve_fname(&table, &DECODING, &samples, 0xc, &class_candidates).unwrap();
    assert_eq!(offset, 0x18);
    assert_eq!(fname.number, Some(8));
}

#[test]
fn instance_numbers_can_include_rare_suffixes_near_i32_max() {
    let mut values = vec![0u32; 128];
    values[1..5].copy_from_slice(&[2, 3, 0x7fff_fc0d, 0x7fff_fbc5]);
    let (samples, _) = samples(false);
    assert!(looks_like_number(&values, &samples));
    values[3..5].copy_from_slice(&[0x12ab_fc0d, 0x3456_fbc5]);
    assert!(!looks_like_number(&values, &samples));
}
