//! Properties of the images themselves: every stock profile reads back as its
//! truth, images are deterministic, the PE parses, decoys are real decoys,
//! corruptions change what they say they change, and shuffled profiles are
//! valid and distinct.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::cast_possible_truncation,
    reason = "test code may panic on failure; clippy.toml only exempts #[test] functions, not helpers"
)]

mod common;

use std::collections::BTreeSet;

use common::lr::Lr;
use ufathom_mem::{MemoryReader, MemoryReaderExt, Perms, RegionKind};
use ufathom_model::EngineVersion;
use ufathom_model::flags::RF_CLASS_DEFAULT_OBJECT;
use ufathom_model::layout::{NameTableLayout, ObjectArrayKind, UEnumLayout};
use ufathom_testkit::{
    BuildOptions, ChainOrder, Corruption, Fixture, FixtureBuilder, PartKind, PoolEntryFormat,
    Profile, ProfileError,
};

/// Every supported minor version, with and without case-preserving names
/// where the version has a display index.
fn all_stock_profiles() -> Vec<Profile> {
    let mut out = Vec::new();
    let versions = (11..=27).map(|m| (4, m)).chain((0..=8).map(|m| (5, m)));
    for (major, minor) in versions {
        let version = EngineVersion {
            major,
            minor,
            patch: 0,
        };
        out.push(Profile::stock(version).unwrap());
        if major == 5 || minor >= 22 {
            let options = BuildOptions {
                case_preserving_names: true,
                pool_entry: None,
            };
            out.push(Profile::with_options(version, options).unwrap());
        }
    }
    out
}

fn small(profile: Profile) -> Fixture {
    FixtureBuilder::new(profile)
        .seed(3)
        .filler_objects(300)
        .build()
}

#[test]
fn every_stock_profile_is_valid_and_reads_back_as_its_truth() {
    let profiles = all_stock_profiles();
    assert!(profiles.len() >= 40);
    for profile in profiles {
        let name = profile.name.clone();
        profile.validate().unwrap_or_else(|e| panic!("{name}: {e}"));
        let fx = small(profile);
        let lr = Lr::new(&fx);

        // Every slot that holds an object stores its own index, and every
        // object's name decodes.
        let num = lr.num_elements();
        assert!(num <= lr.max_elements(), "{name}");
        let mut occupied = 0;
        for i in 0..num {
            if let Some(object) = lr.object(i) {
                assert_eq!(lr.stored_index(object), Some(i as u32), "{name} slot {i}");
                assert!(lr.name_of(object).is_some(), "{name} slot {i} name");
                occupied += 1;
            }
        }
        assert_eq!(occupied, fx.objects.len(), "{name}");
        assert_eq!(lr.name_at(0).as_deref(), Some("None"), "{name}");

        let members = lr.check_reflection();
        assert!(members > 80, "{name}: only {members} members checked");
    }
}

#[test]
fn unsupported_combinations_are_rejected() {
    let v = |major, minor| EngineVersion {
        major,
        minor,
        patch: 0,
    };
    assert!(matches!(
        Profile::stock(v(4, 10)),
        Err(ProfileError::UnsupportedVersion(_))
    ));
    assert!(matches!(
        Profile::stock(v(4, 28)),
        Err(ProfileError::UnsupportedVersion(_))
    ));
    assert!(matches!(
        Profile::stock(v(3, 7)),
        Err(ProfileError::UnsupportedVersion(_))
    ));
    assert!(matches!(
        Profile::stock(v(5, 9)),
        Err(ProfileError::UnsupportedVersion(_))
    ));
    let cp = BuildOptions {
        case_preserving_names: true,
        pool_entry: None,
    };
    assert!(matches!(
        Profile::with_options(v(4, 21), cp),
        Err(ProfileError::CasePreservingBeforeDisplayIndex(_))
    ));
    let pool = BuildOptions {
        case_preserving_names: false,
        pool_entry: Some(PoolEntryFormat {
            comparison_id: false,
            len_shift: 1,
        }),
    };
    assert!(matches!(
        Profile::with_options(v(4, 22), pool),
        Err(ProfileError::PoolEntryWithoutPool(_))
    ));
    assert!(Profile::with_options(v(4, 23), pool).is_ok());
}

