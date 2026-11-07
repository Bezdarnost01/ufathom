//! Version detection: the resource forms, build strings, markers, how they
//! combine, and the testkit images across every stock profile.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "test code may panic on failure; clippy.toml only exempts #[test] functions, not helpers"
)]

mod common;

use common::*;
use ufathom_model::EngineVersion;
use ufathom_scan::limits::{MAX_BUILD_CANDIDATES, SCAN_CHUNK_BYTES};
use ufathom_scan::version::StringEncoding;
use ufathom_scan::{PeImage, detect_version};
use ufathom_testkit::{BuildOptions, FixtureBuilder, Profile, SECTION_NAMES};

fn claimed(strings: &[(&str, &str)]) -> Option<String> {
    facts_of(&pe_with_version(strings).to_bytes())
        .engine
        .claimed
}

fn rdata_pe(rdata: Vec<u8>) -> Vec<u8> {
    // `.data` goes after `.rdata`, however large that is.
    let data_rva = (RDATA_RVA + rdata.len() as u32).next_multiple_of(0x1000);
    PeSpec::new()
        .section(".text", TEXT_RVA, vec![0xC3; 0x100], CODE)
        .section(".rdata", RDATA_RVA, rdata, RDATA)
        .section(".data", data_rva, vec![0; 0x100], DATA)
        .to_bytes()
}

fn v(major: u16, minor: u16) -> EngineVersion {
    EngineVersion {
        major,
        minor,
        patch: 0,
    }
}

// ---- the version resource ------------------------------------------------

#[test]
fn product_version_forms_give_the_engine_version() {
    assert_eq!(
        claimed(&[("ProductVersion", "4.21.1")]).as_deref(),
        Some("4.21.1")
    );
    assert_eq!(
        claimed(&[("ProductVersion", "4.13.2-0+UE4")]).as_deref(),
        Some("4.13.2")
    );
    assert_eq!(
        claimed(&[("ProductVersion", "++UE5+Release-5.4-CL-1234567")]).as_deref(),
        Some("5.4")
    );
    assert_eq!(
        claimed(&[("FileVersion", "4.22.3.0")]).as_deref(),
        Some("4.22.3")
    );
    assert_eq!(claimed(&[("ProductVersion", "1.0.0.0")]), None);
    assert_eq!(claimed(&[]), None);
}

#[test]
fn product_version_wins_over_file_version_and_disagreement_is_reported() {
    let facts = facts_of(
        &pe_with_version(&[("FileVersion", "4.22.0"), ("ProductVersion", "4.21.1")]).to_bytes(),
    );
    assert_eq!(facts.engine.claimed.as_deref(), Some("4.21.1"));
    let text = facts.engine.evidence.join("\n");
    assert!(text.contains("disagreement"), "{text}");
    assert!(text.contains("4.22.0"), "{text}");

    // Agreement on the branch is not a disagreement.
    let facts = facts_of(
        &pe_with_version(&[("FileVersion", "4.21.0"), ("ProductVersion", "4.21.1")]).to_bytes(),
    );
    assert!(!facts.engine.evidence.join("\n").contains("disagreement"));
}

#[test]
fn the_fixed_file_info_is_a_fallback_and_a_cross_check() {
    let blob = version_blob(
        Some(([4, 25, 1, 0], [4, 25, 1, 0])),
        &[("ProductVersion", "1.0.0.0")],
    );
    let rsrc = rsrc_with_versions(RSRC_RVA, &[(0x409, blob)]);
    let bytes = standard_pe(vec![0; 16], vec![0; 16], Some(rsrc)).to_bytes();
    let facts = facts_of(&bytes);
    assert_eq!(facts.engine.claimed.as_deref(), Some("4.25.1"));
    let fixed = facts.resource.as_ref().unwrap().fixed.unwrap();
    assert_eq!(fixed.product_version, [4, 25, 1, 0]);
    let text = facts.engine.evidence.join("\n");
    assert!(text.contains("does not state an engine version"), "{text}");
}

