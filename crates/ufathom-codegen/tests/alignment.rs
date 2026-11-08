//! Classes whose dump records an alignment of 1.
//!
//! Some engine builds register native classes without an alignment, so the
//! dump says 1 for a class that holds pointers and 16-aligned structs. The
//! generator must raise the alignment from the evidence in the type, keep the
//! size and every offset, and still produce a crate that compiles with its
//! compile-time `offset_of!` and `size_of` assertions.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "tests may panic"
)]

mod common;

use common::dumps::{CORE, ENGINE, field, path, script_struct, structure};
use common::{generate_and_check, local_options, make_dump};
use ufathom_codegen::{Degradation, DegradeReason, GenReport, generate};
use ufathom_model::{Dump, FieldDef, PropertyType, StructDef, StructKind};
use ufathom_testkit::Profile;

const GAME: &str = "/Game/Blueprints";

fn object_ptr() -> PropertyType {
    PropertyType::Object {
        class: path(ENGINE, "Object"),
    }
}

fn struct_of(package: &str, name: &str) -> PropertyType {
    PropertyType::Struct {
        path: path(package, name),
    }
}

/// A class that records no alignment, like a native class in the affected build.
fn unaligned_class(
    name: &str,
    super_name: Option<&str>,
    size: u32,
    fields: Vec<FieldDef>,
) -> StructDef {
    let super_path = super_name.map(|s| path(ENGINE, s));
    structure(
        ENGINE,
        name,
        StructKind::Class,
        super_path.as_deref(),
        size,
        1,
        fields,
        Vec::new(),
    )
}

/// A dump shaped like the affected build: the scene component holds an object
/// pointer and a 16-aligned struct and ends in tail padding that its child
/// reuses for its first member.
fn unaligned_dump() -> Dump {
    let structs = vec![
        // A script struct records its real alignment.
        script_struct(
            CORE,
            "Transform",
            0x30,
            16,
            vec![
                field("Rotation", 0, PropertyType::Float, 4),
                field("Scale", 0x20, PropertyType::Float, 4),
            ],
        ),
        unaligned_class("Object", None, 0x28, Vec::new()),
        unaligned_class(
            "SceneComponent",
            Some("Object"),
            0x250,
            vec![
                field("AttachParent", 0x30, object_ptr(), 8),
                field("Transform", 0x40, struct_of(CORE, "Transform"), 0x30),
                field("Owner", 0x240, object_ptr(), 8),
            ],
        ),
        // The first member sits at 0x248, inside the tail padding of the parent.
        unaligned_class(
            "MeshComponent",
            Some("SceneComponent"),
            0x260,
            vec![
                field("Mobility", 0x248, PropertyType::Int32, 4),
                field("Material", 0x250, object_ptr(), 8),
            ],
        ),
        // Only small members: nothing raises it past 4.
        script_struct(
            ENGINE,
            "Small",
            8,
            1,
            vec![
                field("A", 0, PropertyType::Int32, 4),
                field("B", 4, PropertyType::Int32, 4),
            ],
        ),
        // `Alpha` sorts before `Zeta` but needs the planned alignment of its
        // member, which is raised from the pointer inside `Zeta`.
        script_struct(
            ENGINE,
            "Alpha",
            0x10,
            1,
            vec![field("Inner", 0, struct_of(ENGINE, "Zeta"), 8)],
        ),
        script_struct(
            ENGINE,
            "Zeta",
            8,
            1,
            vec![field("Target", 0, object_ptr(), 8)],
        ),
        // `Zeta` as planned needs 8, so a member at offset 4 is misaligned and
        // must neither be typed nor raise the owner.
        script_struct(
            ENGINE,
            "Holder",
            0x10,
            1,
            vec![field("Inner", 4, struct_of(ENGINE, "Zeta"), 8)],
        ),
        // A generated class: its size is not a multiple of its alignment, but
        // the object is a heap allocation, so the Rust type is padded to 0x3B8.
        structure(
            GAME,
            "Generated_C",
            StructKind::Class,
            Some(&path(ENGINE, "Object")),
            0x3B1,
            8,
            vec![
                field("Target", 0x3A8, object_ptr(), 8),
                field("Flag", 0x3B0, PropertyType::UInt8, 1),
            ],
            Vec::new(),
        ),
        // The first member of the child starts at the unpadded end of the
        // parent, inside the parent's new tail padding. Its own size is
        // unpadded too.
        structure(
            GAME,
            "Derived_C",
            StructKind::Class,
            Some(&path(GAME, "Generated_C")),
            0x3C1,
            8,
            vec![
                field("Extra", 0x3B1, PropertyType::UInt8, 1),
                field("Other", 0x3B8, object_ptr(), 8),
            ],
            Vec::new(),
        ),
        // The size is a multiple of 4 only, so the clamp wins.
        script_struct(
            ENGINE,
            "Quarter",
            0x2C,
            1,
            vec![
                field("Count", 0, PropertyType::Int32, 4),
                field("Target", 8, object_ptr(), 8),
            ],
        ),
    ];
    let mut dump = make_dump(Profile::ue4_21_measured().layout, structs, Vec::new());
    dump.engine.claimed = Some("4.21.1".to_owned());
    dump
}

fn raised(report: &GenReport, name: &str) -> Option<(u32, u32)> {
    report
        .alignments_raised
        .iter()
        .find(|r| r.owner == path(ENGINE, name))
        .map(|r| (r.declared, r.used))
}

fn degraded_of<'a>(
    report: &'a GenReport,
    name: &str,
    reason: DegradeReason,
) -> Vec<&'a Degradation> {
    report
        .degraded
        .iter()
        .filter(|d| d.owner == path(ENGINE, name) && d.reason == reason)
        .collect()
}