#[test]
fn eras_differ_where_the_type_descriptions_differ() {
    let layout = |major, minor| {
        Profile::stock(EngineVersion {
            major,
            minor,
            patch: 0,
        })
        .unwrap()
        .layout
    };
    assert_eq!(layout(4, 11).objects.kind, ObjectArrayKind::Fixed);
    assert_eq!(layout(4, 19).objects.kind, ObjectArrayKind::Fixed);
    assert!(matches!(
        layout(4, 20).objects.kind,
        ObjectArrayKind::Chunked { .. }
    ));
    assert!(matches!(layout(4, 22).names, NameTableLayout::Array(_)));
    assert!(matches!(layout(4, 23).names, NameTableLayout::Pool(_)));
    assert!(layout(4, 22).ustruct.child_properties.is_none());
    assert!(layout(4, 25).ustruct.child_properties.is_some());
    assert_eq!(layout(5, 5).ustruct.min_alignment_width, 4);
    assert_eq!(layout(5, 6).ustruct.min_alignment_width, 2);
    assert!(matches!(layout(5, 6).uenum, UEnumLayout::PairArray { .. }));
    assert!(matches!(
        layout(5, 7).uenum,
        UEnumLayout::SplitArrays { .. }
    ));
    // jmap: Objects first, then NumElements, in UE 5.8.
    assert_eq!(layout(5, 8).objects.num_elements, 8);
    assert_eq!(layout(5, 7).objects.num_elements, 0x14);
    // The 5.3 FField owner has no bIsUObject byte, so Next moves up by 8.
    let next = |major, minor| match layout(major, minor).properties.system {
        ufathom_model::layout::PropertySystem::Modern { ffield, .. } => ffield.next,
        ufathom_model::layout::PropertySystem::Legacy => panic!("legacy"),
    };
    assert_eq!(next(5, 2), 0x20);
    assert_eq!(next(5, 3), 0x18);
    // Before 4.18 NumParms follows a RepOffset.
    let shape = |minor| {
        Profile::stock(EngineVersion {
            major: 4,
            minor,
            patch: 0,
        })
        .unwrap()
        .shape
    };
    assert_eq!(shape(17).ufunction_parm_block, 6);
    assert_eq!(shape(18).ufunction_parm_block, 4);
    // Soft pointers were asset pointers before 4.15.
    let fx = small(
        Profile::stock(EngineVersion {
            major: 4,
            minor: 11,
            patch: 0,
        })
        .unwrap(),
    );
    let lr = Lr::new(&fx);
    let actor = fx.object("/Script/Engine.Actor").unwrap().address;
    let classes: Vec<String> = lr
        .properties(actor)
        .into_iter()
        .map(|(a, _)| lr.property_class_name(a).unwrap())
        .collect();
    assert!(classes.contains(&"AssetObjectProperty".to_owned()));
    assert!(!classes.contains(&"EnumProperty".to_owned()));
}

fn parts_equal(a: &Fixture, b: &Fixture) -> bool {
    a.parts.len() == b.parts.len()
        && a.parts
            .iter()
            .zip(&b.parts)
            .all(|(x, y)| x.addr == y.addr && x.bytes == y.bytes)
}

#[test]
fn builds_are_deterministic_across_all_stock_profiles() {
    for profile in all_stock_profiles() {
        let a = FixtureBuilder::new(profile.clone())
            .seed(9)
            .filler_objects(150)
            .build();
        let b = FixtureBuilder::new(profile.clone())
            .seed(9)
            .filler_objects(150)
            .build();
        assert!(parts_equal(&a, &b), "{} is not deterministic", profile.name);
        assert_eq!(a.truth, b.truth);
        assert_eq!(a.layout, b.layout);
        let c = FixtureBuilder::new(profile.clone())
            .seed(10)
            .filler_objects(150)
            .build();
        assert!(
            !parts_equal(&a, &c),
            "{}: seeds do not matter",
            profile.name
        );
    }
}

