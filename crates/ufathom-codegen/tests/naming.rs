//! Names in the generated text: prefixes, keywords, collisions, sanitizing.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "tests may panic"
)]

mod common;

use std::collections::BTreeMap;

use common::dumps::golden_dump;
use common::read_tree;
use ufathom_codegen::{GenOptions, generate};

fn files() -> BTreeMap<String, String> {
    let dir = tempfile::tempdir().unwrap();
    generate(&golden_dump(), &GenOptions::default(), dir.path()).unwrap();
    read_tree(dir.path()).into_iter().collect()
}

#[test]
fn unreal_prefixes_come_from_the_dump() {
    let f = files();
    let engine = &f["src/engine.rs"];
    let core = &f["src/core_u_object.rs"];
    // Actor-derived classes get A, other classes U, structs F, enums E.
    assert!(engine.contains("pub struct AActor {"));
    assert!(engine.contains("pub struct APawn {"));
    assert!(f["src/mods.rs"].contains("pub struct ABP_Thing_C {"));
    assert!(engine.contains("pub struct UEverything {"));
    assert!(core.contains("pub struct UObject {"));
    assert!(core.contains("pub struct UClass {"));
    assert!(core.contains("pub struct FVector {"));
    // An enum name that already starts with E and a capital keeps it.
    assert!(core.contains("pub struct ENetRole(pub u8);"));
    assert!(core.contains("pub struct ESigned(pub i32);"));
    assert!(engine.contains("pub struct EDirection(pub u16);"));
}

#[test]
fn duplicate_type_names_get_deterministic_suffixes() {
    let engine = &files()["src/engine.rs"];
    assert!(engine.contains("pub struct UDup {"));
    assert!(engine.contains("pub struct UDup_1 {"));
    let first = engine.split("pub struct UDup {").nth(1).unwrap();
    // The first by path keeps the plain name and its field.
    assert!(
        first
            .split("pub struct")
            .next()
            .unwrap()
            .contains("pub a: u32")
    );
}

#[test]
fn keywords_collisions_and_odd_characters_are_made_valid() {
    let engine = &files()["src/engine.rs"];
    for expected in [
        "pub r#type: u32",
        "pub self_: u32",
        "pub r#match: u32",
        "pub weird_name: u32",
        "pub u540d_u524d: u32",
        "pub foo: u32",
        "pub foo_1: u32",
        "pub _9lives: u32",
        "pub __: u32",
        "pub crate_: u32",
        "pub self__1: u32",
    ] {
        assert!(engine.contains(expected), "missing `{expected}`");
    }
}

#[test]
fn enumerators_are_unique_constants_and_names_are_optional() {
    let core = &files()["src/core_u_object.rs"];
    assert!(
        core.contains("pub const Type: Self = Self(-1);"),
        "{core}"
    );
    assert!(core.contains("pub const Self_: Self = Self(0);"));
    assert!(core.contains("pub const Zero: Self = Self(1);"));
    assert!(core.contains("pub const Zero_1: Self = Self(2);"));
    assert!(core.contains("pub const Minus_One: Self = Self(3);"));
    // A value listed twice has one name().
    assert!(core.contains("2 => Some(\"Alias\")"));
    assert_eq!(core.matches("Some(\"Zero\")").count(), 1);
    assert!(core.contains("pub const fn name(self) -> Option<&'static str>"));
}

#[test]
fn modules_and_features_are_named_by_package() {
    let f = files();
    assert!(f["src/lib.rs"].contains("#[cfg(feature = \"engine\")]\npub mod engine;"));
    let cargo = &f["Cargo.toml"];
    assert!(cargo.contains("engine = [\"core_u_object\"]"));
    assert!(cargo.contains("core_u_object = []"));
    assert!(cargo.contains("mods = [\"core_u_object\", \"engine\"]"));
    assert!(f["src/lib.rs"].contains("pub type FName = ::ufathom_runtime::FName12;"));
}