#[test]
fn a_string_in_rdata_that_looks_like_a_version_key_is_not_read() {
    // Unrelated text in `.rdata`, with no resource directory at all.
    let mut rdata = utf16z("ProductVersion");
    rdata.extend(utf16z("9.9.9"));
    rdata.extend(b"ProductVersion\x004.21.1\0");
    let facts = facts_of(&rdata_pe(rdata));
    assert_eq!(facts.engine.claimed, None);
    assert!(facts.resource.is_none());
}

#[test]
fn language_preference_is_english_then_neutral_then_file_order() {
    let blob = |text: &str| version_blob(None, &[("ProductVersion", text)]);
    let pick = |languages: &[(u32, Vec<u8>)]| {
        let rsrc = rsrc_with_versions(RSRC_RVA, languages);
        facts_of(&standard_pe(vec![0; 16], vec![0; 16], Some(rsrc)).to_bytes())
            .engine
            .claimed
    };
    assert_eq!(
        pick(&[(0x407, blob("4.20")), (0x409, blob("4.21"))]).as_deref(),
        Some("4.21")
    );
    assert_eq!(
        pick(&[(0x407, blob("4.20")), (0, blob("4.22"))]).as_deref(),
        Some("4.22")
    );
    assert_eq!(
        pick(&[(0x407, blob("4.20")), (0x40C, blob("4.23"))]).as_deref(),
        Some("4.20")
    );
    // A damaged preferred block falls through to the next.
    let mut broken = blob("4.21");
    broken[0] = 0xFF;
    broken[1] = 0xFF;
    assert_eq!(
        pick(&[(0x409, broken), (0x407, blob("4.20"))]).as_deref(),
        Some("4.20")
    );
}

#[test]
fn a_standalone_image_with_a_resource_but_no_version_type_has_none() {
    // Root directory with only a non-version type (RT_ICON = 3), pointing at
    // an empty directory.
    let mut rsrc = directory(&[(3, SUBDIR | 0x18)]);
    rsrc.extend(directory(&[]));
    let bytes = standard_pe(vec![0; 16], vec![0; 16], Some(rsrc)).to_bytes();
    let facts = facts_of(&bytes);
    assert!(facts.resource.is_none());
    assert!(facts.notes.is_empty(), "{:?}", facts.notes);
    assert!(
        facts
            .engine
            .evidence
            .iter()
            .any(|l| l.contains("no version resource"))
    );
}

// ---- build strings -----------------------------------------------------------

#[test]
fn build_strings_are_found_in_both_encodings_and_the_best_is_chosen() {
    let rdata = rdata_with(
        0x400,
        &[
            (0x10, b"++Game+Live\0".to_vec()),
            (0x40, b"++Game+Live-CL-111\0".to_vec()),
            (0x80, utf16z("++Game+Live-CL-222")),
            (0xC0, utf16z("++UE4+Release-4.25")),
        ],
    );
    let facts = facts_of(&rdata_pe(rdata));
    assert_eq!(facts.build_strings.len(), 4);
    // A changelist beats none; UTF-16 beats ASCII.
    assert_eq!(facts.engine.build.as_deref(), Some("++Game+Live-CL-222"));
    let best = &facts.build_strings[0];
    assert_eq!(best.encoding, StringEncoding::Utf16);
    assert_eq!(best.changelist, Some(222));
    assert_eq!(best.section, ".rdata");
    assert_eq!(best.rva, RDATA_RVA + 0x80);
    assert_eq!(facts.build_strings[1].text, "++Game+Live-CL-111");
    let text = facts.engine.evidence.join("\n");
    assert!(text.contains("other build string"), "{text}");
    // The engine's own branch string is a claim of its own.
    assert_eq!(facts.engine.claimed.as_deref(), Some("4.25"));
}

#[test]
fn build_strings_inside_other_text_or_too_long_are_not_candidates() {
    let long = format!("++Game+{}", "x".repeat(300));
    let rdata = rdata_with(
        0x800,
        &[
            (0x10, b"C++\0".to_vec()),
            (0x20, b"operator++ \0".to_vec()),
            (0x40, b"4.21.1-12345+++Game+Live\0".to_vec()),
            (0x80, b"++Game\0".to_vec()),
            (0x90, utf16z("x++Game+Live")),
            (0x100, long.into_bytes()),
        ],
    );
    let facts = facts_of(&rdata_pe(rdata));
    assert!(facts.build_strings.is_empty(), "{:?}", facts.build_strings);
    assert_eq!(facts.engine.build, None);
}