#[test]
fn the_pe_parses_and_the_version_resource_is_reachable() {
    for profile in [
        Profile::ue4_21_measured(),
        Profile::ue4_25_case_preserving_measured(),
        Profile::ue5_case_preserving_assumed(),
    ] {
        let fx = small(profile.clone());
        let mem = &fx.memory;
        let base = fx.addresses.module_base;
        assert_eq!(mem.read_u16(base).unwrap(), 0x5A4D, "MZ");
        let nt = base + u64::from(mem.read_u32(base + 0x3C).unwrap());
        assert_eq!(mem.read_u32(nt).unwrap(), 0x0000_4550, "PE\\0\\0");
        assert_eq!(mem.read_u16(nt + 4).unwrap(), 0x8664, "x64");
        let sections = u64::from(mem.read_u16(nt + 6).unwrap());
        assert_eq!(sections, 4);
        let optional = nt + 24;
        assert_eq!(mem.read_u16(optional).unwrap(), 0x20B);
        assert_eq!(mem.read_u64(optional + 24).unwrap(), base, "ImageBase");
        let table = optional + u64::from(mem.read_u16(nt + 20).unwrap());
        let expected = [
            (".text", Perms::RX),
            (".rdata", Perms::R),
            (".data", Perms::RW),
            (".rsrc", Perms::R),
        ];
        let regions = mem.regions().unwrap();
        for (i, (name, perms)) in expected.iter().enumerate() {
            let header = table + i as u64 * 40;
            let mut raw = [0u8; 8];
            mem.read_exact(header, &mut raw).unwrap();
            let text = String::from_utf8_lossy(&raw)
                .trim_end_matches('\0')
                .to_owned();
            assert_eq!(&text, name);
            let va = base + u64::from(mem.read_u32(header + 12).unwrap());
            let region = regions
                .iter()
                .find(|r| r.base == va)
                .unwrap_or_else(|| panic!("{name} is mapped"));
            assert_eq!(region.perms, *perms, "{name}");
            assert_eq!(region.kind, RegionKind::Image, "{name}");
        }
        let module = mem.modules().unwrap();
        assert_eq!(module.len(), 1);
        assert_eq!(module[0].name, "Game-Win64-Shipping.exe");
        assert_eq!(module[0].base, base);

        // Walk IMAGE_DIRECTORY_ENTRY_RESOURCE by hand: type 16, id 1, language, data.
        let dir = optional + 112 + 2 * 8;
        let root = base + u64::from(mem.read_u32(dir).unwrap());
        let find_entry = |directory: u64, wanted: u32| {
            let named = u64::from(mem.read_u16(directory + 12).unwrap());
            let ids = u64::from(mem.read_u16(directory + 14).unwrap());
            (0..named + ids)
                .map(|n| directory + 16 + n * 8)
                .find(|&e| mem.read_u32(e).unwrap() == wanted)
                .map(|e| mem.read_u32(e + 4).unwrap())
                .expect("entry exists")
        };
        let level2 = root + u64::from(find_entry(root, 16) & 0x7FFF_FFFF);
        let level3 = root + u64::from(find_entry(level2, 1) & 0x7FFF_FFFF);
        let leaf = root + u64::from(find_entry(level3, 0x409));
        let data = base + u64::from(mem.read_u32(leaf).unwrap());
        let size = mem.read_u32(leaf + 4).unwrap() as usize;
        let blob = mem.read_vec::<u8>(data, size).unwrap();
        assert_eq!(
            u16::from_le_bytes([blob[0], blob[1]]) as usize,
            size,
            "wLength"
        );
        let key: Vec<u8> = "VS_VERSION_INFO\0"
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect();
        assert_eq!(&blob[6..6 + key.len()], &key[..]);
        let fixed = (6 + key.len()).next_multiple_of(4);
        let word = |at: usize| u32::from_le_bytes(blob[at..at + 4].try_into().unwrap());
        assert_eq!(word(fixed), 0xFEEF_04BD);
        let (major, minor, patch) = profile.identity.version_parts();
        assert_eq!(
            word(fixed + 16),
            (u32::from(major) << 16) | u32::from(minor),
            "ProductVersion MS"
        );
        assert_eq!(
            word(fixed + 20),
            u32::from(patch) << 16,
            "ProductVersion LS"
        );
        // Find the ProductVersion string by hand.
        let needle: Vec<u8> = "ProductVersion\0"
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect();
        let at = blob
            .windows(needle.len())
            .position(|w| w == &needle[..])
            .expect("ProductVersion key");
        let value_start = (at + needle.len()).next_multiple_of(4);
        let units: Vec<u16> = blob[value_start..]
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .take_while(|&u| u != 0)
            .collect();
        assert_eq!(
            String::from_utf16(&units).unwrap(),
            profile.identity.product_version
        );
        assert_eq!(
            fx.engine.claimed.as_deref(),
            Some(profile.identity.product_version.as_str())
        );

        // The build string is in .rdata as UTF-16.
        let rdata = fx.parts.iter().find(|p| p.kind == PartKind::Rdata).unwrap();
        let wanted: Vec<u8> = fx
            .engine
            .build
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect();
        assert!(rdata.bytes.windows(wanted.len()).any(|w| w == &wanted[..]));
        assert!(fx.engine.build.starts_with("++Game+Live-CL-"));
    }
}

#[test]
fn heap_regions_are_private_with_unmapped_gaps() {
    let fx = small(Profile::ue4_25_case_preserving_measured());
    let regions = fx.memory.regions().unwrap();
    let heap: Vec<_> = regions
        .iter()
        .filter(|r| r.kind == RegionKind::Private)
        .collect();
    assert!(heap.len() >= 6);
    for pair in heap.windows(2) {
        assert!(
            pair[0].base + pair[0].size < pair[1].base,
            "a gap separates heap regions"
        );
    }
    assert!(fx.memory.read_u8(fx.anchors.unmapped).is_err());
}

#[test]
fn the_name_table_spans_blocks_and_looks_like_a_game() {
    for profile in [
        Profile::ue4_22_case_preserving_measured(),
        Profile::ue4_25_case_preserving_measured(),
    ] {
        let fx = FixtureBuilder::new(profile.clone())
            .seed(4)
            .filler_objects(6000)
            .name_blocks(2)
            .build();
        let lr = Lr::new(&fx);
        let slots = &fx.anchors.data.names.block_slots;
        assert!(slots.len() >= 2, "{}: {} blocks", profile.name, slots.len());
        for slot in slots {
            assert_ne!(lr.u64(*slot).unwrap(), 0);
        }
        // Instance numbers 1 to 4 are common, indices are spread out.
        let mut numbers = [0usize; 6];
        let mut total_index = 0u64;
        let mut counted = 0u64;
        for object in fx
            .objects
            .iter()
            .filter(|o| o.path.starts_with("/Game/Maps/Main.Main:PersistentLevel."))
        {
            let name = fx.anchors.data.names.entries.len();
            let _ = name;
            let at = object.address + u64::from(fx.layout.uobject.name);
            let number = lr
                .u32(at + u64::from(fx.layout.fname.number.unwrap()))
                .unwrap() as usize;
            numbers[number.min(5)] += 1;
            total_index += u64::from(lr.u32(at).unwrap());
            counted += 1;
        }
        assert!(counted > 5000, "{} fillers", counted);
        for (n, count) in numbers.iter().enumerate().take(5).skip(1) {
            assert!(*count > 200, "number {n} appears {count} times");
        }
        assert!(total_index / counted > 100, "name indices are not trivial");
    }
    let fx = FixtureBuilder::new(Profile::ue4_21_measured())
        .seed(4)
        .filler_objects(6000)
        .name_blocks(2)
        .build();
    assert!(fx.anchors.data.names.blocks.len() >= 2, "two array chunks");
    let array = fx.layout.names.clone();
    let NameTableLayout::Array(array) = array else {
        panic!("array")
    };
    assert_eq!(array.elements_per_chunk, 16384);
}

