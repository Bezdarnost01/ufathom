//! Tests of layout data and pointer transforms.

use super::{
    EngineLayout, Location, NameTableLayout, ObjectArrayLayout, PropertySystem, TransformOp,
    UEnumLayout,
};
use crate::test_support::{
    layout_array_names_legacy, layout_array_names_wide_fname, layout_pool_modern,
    layout_pool_stock_ue5,
};

fn all_layouts() -> Vec<EngineLayout> {
    vec![
        layout_array_names_legacy(),
        layout_array_names_wide_fname(),
        layout_pool_modern(),
        layout_pool_stock_ue5(),
    ]
}

fn array_with(ops: Vec<TransformOp>) -> ObjectArrayLayout {
    let mut objects = layout_array_names_legacy().objects;
    objects.pointer_transform = ops;
    objects
}

#[test]
fn every_calibration_shape_survives_a_json_round_trip() {
    for layout in all_layouts() {
        let json = serde_json::to_string_pretty(&layout).unwrap();
        let back: EngineLayout = serde_json::from_str(&json).unwrap();
        assert_eq!(back, layout);
    }
}

#[test]
fn calibration_shapes_are_distinct() {
    let layouts = all_layouts();
    for (position, layout) in layouts.iter().enumerate() {
        for other in layouts.iter().skip(position + 1) {
            assert_ne!(layout, other);
        }
    }
}

#[test]
fn eight_and_twelve_byte_fname_place_outer_differently() {
    let narrow = layout_array_names_legacy();
    let wide = layout_array_names_wide_fname();
    assert_eq!(narrow.fname.size, 8);
    assert_eq!(narrow.uobject.outer, 0x20);
    assert_eq!(wide.fname.size, 12);
    assert_eq!(wide.uobject.outer, 0x28);
    assert_eq!(wide.fname.display_index, Some(4));
}

#[test]
fn ue4_25_pool_entry_matches_calibration() {
    let NameTableLayout::Pool(pool) = layout_pool_modern().names else {
        panic!("expected a pool");
    };
    assert_eq!(pool.entry.header_offset, 4);
    assert_eq!(pool.entry.string_offset, 6);
    assert_eq!(pool.entry.len_shift, 1);
    assert_eq!(pool.stride, 4);
}

#[test]
fn stock_pool_entry_and_outline_number_fname() {
    let layout = layout_pool_stock_ue5();
    let NameTableLayout::Pool(pool) = layout.names else {
        panic!("expected a pool");
    };
    assert_eq!(pool.entry.header_offset, 0);
    assert_eq!(pool.entry.string_offset, 2);
    assert_eq!(pool.entry.len_shift, 6);
    assert_eq!(pool.stride, 2);
    assert_eq!(layout.fname.number, None);
}

#[test]
fn array_names_and_pool_names_are_both_representable() {
    assert!(matches!(
        layout_array_names_legacy().names,
        NameTableLayout::Array(_)
    ));
    assert!(matches!(
        layout_pool_modern().names,
        NameTableLayout::Pool(_)
    ));
}

#[test]
fn legacy_and_modern_properties_are_both_representable() {
    assert!(matches!(
        layout_array_names_legacy().properties.system,
        PropertySystem::Legacy
    ));
    assert!(matches!(
        layout_pool_modern().properties.system,
        PropertySystem::Modern { .. }
    ));
}

#[test]
fn array_inner_can_sit_away_from_the_property_size() {
    let layout = layout_pool_stock_ue5();
    assert_ne!(
        layout.properties.typed.array_inner,
        Some(layout.properties.size)
    );
}

#[test]
fn both_enum_layouts_are_representable() {
    assert!(matches!(
        layout_array_names_legacy().uenum,
        UEnumLayout::PairArray { .. }
    ));
    assert!(matches!(
        layout_pool_stock_ue5().uenum,
        UEnumLayout::SplitArrays { .. }
    ));
}

#[test]
fn min_alignment_width_can_be_two_or_four() {
    assert_eq!(layout_array_names_legacy().ustruct.min_alignment_width, 4);
    assert_eq!(layout_pool_stock_ue5().ustruct.min_alignment_width, 2);
}

#[test]
fn location_variants_round_trip() {
    for location in [
        Location::Absolute(0x1234),
        Location::Rva {
            module: "a.exe".to_owned(),
            rva: 8,
        },
    ] {
        let json = serde_json::to_string(&location).unwrap();
        assert_eq!(serde_json::from_str::<Location>(&json).unwrap(), location);
    }
}