#[test]
fn the_best_build_does_not_depend_on_where_the_strings_are() {
    let items = [
        b"++B+Main\0".to_vec(),
        b"++A+Main-CL-5\0".to_vec(),
        utf16z("++C+Main-CL-5"),
        utf16z("++A+Main-CL-5"),
    ];
    let mut order: Vec<usize> = (0..items.len()).collect();
    let mut orders = std::collections::BTreeSet::new();
    for _ in 0..items.len() {
        order.rotate_left(1);
        let placed: Vec<(usize, Vec<u8>)> = order
            .iter()
            .enumerate()
            .map(|(slot, i)| (slot * 0x40, items[*i].clone()))
            .collect();
        let facts = facts_of(&rdata_pe(rdata_with(0x200, &placed)));
        let texts: Vec<_> = facts
            .build_strings
            .iter()
            .map(|b| (b.text.clone(), b.encoding))
            .collect();
        orders.insert(texts);
    }
    assert_eq!(orders.len(), 1, "{orders:?}");
    let ranked = orders.into_iter().next().unwrap();
    assert_eq!(ranked[0].0, "++A+Main-CL-5");
    assert_eq!(ranked[0].1, StringEncoding::Utf16);
}

#[test]
fn many_distinct_build_strings_are_bounded() {
    let mut rdata = Vec::new();
    for i in 0..(MAX_BUILD_CANDIDATES + 20) {
        rdata.extend(format!("++Game+Branch{i:03}\0").into_bytes());
    }
    let facts = facts_of(&rdata_pe(rdata));
    assert_eq!(facts.build_strings.len(), MAX_BUILD_CANDIDATES);
    assert!(
        facts.notes.iter().any(|n| n.contains("ignored")),
        "{:?}",
        facts.notes
    );
}

#[test]
fn strings_straddling_a_chunk_boundary_are_found_once() {
    let chunk = SCAN_CHUNK_BYTES as usize;
    let build = b"++Game+Live-CL-777 ".to_vec();
    let wide_marker = utf16z("FieldPathProperty");
    let ascii_marker = b" FieldPathProperty ".to_vec();
    for back in [0usize, 1, 2, 3, 7, 18, 19, 20, 33, 34, 35] {
        let at = chunk - back;
        let facts = facts_of(&rdata_pe(rdata_with(
            chunk + 0x1000,
            &[(at, build.clone())],
        )));
        assert_eq!(facts.build_strings.len(), 1, "build, back {back}");
        assert_eq!(
            facts.build_strings[0].text, "++Game+Live-CL-777",
            "back {back}"
        );

        let facts = facts_of(&rdata_pe(rdata_with(
            chunk + 0x1000,
            &[(at & !1, wide_marker.clone())],
        )));
        assert_eq!(facts.markers.len(), 1, "wide marker, back {back}");
        assert_eq!(facts.engine.min_version, Some(v(4, 25)), "back {back}");

        let facts = facts_of(&rdata_pe(rdata_with(
            chunk + 0x1000,
            &[(at, ascii_marker.clone())],
        )));
        assert_eq!(facts.markers.len(), 1, "ascii marker, back {back}");
    }
}

#[test]
fn only_read_only_sections_are_searched() {
    let facts = facts_of(
        &standard_pe(
            vec![0; 0x100],
            rdata_with(
                0x100,
                &[(0, b"++Game+Live-CL-1\0FieldPathProperty\0".to_vec())],
            ),
            None,
        )
        .to_bytes(),
    );
    assert!(facts.build_strings.is_empty());
    assert!(facts.markers.is_empty());
    assert!(facts.notes.is_empty());
}

// ---- markers ---------------------------------------------------------------------

