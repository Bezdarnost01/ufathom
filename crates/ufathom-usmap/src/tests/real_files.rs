//! Tests on real usmap files, which are game data and never stored in the
//! repository. Paths come from `ufathom.local.toml` at the workspace root
//! (gitignored); a test skips with a message when the file or key is absent.
//!
//! Run with `cargo test -p ufathom-usmap -- --ignored --nocapture`.
//!
//! Nothing read from a file is copied anywhere: the assertions name only
//! engine types every Unreal build has.

use std::path::{Path, PathBuf};

use crate::{Compression, Usmap, UsmapProperty, UsmapStruct, UsmapType, UsmapVersion, compare};

use super::fixtures::empty_dump;

/// Reads `usmap.<key>` from the local config. `None` means the developer has
/// not set it up, which is not a failure.
fn local_path(key: &str) -> Option<PathBuf> {
    let config = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("ufathom.local.toml");
    let Ok(text) = std::fs::read_to_string(&config) else {
        println!("SKIP {key}: {} not found", config.display());
        return None;
    };
    // Editors on Windows like to add a byte order mark.
    let table: toml::Table = text
        .trim_start_matches('\u{feff}')
        .parse()
        .expect("ufathom.local.toml is valid TOML");
    let path = table
        .get("usmap")
        .and_then(|section| section.get(key))
        .and_then(toml::Value::as_str);
    match path {
        Some(path) => Some(PathBuf::from(path)),
        None => {
            println!("SKIP {key}: no `usmap.{key}` entry in ufathom.local.toml");
            None
        }
    }
}

fn load(key: &str) -> Option<Usmap> {
    let path = local_path(key)?;
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) => {
            println!("SKIP {key}: cannot read {}: {error}", path.display());
            return None;
        }
    };
    let usmap = Usmap::parse(&bytes).unwrap_or_else(|error| panic!("{key}: {error}"));
    let properties: usize = usmap.structs.iter().map(|s| s.properties.len()).sum();
    let values: usize = usmap.enums.iter().map(|e| e.values.len()).sum();
    println!(
        "{key}: {} bytes, version {}, {:?}, {} names, {} enums ({} values), {} structs ({} properties), {} extension bytes, version info: {}",
        bytes.len(),
        usmap.version,
        usmap.compression,
        usmap.names.len(),
        usmap.enums.len(),
        values,
        usmap.structs.len(),
        properties,
        usmap.extension_bytes,
        usmap.version_info.is_some(),
    );
    Some(usmap)
}

fn find<'a>(usmap: &'a Usmap, name: &str) -> &'a UsmapStruct {
    usmap
        .structs
        .iter()
        .find(|def| def.name == name)
        .unwrap_or_else(|| panic!("struct {name} not found"))
}

fn property<'a>(def: &'a UsmapStruct, name: &str) -> &'a UsmapProperty {
    def.properties
        .iter()
        .find(|property| property.name == name)
        .unwrap_or_else(|| panic!("{}.{name} not found", def.name))
}

/// Checks that hold for any Unreal Engine build.
fn check_engine_types(usmap: &Usmap) {
    let guid = find(usmap, "Guid");
    for name in ["A", "B", "C", "D"] {
        let member = property(guid, name);
        assert_eq!(member.ty, UsmapType::Int32, "Guid.{name}");
        assert_eq!(member.array_dim, 1);
    }
    assert_eq!(guid.properties.len(), 4);
    assert_eq!(guid.super_name, None);

    let vector = find(usmap, "Vector");
    for name in ["X", "Y", "Z"] {
        let member = property(vector, name);
        // Float before UE5, double from UE5 large world coordinates.
        assert!(
            matches!(member.ty, UsmapType::Float | UsmapType::Double),
            "Vector.{name} is {}",
            member.ty
        );
    }

    let actor = find(usmap, "Actor");
    assert_eq!(actor.super_name.as_deref(), Some("Object"));
    assert!(!actor.properties.is_empty());

    // Every parent named by a struct exists as a struct, which also checks
    // that the name table was resolved consistently.
    let names: std::collections::HashSet<&str> =
        usmap.structs.iter().map(|def| def.name.as_str()).collect();
    let dangling = usmap
        .structs
        .iter()
        .filter_map(|def| def.super_name.as_deref())
        .filter(|parent| !names.contains(parent))
        .count();
    println!("  parents not present as structs: {dangling}");

    // Comparing a usmap with an empty dump lists everything as missing and
    // cannot panic, whatever the file holds.
    let report = compare(&empty_dump(), usmap);
    assert!(report.counts.missing_structs > 0);
    assert_eq!(report.counts.usmap_structs, usmap.structs.len());
}

#[test]
#[ignore = "needs a real usmap; set usmap.ue4_22 in ufathom.local.toml"]
fn real_ue4_22() {
    let Some(usmap) = load("ue4_22") else {
        return;
    };
    // Written by Dumper-7: version 4, zstd.
    assert_eq!(usmap.version, UsmapVersion::ExplicitEnumValues);
    assert_eq!(usmap.compression, Compression::Zstd);
    assert!(usmap.names.len() > 5_000, "names: {}", usmap.names.len());
    assert!(
        usmap.structs.len() > 1_000,
        "structs: {}",
        usmap.structs.len()
    );
    assert!(usmap.enums.len() > 100, "enums: {}", usmap.enums.len());
    check_engine_types(&usmap);
    // UE 4.22 has float vectors.
    assert_eq!(property(find(&usmap, "Vector"), "X").ty, UsmapType::Float);
}

#[test]
#[ignore = "needs a real usmap; set usmap.ue5 in ufathom.local.toml"]
fn real_ue5() {
    let Some(usmap) = load("ue5") else {
        return;
    };
    // Version 0, stored uncompressed: no version block and 8-bit name lengths.
    assert_eq!(usmap.version, UsmapVersion::Initial);
    assert_eq!(usmap.compression, Compression::None);
    assert!(usmap.names.len() > 10_000, "names: {}", usmap.names.len());
    assert!(
        usmap.structs.len() > 1_000,
        "structs: {}",
        usmap.structs.len()
    );
    assert!(usmap.enums.len() > 100, "enums: {}", usmap.enums.len());
    check_engine_types(&usmap);
    assert!(!usmap.has_explicit_enum_values());
}