/// The naive shape check a solver would apply to a candidate object array.
fn naive_array_shape(lr: &Lr<'_>, fx: &Fixture, base: u64) -> bool {
    let l = &fx.layout.objects;
    let r = |offset: u32| lr.u32(base + u64::from(offset)).unwrap();
    let (max, num) = (r(l.max_elements), r(l.num_elements));
    if num > max || lr.u64(base + u64::from(l.objects)).is_none_or(|p| p == 0) {
        return false;
    }
    match (l.max_chunks, l.num_chunks) {
        (Some(mc), Some(nc)) => {
            let (max_chunks, num_chunks) = (r(mc), r(nc));
            max_chunks > 0
                && num_chunks <= max_chunks
                && max % max_chunks == 0
                && num_chunks == num.div_ceil(max / max_chunks)
        }
        _ => true,
    }
}

/// How many of the first `n` items of an array at `base` satisfy the
/// self-index check.
fn self_index_hits(fx: &Fixture, base: u64, n: u64) -> u64 {
    let l = &fx.layout.objects;
    let lr = Lr::new(fx);
    let table = lr.u64(base + u64::from(l.objects)).unwrap();
    let size = u64::from(l.item_size);
    let max_chunks = l.max_chunks.map(|o| lr.u32(base + u64::from(o)).unwrap());
    let max = u64::from(lr.u32(base + u64::from(l.max_elements)).unwrap());
    (0..n)
        .filter(|&i| {
            let item = match l.kind {
                ObjectArrayKind::Fixed => table + i * size,
                ObjectArrayKind::Chunked { .. } => {
                    let per = max / u64::from(max_chunks.unwrap());
                    let chunk = lr.u64(table + (i / per) * 8).unwrap();
                    chunk + (i % per) * size
                }
            };
            let object = lr.u64(item + u64::from(l.item_object)).unwrap();
            object != 0 && lr.stored_index(object) == Some(i as u32)
        })
        .count() as u64
}