#[test]
fn a_marker_gives_a_lower_bound_in_either_encoding() {
    for (what, bytes, encoding) in [
        (
            "ascii",
            b"\0FieldPathProperty\0".to_vec(),
            StringEncoding::Ascii,
        ),
        (
            "utf16",
            [vec![0, 0], utf16z("FieldPathProperty")].concat(),
            StringEncoding::Utf16,
        ),
        (
            "inside a path",
            b"/Script/CoreUObject.FieldPathProperty\0".to_vec(),
            StringEncoding::Ascii,
        ),
    ] {
        let facts = facts_of(&rdata_pe(rdata_with(0x100, &[(0x10, bytes)])));
        assert_eq!(facts.engine.min_version, Some(v(4, 25)), "{what}");
        assert_eq!(facts.engine.max_version, None, "{what}");
        assert_eq!(facts.markers.len(), 1, "{what}");
        assert_eq!(facts.markers[0].encoding, encoding, "{what}");
        assert_eq!(facts.markers[0].section, ".rdata", "{what}");
        let text = facts.engine.evidence.join("\n");
        assert!(text.contains("at least 4.25"), "{what}: {text}");
    }
}

#[test]
fn a_marker_inside_a_longer_word_is_not_a_marker() {
    for text in [
        b"UFieldPathProperty\0".to_vec(),
        b"FieldPathPropertyX\0".to_vec(),
        b"MyFieldPathProperty_Foo\0".to_vec(),
        b"FieldPath\0".to_vec(),
        utf16z("FFieldPathProperty"),
    ] {
        let facts = facts_of(&rdata_pe(rdata_with(0x100, &[(0x10, text.clone())])));
        assert!(facts.markers.is_empty(), "{text:?}");
        assert_eq!(facts.engine.min_version, None);
    }
    // Absence proves nothing, so there is no bound at all.
    let facts = facts_of(&rdata_pe(vec![0; 0x100]));
    assert_eq!(facts.engine.min_version, None);
    assert_eq!(facts.engine.max_version, None);
}

#[test]
fn a_claim_below_the_marker_bound_is_kept_with_the_contradiction() {
    let blob = version_blob(None, &[("ProductVersion", "4.21.1")]);
    let rsrc = rsrc_with_versions(RSRC_RVA, &[(0x409, blob)]);
    let rdata = rdata_with(0x100, &[(0x10, b"\0FieldPathProperty\0".to_vec())]);
    let facts = facts_of(&standard_pe(rdata, vec![0; 16], Some(rsrc)).to_bytes());
    assert_eq!(facts.engine.claimed.as_deref(), Some("4.21.1"));
    assert_eq!(facts.engine.min_version, Some(v(4, 25)));
    let text = facts.engine.evidence.join("\n");
    assert!(text.contains("contradiction"), "{text}");

    // A consistent pair has no contradiction.
    let blob = version_blob(None, &[("ProductVersion", "4.26.2")]);
    let rsrc = rsrc_with_versions(RSRC_RVA, &[(0x409, blob)]);
    let rdata = rdata_with(0x100, &[(0x10, b"\0FieldPathProperty\0".to_vec())]);
    let facts = facts_of(&standard_pe(rdata, vec![0; 16], Some(rsrc)).to_bytes());
    assert!(!facts.engine.evidence.join("\n").contains("contradiction"));
}

#[test]
fn nothing_found_is_a_result_not_an_error() {
    let facts = facts_of(&rdata_pe(vec![0; 0x100]));
    assert_eq!(facts.engine.claimed, None);
    assert_eq!(facts.engine.build, None);
    assert_eq!(facts.engine.min_version, None);
    assert!(
        !facts.engine.evidence.is_empty(),
        "evidence says what was tried"
    );
}

#[test]
fn a_32_bit_image_reads_the_same() {
    let mut spec = pe_with_version(&[("ProductVersion", "4.21.1")]);
    spec.pe32 = true;
    spec.image_base = 0x40_0000;
    assert_eq!(
        facts_of(&spec.to_bytes()).engine.claimed.as_deref(),
        Some("4.21.1")
    );
}

// ---- the testkit images ------------------------------------------------------------------

fn stock_profiles() -> Vec<Profile> {
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
    out.push(Profile::ue4_21_measured());
    out.push(Profile::ue4_22_case_preserving_measured());
    out.push(Profile::ue4_25_case_preserving_measured());
    out.push(Profile::ue5_case_preserving_assumed());
    out
}

