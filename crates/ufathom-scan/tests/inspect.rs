//! `inspect_exe` and `load_pe_file` on real files on disk, built from
//! hand-made images and from the testkit's fake modules.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "test code may panic on failure; clippy.toml only exempts #[test] functions, not helpers"
)]

mod common;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use common::*;
use ufathom_mem::{MemoryReader, MemoryReaderExt, Snapshot};
use ufathom_model::EngineVersion;
use ufathom_scan::{PeImage, ScanError, inspect_exe, load_pe_file};
use ufathom_testkit::{BuildOptions, FixtureBuilder, Profile};

/// A file in the temp directory, removed when dropped.
struct TempFile(PathBuf);

impl TempFile {
    fn new(bytes: &[u8]) -> Self {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let path = std::env::temp_dir().join(format!(
            "ufathom-scan-test-{}-{}.exe",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::SeqCst)
        ));
        std::fs::write(&path, bytes).unwrap();
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Writes a testkit module back out as the PE file it models: headers, then
/// each section's raw data at the offset its header states.
fn fixture_file(memory: &Snapshot, base: u64) -> Vec<u8> {
    let image = PeImage::parse_from_memory(memory, base).unwrap();
    let mut file = memory
        .read_vec::<u8>(base, image.size_of_headers as usize)
        .unwrap();
    for section in &image.sections {
        let data = memory
            .read_vec::<u8>(base + u64::from(section.rva), section.raw_size as usize)
            .unwrap();
        let start = section.raw_offset as usize;
        if file.len() < start {
            file.resize(start, 0);
        }
        file.truncate(start);
        file.extend_from_slice(&data);
    }
    file
}

#[test]
fn inspect_reports_sections_version_and_packing_of_a_file() {
    let mut spec = pe_with_version(&[("ProductVersion", "4.25.1")]);
    spec.entry = TEXT_RVA;
    let file = TempFile::new(&spec.to_bytes());
    let facts = inspect_exe(file.path()).unwrap();
    assert!(facts.module.name.ends_with(".exe"));
    assert_eq!(facts.module.base, IMAGE_BASE);
    assert_eq!(facts.module.size, u64::from(facts.image.size_of_image));
    assert!(facts.module.path.is_some());
    let names: Vec<_> = facts
        .image
        .sections
        .iter()
        .map(|s| s.name.as_str())
        .collect();
    assert_eq!(names, [".text", ".rdata", ".data", ".rsrc"]);
    assert_eq!(facts.version.engine.claimed.as_deref(), Some("4.25.1"));
    assert!(!facts.packing.looks_packed(), "{:?}", facts.packing);
}

#[test]
fn load_pe_file_returns_a_snapshot_that_parses_at_the_module_base() {
    let spec = pe_with_version(&[("ProductVersion", "4.25.1")]);
    let file = TempFile::new(&spec.to_bytes());
    let (snapshot, module) = load_pe_file(file.path()).unwrap();
    assert_eq!(snapshot.modules().unwrap(), vec![module.clone()]);
    let image = PeImage::parse_from_memory(&snapshot, module.base).unwrap();
    assert_eq!(image.sections.len(), 4);
}

#[test]
fn files_that_cannot_be_inspected_are_errors() {
    let missing = std::env::temp_dir().join("ufathom-scan-test-does-not-exist.exe");
    assert!(matches!(inspect_exe(&missing), Err(ScanError::Io { .. })));
    assert!(matches!(
        inspect_exe(std::env::temp_dir()),
        Err(ScanError::Io { .. })
    ));
    let text = TempFile::new(b"this is not an executable file");
    assert!(matches!(
        inspect_exe(text.path()),
        Err(ScanError::NotPe { .. })
    ));
    let empty = TempFile::new(b"");
    assert!(matches!(
        inspect_exe(empty.path()),
        Err(ScanError::NotPe { .. })
    ));
    let bytes = pe_with_version(&[("ProductVersion", "4.25.1")]).to_bytes();
    let truncated = TempFile::new(&bytes[..bytes.len() - 0x300]);
    assert!(matches!(
        inspect_exe(truncated.path()),
        Err(ScanError::TruncatedFile { .. })
    ));
}

#[test]
fn packer_section_names_and_odd_entry_points_are_indicators() {
    let packed = |build: &dyn Fn(PeSpec) -> PeSpec| {
        let spec = build(PeSpec::new().section(".text", 0x1000, vec![0xC3; 16], CODE));
        let file = TempFile::new(&spec.to_bytes());
        inspect_exe(file.path()).unwrap().packing
    };
    let plain = packed(&|spec| spec);
    assert!(!plain.looks_packed(), "{plain:?}");

    let upx = packed(&|spec| spec.section("UPX1", 0x2000, vec![1; 16], CODE));
    assert!(upx.looks_packed());
    assert!(upx.indicators[0].contains("UPX"), "{upx:?}");

    let wx = packed(&|spec| spec.section(".sec", 0x2000, vec![1; 16], 0xE000_0020));
    assert!(
        wx.indicators
            .iter()
            .any(|i| i.contains("writable and executable"))
    );

    let hollow = packed(&|mut spec| {
        spec = spec.section(".stub", 0x2000, Vec::new(), CODE);
        spec.sections[1].virtual_size = Some(0x1000);
        spec
    });
    assert!(
        hollow
            .indicators
            .iter()
            .any(|i| i.contains("no data in the file"))
    );

    let outside = packed(&|mut spec| {
        spec.entry = 0x9000;
        spec
    });
    assert!(
        outside
            .indicators
            .iter()
            .any(|i| i.contains("outside every section"))
    );

    let in_data = packed(&|mut spec| {
        spec = spec.section(".data", 0x2000, vec![1; 16], DATA);
        spec.entry = 0x2000;
        spec
    });
    assert!(
        in_data
            .indicators
            .iter()
            .any(|i| i.contains("non-executable"))
    );
}

#[test]
fn testkit_modules_round_trip_through_a_file() {
    let profiles = [
        Profile::ue4_21_measured(),
        Profile::ue4_22_case_preserving_measured(),
        Profile::ue4_25_case_preserving_measured(),
        Profile::ue5_case_preserving_assumed(),
    ];
    for profile in profiles {
        let fx = FixtureBuilder::new(profile.clone())
            .seed(5)
            .filler_objects(100)
            .build();
        let base = fx.addresses.module_base;
        let file = TempFile::new(&fixture_file(&fx.memory, base));
        let facts = inspect_exe(file.path()).unwrap();
        let name = &profile.name;
        assert_eq!(facts.module.base, base, "{name}");
        assert_eq!(facts.version.engine.claimed, fx.engine.claimed, "{name}");
        assert_eq!(
            facts.version.engine.build.as_deref(),
            Some(fx.engine.build.as_str()),
            "{name}"
        );
        assert!(!facts.packing.looks_packed(), "{name}: {:?}", facts.packing);

        // Every mapped byte of every section equals the testkit's memory.
        let (loaded, module) = load_pe_file(file.path()).unwrap();
        for section in &facts.image.sections {
            let at = module.base + u64::from(section.rva);
            let len = section.mapped_size() as usize;
            assert_eq!(
                loaded.read_vec::<u8>(at, len).unwrap(),
                fx.memory.read_vec::<u8>(at, len).unwrap(),
                "{name} {}",
                section.name
            );
        }
    }
}

#[test]
fn every_stock_profile_loads_from_file_bytes_and_reports_its_version() {
    let versions = (11..=27).map(|m| (4, m)).chain((0..=8).map(|m| (5, m)));
    for (major, minor) in versions {
        let version = EngineVersion {
            major,
            minor,
            patch: 0,
        };
        let options = BuildOptions {
            case_preserving_names: major == 5 || minor >= 22,
            pool_entry: None,
        };
        let profile = Profile::with_options(version, options).unwrap();
        let fx = FixtureBuilder::new(profile.clone())
            .seed(9)
            .filler_objects(50)
            .build();
        let bytes = fixture_file(&fx.memory, fx.addresses.module_base);
        let facts = facts_of(&bytes);
        assert_eq!(
            facts.engine.claimed.as_deref(),
            Some(profile.identity.product_version.as_str()),
            "{}",
            profile.name
        );
        assert_eq!(
            facts.engine.build.as_deref(),
            Some(fx.engine.build.as_str())
        );
    }
}