#[test]
fn decoys_pass_a_naive_check_and_fail_a_real_one() {
    for profile in [
        Profile::ue4_21_measured(),
        Profile::ue4_25_case_preserving_measured(),
        Profile::stock(EngineVersion {
            major: 4,
            minor: 11,
            patch: 0,
        })
        .unwrap(),
        Profile::shuffled(5),
    ] {
        let name = profile.name.clone();
        let fx = small(profile);
        let lr = Lr::new(&fx);
        let data = &fx.anchors.data;

        // Object array: the decoy has plausible numbers, its items fail the self-index check.
        assert_ne!(data.decoy_array, data.object_array);
        assert!(
            naive_array_shape(&lr, &fx, data.object_array),
            "{name} real"
        );
        assert!(
            naive_array_shape(&lr, &fx, data.decoy_array),
            "{name} decoy passes the shape check"
        );
        let real = lr.num_elements();
        let occupied = (0..real.min(300))
            .filter(|&i| lr.object(i).is_some())
            .count() as u64;
        assert_eq!(
            self_index_hits(&fx, data.object_array, real.min(300)),
            occupied,
            "{name}"
        );
        let l = &fx.layout.objects;
        let decoy_num = u64::from(
            lr.u32(data.decoy_array + u64::from(l.num_elements))
                .unwrap(),
        );
        assert!(decoy_num > 100);
        let hits = self_index_hits(&fx, data.decoy_array, decoy_num);
        assert!(
            hits * 100 <= decoy_num,
            "{name}: decoy items satisfy the self-index check {hits}/{decoy_num}"
        );

        // Counters followed by heap pointers: the lengths agree, the blocks are not names.
        if let NameTableLayout::Pool(pool) = &fx.layout.names {
            let counters = data.decoy_counters;
            let current = lr.u32(counters + u64::from(pool.current_block)).unwrap();
            let run = (0..)
                .map(|i| lr.u64(counters + u64::from(pool.blocks) + i * 8).unwrap())
                .take_while(|&p| p != 0)
                .collect::<Vec<_>>();
            assert_eq!(
                run.len() as u32,
                current + 1,
                "{name}: run length matches the counter"
            );
            let first = run[0];
            assert_ne!(
                lr.fx
                    .memory
                    .read_vec::<u8>(first + u64::from(pool.entry.string_offset), 4)
                    .unwrap(),
                b"None"
            );

            // A pointer to a buffer that starts with `None` but continues with something else.
            let target = lr.u64(data.decoy_name_slot).unwrap();
            let entry = |at: u64| {
                let header = u64::from(lr.u16(at + u64::from(pool.entry.header_offset)).unwrap());
                let len = (header >> pool.entry.len_shift) as usize;
                let chars = lr
                    .fx
                    .memory
                    .read_vec::<u8>(at + u64::from(pool.entry.string_offset), len)
                    .unwrap();
                (String::from_utf8(chars).unwrap(), len)
            };
            let (first_name, len) = entry(target);
            assert_eq!(first_name, "None");
            let step = (pool.entry.string_offset + len as u32).next_multiple_of(pool.stride);
            let (second, _) = entry(target + u64::from(step));
            assert_ne!(second, "ByteProperty");
            assert_eq!(second, "ByteBanana");
            // The real table does have ByteProperty second.
            let real_second = (pool.entry.string_offset + 4).div_ceil(pool.stride);
            assert_eq!(lr.name_at(real_second).as_deref(), Some("ByteProperty"));
        } else {
            let target = lr.u64(data.decoy_name_slot).unwrap();
            let NameTableLayout::Array(array) = &fx.layout.names else {
                unreachable!()
            };
            let chunk = lr.u64(target).unwrap();
            let entry = |n: u64| lr.u64(chunk + n * 8).unwrap();
            let text = |at: u64| {
                let mut bytes = Vec::new();
                for i in 0.. {
                    let b = lr
                        .u8(at + u64::from(array.entry.string_offset) + i)
                        .unwrap();
                    if b == 0 {
                        break;
                    }
                    bytes.push(b);
                }
                String::from_utf8(bytes).unwrap()
            };
            assert_eq!(text(entry(0)), "None");
            assert_ne!(text(entry(1)), "ByteProperty");
            assert_eq!(lr.name_at(1).as_deref(), Some("ByteProperty"));
        }

        // A stale World-looking object: class and name look right, the slot disagrees.
        let stale = lr.u64(data.stale_world_slot).unwrap();
        assert_eq!(stale, fx.anchors.stale_world);
        assert_ne!(stale, fx.anchors.world);
        assert_eq!(lr.class_name_of(stale).as_deref(), Some("World"));
        let stored = lr.stored_index(stale).unwrap();
        assert_ne!(
            lr.object(u64::from(stored)),
            Some(stale),
            "{name}: the slot belongs to someone else"
        );
        // Default objects are World-looking too, but carry the CDO flag.
        let world_cdo = lr.u64(data.world_cdo_slot).unwrap();
        assert_eq!(lr.class_name_of(world_cdo).as_deref(), Some("World"));
        let flags = lr
            .u32(world_cdo + u64::from(fx.layout.uobject.flags))
            .unwrap();
        assert_ne!(flags & RF_CLASS_DEFAULT_OBJECT, 0);
        // The real GWorld is a plain instance.
        let gworld = lr.u64(data.gworld).unwrap();
        assert_eq!(gworld, fx.anchors.world);
        assert_eq!(
            lr.u32(gworld + u64::from(fx.layout.uobject.flags)).unwrap() & RF_CLASS_DEFAULT_OBJECT,
            0
        );
    }
}

#[test]
fn process_event_has_the_signature_and_the_decoys_do_not() {
    let fx = small(Profile::ue4_25_case_preserving_measured());
    let text = fx.parts.iter().find(|p| p.kind == PartKind::Text).unwrap();
    let disp = fx.layout.ufunction.flags.to_le_bytes();
    // Every `F7 /0` test of FunctionFlags in .text, with its immediate.
    let mut hits: Vec<(usize, u32)> = Vec::new();
    for (i, w) in text.bytes.windows(10).enumerate() {
        if w[0] == 0xF7 && (0x80..0x88).contains(&w[1]) && w[2..6] == disp {
            hits.push((i, u32::from_le_bytes(w[6..10].try_into().unwrap())));
        }
    }
    let pe = (fx.anchors.process_event - text.addr) as usize;
    let in_pe: Vec<u32> = hits
        .iter()
        .filter(|(i, _)| *i >= pe && *i < pe + 64)
        .map(|h| h.1)
        .collect();
    assert!(in_pe.contains(&0x400) && in_pe.contains(&0x40_0000));
    let decoys = fx.anchors.decoy_functions.len();
    assert_eq!(decoys, 4);
    // Functions with the right displacement and the Native flag: ProcessEvent
    // and the native-only decoy; none of the others have both.
    let both = fx
        .anchors
        .decoy_functions
        .iter()
        .chain(std::iter::once(&fx.anchors.process_event))
        .filter(|&&f| {
            let start = (f - text.addr) as usize;
            let flags: Vec<u32> = hits
                .iter()
                .filter(|(i, _)| *i >= start && *i < start + 48)
                .map(|h| h.1)
                .collect();
            flags.contains(&0x400) && flags.contains(&0x40_0000)
        })
        .count();
    assert_eq!(
        both, 1,
        "only ProcessEvent has both tests at the FunctionFlags displacement"
    );
}

fn count_null_slots(lr: &Lr<'_>) -> u64 {
    (0..lr.num_elements())
        .filter(|&i| lr.object(i).is_none())
        .count() as u64
}

fn wrong_index_slots(lr: &Lr<'_>) -> u64 {
    (0..lr.num_elements())
        .filter(|&i| {
            lr.object(i)
                .is_some_and(|o| lr.stored_index(o) != Some(i as u32))
        })
        .count() as u64
}