fn engine_source(dir: &std::path::Path) -> String {
    std::fs::read_to_string(dir.join("src").join("engine.rs")).unwrap()
}

#[test]
fn unaligned_classes_are_raised_and_the_crate_compiles() {
    let (dir, report, _) = generate_and_check(&unaligned_dump(), "align-raised");

    // `Quarter` is the one type whose size forbids the alignment its member needs.
    let too_low: Vec<_> = report
        .degraded
        .iter()
        .filter(|d| d.reason == DegradeReason::AlignmentTooLow)
        .map(|d| d.owner.as_str())
        .collect();
    assert_eq!(too_low, vec!["/Script/Engine.Quarter"]);
    assert_eq!(raised(&report, "SceneComponent"), Some((1, 16)));
    assert_eq!(raised(&report, "MeshComponent"), Some((1, 16)));
    assert_eq!(raised(&report, "Alpha"), Some((1, 8)));
    assert_eq!(raised(&report, "Zeta"), Some((1, 8)));

    // The child reuses the tail padding of its parent: a cast base, not a field.
    let base = degraded_of(&report, "MeshComponent", DegradeReason::BaseNotEmbedded);
    assert_eq!(base.len(), 1, "{:?}", report.degraded);
    assert_eq!(base[0].member, "<base>");

    let text = engine_source(dir.path());
    assert!(text.contains("assert!(::core::mem::size_of::<UMeshComponent>() == 0x260);"));
    assert!(text.contains("assert!(::core::mem::align_of::<UMeshComponent>() == 16);"));
    assert!(text.contains("assert!(::core::mem::offset_of!(UMeshComponent, mobility) == 0x248);"));
    assert!(text.contains("assert!(::core::mem::align_of::<USceneComponent>() == 16);"));
    assert!(
        text.contains("pub transform: crate::core_uobject::FTransform,"),
        "the aligned struct member must be typed"
    );
    assert!(
        text.contains("(self as *const Self).cast::<crate::engine::USceneComponent>()"),
        "the child must view its parent by a pointer cast"
    );
    assert!(!text.contains("pub base: crate::engine::USceneComponent"));
}

#[test]
fn small_members_keep_a_small_alignment() {
    let dir = tempfile::tempdir().unwrap();
    let report = generate(&unaligned_dump(), &local_options(), dir.path()).unwrap();
    assert_eq!(raised(&report, "Small"), Some((1, 4)));
    assert!(engine_source(dir.path()).contains("assert!(::core::mem::align_of::<FSmall>() == 4);"));
}

#[test]
fn misaligned_and_clamped_types_degrade() {
    let dir = tempfile::tempdir().unwrap();
    let report = generate(&unaligned_dump(), &local_options(), dir.path()).unwrap();

    // The nested struct at offset 4 needs the planned alignment of `Zeta`, 8.
    let holder = degraded_of(&report, "Holder", DegradeReason::Misaligned);
    assert_eq!(holder.len(), 1, "{:?}", report.degraded);
    assert_eq!(raised(&report, "Holder"), None);

    // 0x2C is a multiple of 4 only: the 8-byte pointer at 8 cannot be typed.
    assert_eq!(raised(&report, "Quarter"), Some((1, 4)));
    assert_eq!(
        degraded_of(&report, "Quarter", DegradeReason::AlignmentTooLow).len(),
        1,
        "{:?}",
        report.degraded
    );
}

#[test]
fn the_result_does_not_depend_on_the_input_order() {
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    let first = generate(&unaligned_dump(), &local_options(), a.path()).unwrap();
    let mut dump = unaligned_dump();
    dump.structs.reverse();
    for s in &mut dump.structs {
        s.fields.reverse();
    }
    let second = generate(&dump, &local_options(), b.path()).unwrap();
    assert_eq!(first.alignments_raised, second.alignments_raised);
    assert_eq!(first.degraded, second.degraded);
    assert_eq!(engine_source(a.path()), engine_source(b.path()));
}

fn all_sources(dir: &std::path::Path) -> String {
    let mut files: Vec<_> = std::fs::read_dir(dir.join("src"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    files.sort();
    files
        .iter()
        .map(|p| std::fs::read_to_string(p).unwrap())
        .collect()
}

#[test]
fn generated_classes_are_padded_and_children_still_view_their_parent() {
    let (dir, report, _) = generate_and_check(&unaligned_dump(), "align-padded");

    let padded: Vec<_> = report
        .sizes_padded
        .iter()
        .map(|p| (p.owner.as_str(), p.dump_size, p.rust_size))
        .collect();
    // Only the two classes. The script struct of size 0x2C stays exact.
    assert_eq!(
        padded,
        vec![
            ("/Game/Blueprints.Derived_C", 0x3C1, 0x3C8),
            ("/Game/Blueprints.Generated_C", 0x3B1, 0x3B8),
        ]
    );

    // No alignment degradation for the classes.
    assert!(
        report
            .degraded
            .iter()
            .all(|d| !d.owner.starts_with(GAME) || d.reason == DegradeReason::BaseNotEmbedded),
        "{:?}",
        report.degraded
    );
    let base = report
        .degraded
        .iter()
        .filter(|d| d.owner == format!("{GAME}.Derived_C"))
        .collect::<Vec<_>>();
    assert_eq!(base.len(), 1, "{base:?}");
    assert_eq!(base[0].member, "<base>");

    let text = all_sources(dir.path());
    assert!(
        text.contains("assert!(::core::mem::size_of::<AGenerated_C>() == 0x3b8);")
            || text.contains("assert!(::core::mem::size_of::<UGenerated_C>() == 0x3b8);")
    );
    assert!(text.contains("== 0x3c8);"));
    assert!(text.contains("The dump says 0x3b1 bytes"));
}