#[test]
fn missing_pointer_transform_in_json_means_no_steps() {
    let mut value = serde_json::to_value(layout_array_names_legacy().objects).unwrap();
    value.as_object_mut().unwrap().remove("pointer_transform");
    let objects: ObjectArrayLayout = serde_json::from_value(value).unwrap();
    assert!(objects.pointer_transform.is_empty());
}

#[test]
fn empty_transform_is_the_identity() {
    let objects = array_with(Vec::new());
    for value in [0, 1, 0xDEAD_BEEF, u64::MAX] {
        assert_eq!(objects.apply_pointer_transform(value), value);
    }
}

#[test]
fn xor_not_add_sub_behave() {
    assert_eq!(TransformOp::Xor(0xFF).apply(0x0F), 0xF0);
    assert_eq!(TransformOp::Not.apply(0), u64::MAX);
    assert_eq!(TransformOp::Add(5).apply(10), 15);
    assert_eq!(TransformOp::Sub(5).apply(10), 5);
}

#[test]
fn add_and_sub_wrap_instead_of_panicking() {
    assert_eq!(TransformOp::Add(1).apply(u64::MAX), 0);
    assert_eq!(TransformOp::Add(u64::MAX).apply(u64::MAX), u64::MAX - 1);
    assert_eq!(TransformOp::Sub(1).apply(0), u64::MAX);
    assert_eq!(TransformOp::Sub(u64::MAX).apply(0), 1);
}

#[test]
fn rotate_by_zero_and_by_width_is_the_identity() {
    let value = 0x0123_4567_89AB_CDEF;
    for bits in [0, 64, 128] {
        assert_eq!(TransformOp::RotateLeft(bits).apply(value), value);
        assert_eq!(TransformOp::RotateRight(bits).apply(value), value);
    }
}

#[test]
fn rotate_counts_wrap_modulo_the_width() {
    let value = 0x0123_4567_89AB_CDEF;
    assert_eq!(
        TransformOp::RotateLeft(65).apply(value),
        TransformOp::RotateLeft(1).apply(value)
    );
    assert_eq!(
        TransformOp::RotateRight(u32::MAX).apply(value),
        TransformOp::RotateRight(u32::MAX % 64).apply(value)
    );
    assert_eq!(
        TransformOp::RotateLeft(4).apply(0xF000_0000_0000_0001),
        0x1F
    );
    assert_eq!(
        TransformOp::RotateRight(4).apply(0x1F),
        0xF000_0000_0000_0001
    );
}

#[test]
fn rotate_left_and_right_undo_each_other() {
    let value = 0xFEDC_BA98_7654_3210;
    for bits in 0..=130 {
        let rotated = TransformOp::RotateLeft(bits).apply(value);
        assert_eq!(TransformOp::RotateRight(bits).apply(rotated), value);
    }
}

#[test]
fn steps_apply_in_declared_order() {
    let forward = array_with(vec![TransformOp::Add(1), TransformOp::Xor(0xFF)]);
    let backward = array_with(vec![TransformOp::Xor(0xFF), TransformOp::Add(1)]);
    // (0 + 1) ^ 0xFF = 0xFE, but (0 ^ 0xFF) + 1 = 0x100.
    assert_eq!(forward.apply_pointer_transform(0), 0xFE);
    assert_eq!(backward.apply_pointer_transform(0), 0x100);
}

#[test]
fn a_chain_can_be_inverted_step_by_step() {
    let objects = array_with(vec![
        TransformOp::Xor(0xA5A5_A5A5_A5A5_A5A5),
        TransformOp::RotateLeft(7),
        TransformOp::Add(0x1000),
        TransformOp::Not,
    ]);
    let real: u64 = 0x0000_7FF6_1234_5678;
    // Encode with the inverse steps, then decode with the chain.
    let stored = (!real).wrapping_sub(0x1000).rotate_right(7) ^ 0xA5A5_A5A5_A5A5_A5A5;
    assert_eq!(objects.apply_pointer_transform(stored), real);
}

#[test]
fn a_layout_with_no_typed_payload_offsets_round_trips() {
    let mut layout = layout_pool_modern();
    layout.properties.typed = super::TypedPayloadLayout {
        bool_property: None,
        object_class: None,
        class_meta: None,
        script_struct: None,
        array_inner: None,
        set_element: None,
        map_key: None,
        map_value: None,
        enum_underlying: None,
        enum_type: None,
        byte_enum: None,
        delegate_signature: None,
        interface_class: None,
        optional_value: None,
    };
    let json = serde_json::to_string(&layout).unwrap();
    assert_eq!(serde_json::from_str::<EngineLayout>(&json).unwrap(), layout);
}