fn changed_bytes(a: &Fixture, b: &Fixture) -> usize {
    assert_eq!(a.parts.len(), b.parts.len());
    a.parts
        .iter()
        .zip(&b.parts)
        .map(|(x, y)| {
            assert_eq!(
                (x.addr, x.bytes.len()),
                (y.addr, y.bytes.len()),
                "the layout of parts is stable"
            );
            x.bytes.iter().zip(&y.bytes).filter(|(p, q)| p != q).count()
        })
        .sum()
}

#[test]
fn each_corruption_changes_the_bytes_in_the_intended_way() {
    for profile in [
        Profile::ue4_21_measured(),
        Profile::ue4_25_case_preserving_measured(),
        Profile::stock(EngineVersion {
            major: 5,
            minor: 7,
            patch: 0,
        })
        .unwrap(),
    ] {
        let clean = FixtureBuilder::new(profile.clone())
            .seed(21)
            .filler_objects(400)
            .build();
        for corruption in Corruption::ALL {
            let name = format!("{} / {corruption:?}", profile.name);
            let dirty = FixtureBuilder::new(profile.clone())
                .seed(21)
                .filler_objects(400)
                .corrupt(corruption)
                .build();
            assert!(changed_bytes(&clean, &dirty) > 0, "{name} changes nothing");
            assert_eq!(dirty.corruptions, [corruption]);
            let (c, d) = (Lr::new(&clean), Lr::new(&dirty));
            let l = &dirty.layout;
            let addr = |fx: &Fixture, path: &str| fx.object(path).unwrap().address;
            let actor = addr(&dirty, "/Script/Engine.Actor");
            let pawn = addr(&dirty, "/Script/Engine.Pawn");
            match corruption {
                Corruption::NullObjectSlots => {
                    assert!(count_null_slots(&d) > count_null_slots(&c) + 5, "{name}");
                }
                Corruption::ClassChainCycle => {
                    assert_eq!(d.class_of(actor), Some(pawn), "{name}");
                    assert_eq!(d.class_of(pawn), Some(actor), "{name}");
                    assert_ne!(c.class_of(actor), Some(pawn));
                }
                Corruption::SuperChainCycle => {
                    let sup = u64::from(l.ustruct.super_struct);
                    assert_eq!(d.u64(actor + sup), Some(pawn), "{name}");
                    assert_eq!(d.u64(pawn + sup), Some(actor), "{name}");
                }
                Corruption::PropertyListCycle => {
                    let guid = addr(&dirty, "/Script/CoreUObject.Guid");
                    let members = d.properties(guid);
                    assert_eq!(
                        members.len(),
                        4,
                        "{name}: the cycle is cut by the visited set"
                    );
                    let next = match &l.properties.system {
                        ufathom_model::layout::PropertySystem::Modern { ffield, .. } => ffield.next,
                        ufathom_model::layout::PropertySystem::Legacy => l.ufield.next,
                    };
                    let last = members.last().unwrap().0;
                    assert_eq!(d.u64(last + u64::from(next)), Some(members[0].0), "{name}");
                }
                Corruption::HugeArrayCounts => {
                    for e in &dirty.truth.enums {
                        let object = addr(&dirty, &e.path);
                        let count = match l.uenum {
                            UEnumLayout::PairArray { offset, .. } => {
                                d.u32(object + u64::from(offset) + 8)
                            }
                            UEnumLayout::SplitArrays { count, .. } => {
                                d.u32(object + u64::from(count))
                            }
                        };
                        assert_eq!(count, Some(i32::MAX as u32), "{name}");
                    }
                }
                Corruption::DanglingPointers => {
                    let list = l.ustruct.child_properties.unwrap_or(l.ustruct.children);
                    for (object, offset) in [
                        (actor, list),
                        (pawn, l.ustruct.super_struct),
                        (addr(&dirty, "/Script/Engine.Level"), l.uobject.outer),
                    ] {
                        let target = d.u64(object + u64::from(offset)).unwrap();
                        assert_eq!(target, dirty.anchors.unmapped, "{name}");
                        assert!(dirty.memory.read_u8(target).is_err());
                    }
                }
                Corruption::UnmappedNamesBlock => {
                    let slot = *dirty.anchors.data.names.block_slots.last().unwrap();
                    let target = d.u64(slot).unwrap();
                    assert_eq!(target, dirty.anchors.unmapped, "{name}");
                    assert!(dirty.memory.read_u8(target).is_err());
                    assert_ne!(c.u64(slot), Some(target));
                }
                Corruption::NameIndexOutOfRange => {
                    assert!(c.name_of(actor).is_some());
                    assert_eq!(d.name_of(actor), None, "{name}");
                    assert_eq!(d.name_of(pawn), None, "{name}");
                }
                Corruption::NonUtf8Names => {
                    let special = |fx: &Fixture, suffix: &str| {
                        fx.objects
                            .iter()
                            .find(|o| o.path.ends_with(suffix))
                            .unwrap()
                            .address
                    };
                    for suffix in [".CafeSpawn", ".Gr\u{fc}nerEingang"] {
                        assert!(
                            c.name_of(special(&clean, suffix)).is_some(),
                            "{name} clean {suffix}"
                        );
                        assert_eq!(
                            d.name_of(special(&dirty, suffix)),
                            None,
                            "{name} {suffix}: not valid text"
                        );
                    }
                }
                Corruption::TruncatedChunkTable => {
                    let base = dirty.anchors.data.object_array;
                    match (l.objects.max_chunks, l.objects.num_chunks) {
                        (Some(max_chunks), Some(num_chunks)) => {
                            let max = d.u32(base + u64::from(max_chunks)).unwrap();
                            let num = d.u32(base + u64::from(num_chunks)).unwrap();
                            assert_eq!(num, max + 1, "{name}");
                            let table = d.u64(base + u64::from(l.objects.objects)).unwrap();
                            assert!(
                                dirty.memory.read_u64(table + u64::from(max) * 8).is_err(),
                                "{name}"
                            );
                            assert!(
                                dirty
                                    .memory
                                    .read_u64(table + u64::from(max - 1) * 8)
                                    .is_ok(),
                                "{name}"
                            );
                        }
                        _ => {
                            assert!(d.num_elements() > d.max_elements(), "{name}");
                        }
                    }
                }
                Corruption::ZeroedObjectHeaders => {
                    let header = dirty
                        .memory
                        .read_vec::<u8>(pawn, l.uobject.size as usize)
                        .unwrap();
                    assert!(header.iter().all(|&b| b == 0), "{name}");
                    assert!(
                        wrong_index_slots(&d) > 20,
                        "{name}: zeroed headers store index 0"
                    );
                }
                Corruption::WrongElementSize => {
                    let guid = addr(&dirty, "/Script/CoreUObject.Guid");
                    let b = d
                        .properties(guid)
                        .into_iter()
                        .find(|(_, n)| n == "B")
                        .unwrap()
                        .0;
                    assert_eq!(
                        d.u32(b + u64::from(l.properties.element_size)),
                        Some(0x100),
                        "{name}"
                    );
                    let owner = dirty
                        .truth
                        .structs
                        .iter()
                        .find(|s| s.name == "Guid")
                        .unwrap()
                        .size;
                    assert!(0x100 > owner);
                }
                Corruption::OverlappingField => {
                    let guid = addr(&dirty, "/Script/CoreUObject.Guid");
                    let props = d.properties(guid);
                    let offset = |n: &str| {
                        let a = props.iter().find(|(_, name)| name == n).unwrap().0;
                        d.u32(a + u64::from(l.properties.offset_internal)).unwrap()
                    };
                    assert_eq!(offset("D"), 4, "{name}");
                    assert_eq!(offset("B"), 4, "{name}: D now overlaps B");
                }
                Corruption::ObjectFreedAndReused => {
                    assert_eq!(wrong_index_slots(&c), 0);
                    assert_eq!(wrong_index_slots(&d), 1, "{name}");
                }
            }
        }
    }
}