#[test]
fn every_stock_profile_reports_its_version_and_build() {
    let profiles = stock_profiles();
    assert!(profiles.len() >= 40);
    for profile in profiles {
        let name = profile.name.clone();
        let fx = FixtureBuilder::new(profile.clone())
            .seed(3)
            .filler_objects(300)
            .build();
        let base = fx.addresses.module_base;
        let image = PeImage::parse_from_memory(&fx.memory, base).unwrap();

        let names: Vec<_> = image.sections.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, SECTION_NAMES, "{name}");
        assert!(image.is_64_bit, "{name}");
        assert_eq!(image.image_base, base, "{name}");

        let facts = detect_version(&fx.memory, &image);
        assert_eq!(
            facts.engine.claimed, fx.engine.claimed,
            "{name}: claimed version"
        );
        assert_eq!(
            facts.engine.claimed.as_deref(),
            Some(profile.identity.product_version.as_str()),
            "{name}"
        );
        assert_eq!(
            facts.engine.build.as_deref(),
            Some(fx.engine.build.as_str()),
            "{name}: build string"
        );
        let best = &facts.build_strings[0];
        assert_eq!(
            best.changelist,
            Some(u64::from(fx.engine.changelist)),
            "{name}"
        );
        assert_eq!(best.encoding, StringEncoding::Utf16, "{name}");
        assert_eq!(facts.engine.max_version, None, "{name}");
        assert!(facts.notes.is_empty(), "{name}: {:?}", facts.notes);
        assert!(
            facts
                .resource
                .as_ref()
                .is_some_and(|r| r.problems.is_empty()),
            "{name}"
        );
        // The resource has the fixed numbers as well, and they agree.
        let fixed = facts.resource.as_ref().unwrap().fixed.unwrap();
        let (major, minor, patch) = profile.identity.version_parts();
        assert_eq!(fixed.product_version, [major, minor, patch, 0], "{name}");
    }
}

#[test]
fn testkit_images_give_identical_facts_when_built_twice() {
    for profile in [
        Profile::ue4_21_measured(),
        Profile::ue5_case_preserving_assumed(),
    ] {
        let facts = |seed| {
            let fx = FixtureBuilder::new(profile.clone())
                .seed(seed)
                .filler_objects(100)
                .build();
            let image = PeImage::parse_from_memory(&fx.memory, fx.addresses.module_base).unwrap();
            detect_version(&fx.memory, &image)
        };
        assert_eq!(facts(1), facts(1));
        // A different seed moves the module and rearranges `.rdata`, yet the
        // facts that depend on addresses are the only ones that may differ.
        assert_eq!(facts(1).engine.claimed, facts(2).engine.claimed);
        assert_eq!(facts(1).engine.build, facts(2).engine.build);
    }
}

#[test]
fn detection_is_deterministic() {
    let blob = version_blob(
        Some(([4, 25, 1, 0], [4, 25, 1, 0])),
        &[
            ("FileVersion", "4.25.1"),
            ("ProductVersion", "++UE4+Release-4.25-CL-5"),
        ],
    );
    let rsrc = rsrc_with_versions(RSRC_RVA, &[(0x409, blob)]);
    let rdata = rdata_with(
        0x400,
        &[
            (0x10, utf16z("++Game+Live-CL-99")),
            (0x80, b"++Game+Live-CL-98 ".to_vec()),
            (0xA0, b" FieldPathProperty ".to_vec()),
            (0xE0, utf16z("FieldPathProperty")),
            (0x120, utf16z("++UE4+Release-4.26")),
        ],
    );
    let bytes = standard_pe(rdata, vec![0; 0x100], Some(rsrc)).to_bytes();
    let first = facts_of(&bytes);
    for _ in 0..5 {
        assert_eq!(facts_of(&bytes), first);
    }
    // Reading the same snapshot repeatedly gives the same answer too.
    let (snapshot, module) = load(&bytes).unwrap();
    let image = PeImage::parse_from_memory(&snapshot, module.base).unwrap();
    assert_eq!(detect_version(&snapshot, &image), first);
    assert_eq!(detect_version(&snapshot, &image), first);
    // Both markers are reported, in a fixed order.
    assert_eq!(first.markers.len(), 2);
    assert_eq!(first.markers[0].encoding, StringEncoding::Ascii);
    assert_eq!(first.markers[1].encoding, StringEncoding::Utf16);
}