#[test]
fn latin_1_and_invalid_utf_16_are_exactly_what_the_corruption_writes() {
    let profile = Profile::ue4_25_case_preserving_measured();
    let fx = FixtureBuilder::new(profile)
        .seed(1)
        .filler_objects(50)
        .corrupt(Corruption::NonUtf8Names)
        .build();
    let anchors = &fx.anchors.data.names;
    let mut found_latin1 = false;
    let mut found_surrogate = false;
    for &entry in &anchors.entries {
        let header = fx.memory.read_u16(entry + 4).unwrap();
        let len = (header >> 1) as usize;
        let chars = entry + 6;
        if header & 1 == 0 {
            let bytes = fx.memory.read_vec::<u8>(chars, len).unwrap();
            if String::from_utf8(bytes.clone()).is_err() {
                found_latin1 = true;
                let decoded: String = bytes.iter().map(|&b| char::from(b)).collect();
                assert_eq!(decoded, "Caf\u{e9}Spawn");
            }
        } else {
            let units = fx.memory.read_vec::<u16>(chars, len).unwrap();
            if String::from_utf16(&units).is_err() {
                found_surrogate = true;
                assert_eq!(units[2], 0xD800);
            }
        }
    }
    assert!(found_latin1 && found_surrogate);
}

#[test]
fn member_chains_follow_the_requested_order() {
    for profile in [
        Profile::ue4_21_measured(),
        Profile::ue4_25_case_preserving_measured(),
    ] {
        for order in [
            ChainOrder::Declaration,
            ChainOrder::Reversed,
            ChainOrder::FunctionsFirst,
        ] {
            let fx = FixtureBuilder::new(profile.clone())
                .seed(2)
                .filler_objects(100)
                .chain_order(order)
                .build();
            let lr = Lr::new(&fx);
            // Reading still works whatever the order: parameters are recovered by offset.
            assert!(lr.check_reflection() > 80, "{} {order:?}", profile.name);
            let guid = fx.object("/Script/CoreUObject.Guid").unwrap().address;
            let names: Vec<String> = lr.properties(guid).into_iter().map(|(_, n)| n).collect();
            match order {
                ChainOrder::Reversed => assert_eq!(names, ["D", "C", "B", "A"]),
                _ => assert_eq!(names, ["A", "B", "C", "D"]),
            }
            // In the legacy list properties and functions share Children.
            if !profile.is_modern() {
                let actor = fx.object("/Script/Engine.Actor").unwrap().address;
                let head = lr
                    .u64(actor + u64::from(fx.layout.ustruct.children))
                    .unwrap();
                let first_is_function = lr.class_name_of(head).as_deref() == Some("Function");
                match order {
                    ChainOrder::FunctionsFirst => assert!(first_is_function),
                    ChainOrder::Declaration => assert!(!first_is_function),
                    ChainOrder::Reversed => {
                        assert!(first_is_function, "the last function comes first")
                    }
                }
            }
        }
    }
}

#[test]
fn shuffled_profiles_are_valid_distinct_and_readable() {
    let mut seen = BTreeSet::new();
    for seed in 0..64u64 {
        let profile = Profile::shuffled(seed);
        profile
            .validate()
            .unwrap_or_else(|e| panic!("seed {seed}: {e}"));
        assert_eq!(profile, Profile::shuffled(seed), "same seed, same profile");
        assert!(
            seen.insert(format!("{:?}{:?}", profile.layout, profile.shape)),
            "seed {seed} repeats a layout"
        );
        if seed < 56 {
            let fx = FixtureBuilder::new(profile)
                .seed(seed)
                .filler_objects(120)
                .build();
            assert!(Lr::new(&fx).check_reflection() > 80, "seed {seed}");
        }
    }
    assert_eq!(seen.len(), 64);
    // Name before Class appears, and FName shapes vary.
    let profiles: Vec<Profile> = (0..64).map(Profile::shuffled).collect();
    assert!(
        profiles
            .iter()
            .any(|p| p.layout.uobject.name < p.layout.uobject.class)
    );
    let sizes: BTreeSet<u32> = profiles.iter().map(|p| p.layout.fname.size).collect();
    assert!(sizes.len() >= 3, "FName sizes: {sizes:?}");
    let strides: BTreeSet<u32> = profiles
        .iter()
        .filter_map(|p| match &p.layout.names {
            NameTableLayout::Pool(pool) => Some(pool.stride),
            NameTableLayout::Array(_) => None,
        })
        .collect();
    assert!(strides.len() >= 2);
    let item_sizes: BTreeSet<u32> = profiles
        .iter()
        .map(|p| p.layout.objects.item_size)
        .collect();
    assert!(item_sizes.len() >= 2);
    let array_orders: BTreeSet<(u32, u32, u32)> = profiles
        .iter()
        .map(|p| {
            (
                p.layout.objects.objects,
                p.layout.objects.max_elements,
                p.layout.objects.num_elements,
            )
        })
        .collect();
    assert!(array_orders.len() >= 6);
}

#[test]
fn the_named_measured_profiles_equal_the_derivation() {
    let v = |major, minor, patch| EngineVersion {
        major,
        minor,
        patch,
    };
    assert_eq!(
        Profile::ue4_21_measured(),
        Profile::stock(v(4, 21, 1)).unwrap()
    );
    let cp = BuildOptions {
        case_preserving_names: true,
        pool_entry: None,
    };
    assert_eq!(
        Profile::ue4_25_case_preserving_measured(),
        Profile::with_options(v(4, 25, 1), cp).unwrap()
    );
    assert_eq!(
        Profile::by_name("ue4.25-cp").unwrap().layout,
        Profile::ue4_25_case_preserving_measured().layout
    );
    assert!(Profile::by_name("nonsense").is_none());
}

#[test]
fn mangled_display_names_differ_from_the_comparison_name_only_in_case() {
    for profile in [
        Profile::ue4_22_case_preserving_measured(),
        Profile::ue4_25_case_preserving_measured(),
        Profile::ue5_case_preserving_assumed(),
    ] {
        let name = profile.name.clone();
        let fx = FixtureBuilder::new(profile)
            .seed(5)
            .filler_objects(200)
            .mangled_display_names(7)
            .build();
        let lr = Lr::new(&fx);
        let display_offset = fx.layout.fname.display_index.unwrap();
        let mut mangled = 0;
        for record in &fx.objects {
            let at = record.address + u64::from(fx.layout.uobject.name);
            let comparison = lr
                .u32(at + u64::from(fx.layout.fname.comparison_index))
                .unwrap();
            let display = lr.u32(at + u64::from(display_offset)).unwrap();
            if comparison == display {
                continue;
            }
            mangled += 1;
            let comparison_text = lr.name_at(comparison).unwrap();
            let display_text = lr.name_at(display).unwrap();
            assert_ne!(comparison_text, display_text, "{name}");
            assert_eq!(comparison_text, display_text.to_ascii_lowercase(), "{name}");
            // The truth keeps the display text.
            assert!(record.path.ends_with(&display_text), "{name}");
        }
        assert_eq!(mangled, 7, "{name}");
        assert_eq!(
            fx.truth,
            FixtureBuilder::new(fx.profile.clone())
                .seed(5)
                .filler_objects(200)
                .build()
                .truth
        );
    }
    // Without a display index there is nothing to mangle.
    let fx = FixtureBuilder::new(Profile::ue4_21_measured())
        .filler_objects(50)
        .mangled_display_names(7)
        .build();
    let plain = FixtureBuilder::new(Profile::ue4_21_measured())
        .filler_objects(50)
        .build();
    assert!(parts_equal(&fx, &plain));
}
