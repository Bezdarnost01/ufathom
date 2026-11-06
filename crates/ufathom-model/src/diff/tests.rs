//! One test per diff rule.

use super::{
    Ambiguity, AmbiguousSubject, DumpDiff, EnumChange, FunctionChange, ParamChange, ParamFlag,
    SlotChange, TypeChange, TypeKey, diff,
};
use crate::dump::{BoolBits, Dump, PropertyType, StructKind};
use crate::flags::{
    CPF_CONST_PARM, CPF_OUT_PARM, CPF_REFERENCE_PARM, CPF_RETURN_PARM, FUNC_BLUEPRINT_CALLABLE,
    FUNC_NATIVE, FUNC_NET, FUNC_NET_CLIENT, FUNC_NET_MULTICAST, FUNC_NET_SERVER,
};
use crate::test_support::{
    busy_dump, class, dump_of, enumeration, field, function, layout_pool_stock_ue5, param,
    return_value,
};

const ENGINE: &str = "/Script/Engine";
const ACTOR: &str = "/Script/Engine.Actor";

fn bool_type(byte_offset: u8, byte_mask: u8) -> PropertyType {
    PropertyType::Bool {
        byte_offset,
        byte_mask,
        field_mask: byte_mask,
        field_size: 1,
    }
}

fn object_of(class: &str) -> PropertyType {
    PropertyType::Object {
        class: class.to_owned(),
    }
}

/// `Object <- Actor <- Pawn`, with one function on `Actor` and one on `Pawn`.
fn base_dump() -> Dump {
    let mut actor = class(ENGINE, "Actor", Some("/Script/CoreUObject.Object"), 0x100);
    actor.fields = vec![
        field("Health", 0x20, PropertyType::Float),
        field("bHidden", 0x24, bool_type(0, 0x01)),
    ];
    actor.functions = vec![
        function(
            ACTOR,
            "Hit",
            FUNC_NET_SERVER,
            vec![
                param("Amount", 0, PropertyType::Float, 0),
                param("Instigator", 8, object_of(ACTOR), 0),
            ],
        ),
        function(
            ACTOR,
            "Heal",
            FUNC_NATIVE,
            vec![param("Amount", 0, PropertyType::Float, 0)],
        ),
    ];
    let mut pawn = class(ENGINE, "Pawn", Some(ACTOR), 0x180);
    pawn.fields = vec![field("Speed", 0x100, PropertyType::Float)];
    dump_of(
        vec![actor, pawn],
        vec![enumeration(
            ENGINE,
            "ENetRole",
            &[("None", 0), ("Authority", 3)],
        )],
    )
}

fn actor_mut(dump: &mut Dump) -> &mut crate::dump::StructDef {
    dump.structs
        .iter_mut()
        .find(|def| def.name == "Actor")
        .unwrap()
}

fn pawn_mut(dump: &mut Dump) -> &mut crate::dump::StructDef {
    dump.structs
        .iter_mut()
        .find(|def| def.name == "Pawn")
        .unwrap()
}

fn function_mut<'a>(
    def: &'a mut crate::dump::StructDef,
    name: &str,
) -> &'a mut crate::dump::FunctionDef {
    def.functions
        .iter_mut()
        .find(|function| function.name == name)
        .unwrap()
}

fn key(package: &str, name: &str) -> TypeKey {
    TypeKey::new(package, name)
}

/// The changes of the only changed type, which must be `name`.
fn only_type_changes(result: &DumpDiff, name: &str) -> Vec<TypeChange> {
    assert_eq!(result.changed_types.len(), 1, "{result}");
    assert_eq!(result.changed_types[0].key.name, name);
    result.changed_types[0].changes.clone()
}

/// The changes recorded for the type `name`, whatever else changed.
fn changes_of(result: &DumpDiff, name: &str) -> Vec<TypeChange> {
    result
        .changed_types
        .iter()
        .find(|changed| changed.key.name == name)
        .map(|changed| changed.changes.clone())
        .unwrap_or_default()
}

/// Changes to the `Hit` function after `edit` is applied to it in the new dump.
fn hit_changes(edit: impl FnOnce(&mut crate::dump::FunctionDef)) -> Vec<FunctionChange> {
    let old = base_dump();
    let mut new = base_dump();
    edit(function_mut(actor_mut(&mut new), "Hit"));
    let result = diff(&old, &new);
    let changes = only_type_changes(&result, "Actor");
    assert_eq!(changes.len(), 1, "{changes:?}");
    match changes.into_iter().next() {
        Some(TypeChange::FunctionChanged { name, changes }) => {
            assert_eq!(name, "Hit");
            changes
        }
        other => panic!("expected FunctionChanged, got {other:?}"),
    }
}

// ----- identity and noise -----

#[test]
fn identical_dumps_give_an_empty_diff() {
    for dump in [base_dump(), busy_dump()] {
        let result = diff(&dump, &dump.clone());
        assert!(result.is_empty(), "{result}");
        assert_eq!(result, DumpDiff::default());
    }
}

#[test]
fn empty_dumps_give_an_empty_diff() {
    assert!(
        diff(
            &dump_of(Vec::new(), Vec::new()),
            &dump_of(Vec::new(), Vec::new())
        )
        .is_empty()
    );
}

#[test]
fn member_order_produces_no_difference() {
    let old = base_dump();
    let mut new = base_dump();
    new.structs.reverse();
    new.enums.reverse();
    for def in &mut new.structs {
        def.fields.reverse();
        def.functions.reverse();
    }
    for def in &mut new.enums {
        def.values.reverse();
    }
    assert!(diff(&old, &new).is_empty());
    assert!(diff(&new, &old).is_empty());
}

#[test]
fn native_rva_produces_no_difference() {
    let old = base_dump();
    let mut new = base_dump();
    function_mut(actor_mut(&mut new), "Hit").native_rva = Some(0x12_3456);
    function_mut(actor_mut(&mut new), "Heal").native_rva = Some(0x9);
    assert!(diff(&old, &new).is_empty());
}

#[test]
fn layout_target_and_engine_differences_produce_no_difference() {
    let old = base_dump();
    let mut new = base_dump();
    new.layout = layout_pool_stock_ue5();
    new.target.build_id = "other-build".to_owned();
    new.engine.claimed = None;
    new.diagnostics.clear();
    assert!(diff(&old, &new).is_empty());
}

#[test]
fn diff_is_independent_of_unrelated_input_order() {
    let mut old = busy_dump();
    let mut new = busy_dump();
    new.structs[0].size += 8;
    new.enums[0].values.push(crate::dump::EnumValue {
        name: "Extra".to_owned(),
        value: 9,
    });
    let straight = diff(&old, &new);
    old.structs.reverse();
    new.structs.reverse();
    new.enums.reverse();
    let reordered = diff(&old, &new);
    assert_eq!(straight, reordered);
    assert_eq!(straight.to_string(), reordered.to_string());
    assert!(!straight.is_empty());
}

// ----- types -----

#[test]
fn added_and_removed_types_are_reported() {
    let old = base_dump();
    let mut new = base_dump();
    new.structs.retain(|def| def.name != "Pawn");
    new.structs
        .push(class(ENGINE, "Controller", Some(ACTOR), 0x120));

    let result = diff(&old, &new);
    assert_eq!(result.added_types, [key(ENGINE, "Controller")]);
    assert_eq!(result.removed_types, [key(ENGINE, "Pawn")]);
    assert!(result.changed_types.is_empty());
    assert!(!result.is_empty());
}

#[test]
fn types_match_by_package_and_name_not_by_path() {
    let old = base_dump();
    let mut new = base_dump();
    pawn_mut(&mut new).path = "/Game/Moved.Pawn_Path".to_owned();
    assert!(diff(&old, &new).is_empty());
}

#[test]
fn same_name_in_another_package_is_a_different_type() {
    let old = base_dump();
    let mut new = base_dump();
    pawn_mut(&mut new).package = "/Script/Other".to_owned();
    let result = diff(&old, &new);
    assert_eq!(result.removed_types, [key(ENGINE, "Pawn")]);
    assert_eq!(result.added_types, [key("/Script/Other", "Pawn")]);
}

#[test]
fn parent_size_alignment_and_kind_changes_are_reported() {
    let old = base_dump();
    let mut new = base_dump();
    let pawn = pawn_mut(&mut new);
    pawn.super_path = Some("/Script/Engine.Other".to_owned());
    pawn.size = 0x200;
    pawn.alignment = 16;
    pawn.kind = StructKind::ScriptStruct;

    let changes = only_type_changes(&diff(&old, &new), "Pawn");
    assert_eq!(
        changes,
        [
            TypeChange::KindChanged {
                old: StructKind::Class,
                new: StructKind::ScriptStruct
            },
            TypeChange::ParentChanged {
                old: Some(ACTOR.to_owned()),
                new: Some("/Script/Engine.Other".to_owned())
            },
            TypeChange::SizeChanged {
                old: 0x180,
                new: 0x200
            },
            TypeChange::AlignmentChanged { old: 8, new: 16 },
        ]
    );
}

#[test]
fn losing_a_parent_is_a_parent_change() {
    let old = base_dump();
    let mut new = base_dump();
    pawn_mut(&mut new).super_path = None;
    let changes = only_type_changes(&diff(&old, &new), "Pawn");
    assert_eq!(
        changes,
        [TypeChange::ParentChanged {
            old: Some(ACTOR.to_owned()),
            new: None
        }]
    );
}

// ----- fields -----

#[test]
fn added_and_removed_fields_are_reported() {
    let old = base_dump();
    let mut new = base_dump();
    let actor = actor_mut(&mut new);
    actor.fields.retain(|field| field.name != "Health");
    actor.fields.push(field("Armor", 0x30, PropertyType::Int32));

    let changes = only_type_changes(&diff(&old, &new), "Actor");
    assert_eq!(
        changes,
        [
            TypeChange::FieldAdded {
                name: "Armor".to_owned(),
                offset: 0x30
            },
            TypeChange::FieldRemoved {
                name: "Health".to_owned(),
                offset: 0x20
            },
        ]
    );
}

fn field_change(edit: impl FnOnce(&mut crate::dump::FieldDef)) -> Vec<SlotChange> {
    let old = base_dump();
    let mut new = base_dump();
    let target = actor_mut(&mut new)
        .fields
        .iter_mut()
        .find(|field| field.name == "Health")
        .unwrap();
    edit(target);
    let changes = only_type_changes(&diff(&old, &new), "Actor");
    match changes.as_slice() {
        [TypeChange::FieldChanged { name, changes }] => {
            assert_eq!(name, "Health");
            changes.clone()
        }
        other => panic!("expected one FieldChanged, got {other:?}"),
    }
}

#[test]
fn field_offset_change_is_reported() {
    assert_eq!(
        field_change(|field| field.offset = 0x28),
        [SlotChange::Offset {
            old: 0x20,
            new: 0x28
        }]
    );
}

#[test]
fn field_type_change_is_reported() {
    assert_eq!(
        field_change(|field| field.ty = PropertyType::Double),
        [SlotChange::Type {
            old: PropertyType::Float,
            new: PropertyType::Double
        }]
    );
}

#[test]
fn field_type_change_inside_a_container_is_reported() {
    let mut old = base_dump();
    let mut new = base_dump();
    actor_mut(&mut old).fields.push(field(
        "Items",
        0x40,
        PropertyType::Array {
            inner: Box::new(PropertyType::Int32),
        },
    ));
    actor_mut(&mut new).fields.push(field(
        "Items",
        0x40,
        PropertyType::Array {
            inner: Box::new(PropertyType::Int64),
        },
    ));
    let changes = only_type_changes(&diff(&old, &new), "Actor");
    assert!(matches!(
        changes.as_slice(),
        [TypeChange::FieldChanged { name, changes }]
            if name == "Items" && matches!(changes.as_slice(), [SlotChange::Type { .. }])
    ));
}

#[test]
fn field_size_and_array_dim_changes_are_reported() {
    assert_eq!(
        field_change(|field| {
            field.element_size = 8;
            field.array_dim = 3;
        }),
        [
            SlotChange::ElementSize { old: 4, new: 8 },
            SlotChange::ArrayDim { old: 1, new: 3 }
        ]
    );
}

#[test]
fn bool_mask_and_byte_offset_changes_are_reported() {
    let old = base_dump();
    let mut new = base_dump();
    for target in &mut actor_mut(&mut new).fields {
        if target.name == "bHidden" {
            target.ty = bool_type(1, 0x02);
        }
    }
    let changes = only_type_changes(&diff(&old, &new), "Actor");
    assert_eq!(
        changes,
        [TypeChange::FieldChanged {
            name: "bHidden".to_owned(),
            changes: vec![SlotChange::BoolBits {
                old: BoolBits {
                    byte_offset: 0,
                    byte_mask: 0x01,
                    field_mask: 0x01,
                    field_size: 1
                },
                new: BoolBits {
                    byte_offset: 1,
                    byte_mask: 0x02,
                    field_mask: 0x02,
                    field_size: 1
                },
            }]
        }]
    );
}

#[test]
fn a_bool_becoming_another_type_is_a_type_change() {
    let old = base_dump();
    let mut new = base_dump();
    for target in &mut actor_mut(&mut new).fields {
        if target.name == "bHidden" {
            target.ty = PropertyType::UInt8;
        }
    }
    let changes = only_type_changes(&diff(&old, &new), "Actor");
    assert!(matches!(
        changes.as_slice(),
        [TypeChange::FieldChanged { changes, .. }]
            if matches!(changes.as_slice(), [SlotChange::Type { .. }])
    ));
}

// ----- enums -----

#[test]
fn added_and_removed_enums_are_reported() {
    let old = base_dump();
    let mut new = base_dump();
    new.enums.clear();
    new.enums.push(enumeration(ENGINE, "EMode", &[("A", 0)]));
    let result = diff(&old, &new);
    assert_eq!(result.added_enums, [key(ENGINE, "EMode")]);
    assert_eq!(result.removed_enums, [key(ENGINE, "ENetRole")]);
}

#[test]
fn enum_value_changes_are_reported() {
    let old = base_dump();
    let mut new = base_dump();
    new.enums[0] = enumeration(ENGINE, "ENetRole", &[("Authority", 4), ("Extra", 9)]);
    new.enums[0].underlying_size = 4;
    let result = diff(&old, &new);
    assert_eq!(result.changed_enums.len(), 1);
    assert_eq!(
        result.changed_enums[0].changes,
        [
            EnumChange::UnderlyingSizeChanged { old: 1, new: 4 },
            EnumChange::ValueChanged {
                name: "Authority".to_owned(),
                old: 3,
                new: 4
            },
            EnumChange::ValueAdded {
                name: "Extra".to_owned(),
                value: 9
            },
            EnumChange::ValueRemoved {
                name: "None".to_owned(),
                value: 0
            },
        ]
    );
}

#[test]
fn enum_and_struct_namespaces_are_separate() {
    let old = base_dump();
    let mut new = base_dump();
    new.enums.push(enumeration(ENGINE, "Actor", &[("A", 0)]));
    let result = diff(&old, &new);
    assert_eq!(result.added_enums, [key(ENGINE, "Actor")]);
    assert!(result.added_types.is_empty());
}

// ----- function ABI -----

#[test]
fn renamed_parameter_is_not_an_abi_change() {
    let old = base_dump();
    let mut new = base_dump();
    let hit = function_mut(actor_mut(&mut new), "Hit");
    hit.params[0].name = "Damage".to_owned();
    hit.params[1].name = "Culprit".to_owned();
    assert!(diff(&old, &new).is_empty());
}

#[test]
fn parameter_type_change_is_reported_with_its_position() {
    let changes = hit_changes(|hit| hit.params[1].ty = object_of("/Script/Engine.Pawn"));
    assert_eq!(
        changes,
        [FunctionChange::Param {
            index: 1,
            name: "Instigator".to_owned(),
            changes: vec![ParamChange::Slot(SlotChange::Type {
                old: object_of(ACTOR),
                new: object_of("/Script/Engine.Pawn")
            })]
        }]
    );
}

#[test]
fn parameter_offset_and_size_changes_are_reported() {
    let changes = hit_changes(|hit| {
        hit.params[0].offset = 4;
        hit.params[0].element_size = 8;
        hit.params_size = 0x18;
    });
    assert_eq!(
        changes,
        [
            FunctionChange::ParamsSize {
                old: 0xC,
                new: 0x18
            },
            FunctionChange::Param {
                index: 0,
                name: "Amount".to_owned(),
                changes: vec![
                    ParamChange::Slot(SlotChange::Offset { old: 0, new: 4 }),
                    ParamChange::Slot(SlotChange::ElementSize { old: 4, new: 8 }),
                ]
            }
        ]
    );
}

#[test]
fn parameters_are_compared_by_order_not_by_name() {
    // Same two names, swapped positions: position 0 is now a float.
    let changes = hit_changes(|hit| {
        hit.params = vec![
            param("Instigator", 8, object_of(ACTOR), 0),
            param("Amount", 0, PropertyType::Float, 0),
        ];
    });
    assert_eq!(changes.len(), 2, "{changes:?}");
    assert!(changes.iter().all(|change| matches!(
        change,
        FunctionChange::Param { index, .. } if *index < 2
    )));
}

#[test]
fn parameter_flags_out_return_reference_and_const_are_reported() {
    for (flag, mask) in [
        (ParamFlag::Out, CPF_OUT_PARM),
        (ParamFlag::Return, CPF_RETURN_PARM),
        (ParamFlag::Reference, CPF_REFERENCE_PARM),
        (ParamFlag::Const, CPF_CONST_PARM),
    ] {
        let changes = hit_changes(|hit| hit.params[0].flags |= mask);
        assert_eq!(
            changes,
            [FunctionChange::Param {
                index: 0,
                name: "Amount".to_owned(),
                changes: vec![ParamChange::Flag {
                    flag,
                    old: false,
                    new: true
                }]
            }],
            "flag {flag}"
        );
    }
}

#[test]
fn clearing_a_parameter_flag_is_reported() {
    let mut old = base_dump();
    function_mut(actor_mut(&mut old), "Hit").params[0].flags |= CPF_OUT_PARM;
    let new = base_dump();
    let changes = only_type_changes(&diff(&old, &new), "Actor");
    assert!(matches!(
        changes.as_slice(),
        [TypeChange::FunctionChanged { changes, .. }]
            if matches!(
                changes.as_slice(),
                [FunctionChange::Param { changes, .. }]
                    if matches!(changes.as_slice(), [ParamChange::Flag { old: true, new: false, .. }])
            )
    ));
}

#[test]
fn network_flag_changes_are_reported() {
    for (from, to) in [
        (FUNC_NET_SERVER, FUNC_NET_CLIENT),
        (FUNC_NET_SERVER, FUNC_NET_SERVER | FUNC_NET),
        (FUNC_NET_SERVER, FUNC_NET_MULTICAST),
        (FUNC_NET_SERVER, 0),
    ] {
        let mut old = base_dump();
        function_mut(actor_mut(&mut old), "Hit").flags = from;
        let mut new = base_dump();
        function_mut(actor_mut(&mut new), "Hit").flags = to;
        let changes = only_type_changes(&diff(&old, &new), "Actor");
        assert_eq!(
            changes,
            [TypeChange::FunctionChanged {
                name: "Hit".to_owned(),
                changes: vec![FunctionChange::NetworkFlags { old: from, new: to }]
            }]
        );
    }
}

#[test]
fn non_network_function_flags_are_not_an_abi_change() {
    let old = base_dump();
    let mut new = base_dump();
    function_mut(actor_mut(&mut new), "Hit").flags |= FUNC_BLUEPRINT_CALLABLE | FUNC_NATIVE;
    assert!(diff(&old, &new).is_empty());
}

#[test]
fn parameter_count_change_is_reported_and_prefix_still_compared() {
    let changes = hit_changes(|hit| {
        hit.params.push(return_value(
            0x10,
            PropertyType::Bool {
                byte_offset: 0,
                byte_mask: 1,
                field_mask: 1,
                field_size: 1,
            },
        ));
        hit.params[0].ty = PropertyType::Int32;
        hit.params_size = 0x14;
    });
    assert!(changes.contains(&FunctionChange::ParamCount { old: 2, new: 3 }));
    assert!(changes.contains(&FunctionChange::ParamsSize {
        old: 0xC,
        new: 0x14
    }));
    assert!(
        changes
            .iter()
            .any(|change| matches!(change, FunctionChange::Param { index: 0, .. }))
    );
}

#[test]
fn added_function_is_reported() {
    let old = base_dump();
    let mut new = base_dump();
    actor_mut(&mut new)
        .functions
        .push(function(ACTOR, "Kill", 0, Vec::new()));
    let changes = only_type_changes(&diff(&old, &new), "Actor");
    assert_eq!(
        changes,
        [TypeChange::FunctionAdded {
            name: "Kill".to_owned()
        }]
    );
}

#[test]
fn removed_function_with_no_ancestor_declaring_it_is_reported() {
    let old = base_dump();
    let mut new = base_dump();
    actor_mut(&mut new).functions.retain(|f| f.name != "Heal");
    let changes = only_type_changes(&diff(&old, &new), "Actor");
    assert_eq!(
        changes,
        [TypeChange::FunctionRemoved {
            name: "Heal".to_owned()
        }]
    );
}

// ----- function moved through ancestors -----

/// `Object <- Actor <- Pawn <- Character`. In the old dump `Character` declares
/// `Jump`; the tests decide where it lives in the new dump.
fn character_chain() -> (Dump, Dump) {
    let character_path = "/Script/Engine.Character";
    let mut old = base_dump();
    let mut character = class(ENGINE, "Character", Some("/Script/Engine.Pawn"), 0x200);
    character.functions = vec![function(
        character_path,
        "Jump",
        0,
        vec![param("Power", 0, PropertyType::Float, 0)],
    )];
    old.structs.push(character.clone());
    let mut new = base_dump();
    character.functions.clear();
    new.structs.push(character);
    (old, new)
}

#[test]
fn function_found_on_an_ancestor_is_moved_not_removed() {
    let (old, mut new) = character_chain();
    pawn_mut(&mut new).functions.push(function(
        "/Script/Engine.Pawn",
        "Jump",
        0,
        vec![param("Power", 0, PropertyType::Float, 0)],
    ));
    let result = diff(&old, &new);
    let changes = changes_of(&result, "Character");
    assert_eq!(
        changes,
        [TypeChange::FunctionMoved {
            name: "Jump".to_owned(),
            to: key(ENGINE, "Pawn")
        }]
    );
    assert!(result.ambiguities.is_empty());
}

#[test]
fn function_is_found_on_a_distant_ancestor() {
    let (old, mut new) = character_chain();
    actor_mut(&mut new).functions.push(function(
        ACTOR,
        "Jump",
        0,
        vec![param("Power", 0, PropertyType::Float, 0)],
    ));
    let changes = changes_of(&diff(&old, &new), "Character");
    assert_eq!(
        changes,
        [TypeChange::FunctionMoved {
            name: "Jump".to_owned(),
            to: key(ENGINE, "Actor")
        }]
    );
}

#[test]
fn the_nearest_ancestor_declaring_the_function_wins() {
    let (old, mut new) = character_chain();
    for (path, owner) in [("/Script/Engine.Pawn", true), (ACTOR, false)] {
        let functions = vec![function(
            path,
            "Jump",
            0,
            vec![param("Power", 0, PropertyType::Float, 0)],
        )];
        if owner {
            pawn_mut(&mut new).functions.extend(functions);
        } else {
            actor_mut(&mut new).functions.extend(functions);
        }
    }
    let changes = changes_of(&diff(&old, &new), "Character");
    assert_eq!(
        changes,
        [TypeChange::FunctionMoved {
            name: "Jump".to_owned(),
            to: key(ENGINE, "Pawn")
        }]
    );
}

#[test]
fn a_moved_function_is_still_compared_by_abi() {
    let (old, mut new) = character_chain();
    pawn_mut(&mut new).functions.push(function(
        "/Script/Engine.Pawn",
        "Jump",
        FUNC_NET_SERVER,
        vec![param("Power", 0, PropertyType::Double, 0)],
    ));
    let changes = changes_of(&diff(&old, &new), "Character");
    assert_eq!(changes.len(), 2, "{changes:?}");
    assert!(matches!(changes[0], TypeChange::FunctionMoved { .. }));
    assert!(matches!(
        &changes[1],
        TypeChange::FunctionChanged { name, changes }
            if name == "Jump"
                && changes.iter().any(|c| matches!(c, FunctionChange::NetworkFlags { .. }))
                && changes.iter().any(|c| matches!(c, FunctionChange::Param { index: 0, .. }))
    ));
}

#[test]
fn function_on_a_descendant_does_not_count_as_moved() {
    let (old, mut new) = character_chain();
    // The function now exists only in a type below Character.
    let mut sprinter = class(ENGINE, "Sprinter", Some("/Script/Engine.Character"), 0x220);
    sprinter.functions = vec![function("/Script/Engine.Sprinter", "Jump", 0, Vec::new())];
    new.structs.push(sprinter);
    let changes = changes_of(&diff(&old, &new), "Character");
    assert_eq!(
        changes,
        [TypeChange::FunctionRemoved {
            name: "Jump".to_owned()
        }]
    );
}

#[test]
fn ancestor_search_survives_a_parent_cycle_and_a_missing_parent() {
    let (old, mut new) = character_chain();
    actor_mut(&mut new).super_path = Some("/Script/Engine.Character".to_owned());
    // Pawn -> Actor -> Character -> Pawn is a loop: the search must end and
    // report the function as removed instead of spinning.
    let result = diff(&old, &new);
    assert!(
        changes_of(&result, "Actor")
            .iter()
            .any(|c| matches!(c, TypeChange::ParentChanged { .. }))
    );
    assert!(
        changes_of(&result, "Character").contains(&TypeChange::FunctionRemoved {
            name: "Jump".to_owned(),
        })
    );

    let (old, mut new) = character_chain();
    pawn_mut(&mut new).super_path = Some("/Script/Nowhere.Missing".to_owned());
    let result = diff(&old, &new);
    assert!(result.changed_types.iter().any(|changed| {
        changed.key.name == "Character"
            && changed.changes.contains(&TypeChange::FunctionRemoved {
                name: "Jump".to_owned(),
            })
    }));
}

// ----- ambiguity -----

#[test]
fn duplicate_type_is_ambiguous_and_never_picked() {
    let old = base_dump();
    let mut new = base_dump();
    let mut second = class(ENGINE, "Pawn", Some(ACTOR), 0x999);
    second.path = "/Script/Engine.Pawn_Duplicate".to_owned();
    new.structs.push(second);

    let result = diff(&old, &new);
    assert_eq!(
        result.ambiguities,
        [Ambiguity {
            subject: AmbiguousSubject::Type,
            name: "/Script/Engine.Pawn".to_owned(),
            old_count: 1,
            new_count: 2
        }]
    );
    assert!(result.changed_types.is_empty());
    assert!(result.added_types.is_empty());
    assert!(result.removed_types.is_empty());
    assert!(!result.is_empty());
}

#[test]
fn duplicate_on_the_old_side_is_also_ambiguous() {
    let mut old = base_dump();
    let new = base_dump();
    old.structs.push(class(ENGINE, "Pawn", Some(ACTOR), 0x1));
    let result = diff(&old, &new);
    assert_eq!(result.ambiguities.len(), 1);
    assert_eq!(result.ambiguities[0].old_count, 2);
    assert_eq!(result.ambiguities[0].new_count, 1);
    assert!(result.changed_types.is_empty());
}

#[test]
fn duplicate_field_name_is_ambiguous_and_other_fields_still_compare() {
    let old = base_dump();
    let mut new = base_dump();
    let actor = actor_mut(&mut new);
    actor
        .fields
        .push(field("Health", 0x50, PropertyType::Int32));
    actor.fields.push(field("Armor", 0x60, PropertyType::Int32));

    let result = diff(&old, &new);
    assert_eq!(
        result.ambiguities,
        [Ambiguity {
            subject: AmbiguousSubject::Field {
                owner: key(ENGINE, "Actor")
            },
            name: "Health".to_owned(),
            old_count: 1,
            new_count: 2
        }]
    );
    let changes = only_type_changes(&result, "Actor");
    assert_eq!(
        changes,
        [TypeChange::FieldAdded {
            name: "Armor".to_owned(),
            offset: 0x60
        }]
    );
}

#[test]
fn duplicate_function_name_is_ambiguous_and_never_picked() {
    let old = base_dump();
    let mut new = base_dump();
    // One copy equal to the old function, one different: any pick could lie.
    actor_mut(&mut new)
        .functions
        .push(function(ACTOR, "Hit", 0, Vec::new()));
    let result = diff(&old, &new);
    assert_eq!(
        result.ambiguities,
        [Ambiguity {
            subject: AmbiguousSubject::Function {
                owner: key(ENGINE, "Actor")
            },
            name: "Hit".to_owned(),
            old_count: 1,
            new_count: 2
        }]
    );
    assert!(result.changed_types.is_empty());
}

#[test]
fn duplicate_in_an_ancestor_blocks_the_moved_verdict() {
    let (old, mut new) = character_chain();
    for _ in 0..2 {
        pawn_mut(&mut new)
            .functions
            .push(function("/Script/Engine.Pawn", "Jump", 0, Vec::new()));
    }
    let result = diff(&old, &new);
    // Pawn itself has two Jumps (new on that side), and Character's search
    // ran into the same duplicate and refused to choose.
    assert!(result.ambiguities.contains(&Ambiguity {
        subject: AmbiguousSubject::AncestorFunction {
            owner: key(ENGINE, "Pawn")
        },
        name: "Jump".to_owned(),
        old_count: 0,
        new_count: 2
    }));
    assert!(result.ambiguities.contains(&Ambiguity {
        subject: AmbiguousSubject::Function {
            owner: key(ENGINE, "Pawn")
        },
        name: "Jump".to_owned(),
        old_count: 0,
        new_count: 2
    }));
    assert!(changes_of(&result, "Character").is_empty());
}

#[test]
fn ambiguous_parent_path_stops_the_ancestor_search() {
    let (old, mut new) = character_chain();
    let mut twin = class(ENGINE, "PawnTwin", Some(ACTOR), 0x10);
    twin.path = "/Script/Engine.Pawn".to_owned();
    new.structs.push(twin);
    let result = diff(&old, &new);
    assert!(result.ambiguities.contains(&Ambiguity {
        subject: AmbiguousSubject::AncestorPath,
        name: "/Script/Engine.Pawn".to_owned(),
        old_count: 0,
        new_count: 2
    }));
    assert!(
        result
            .changed_types
            .iter()
            .all(|changed| changed.key.name != "Character")
    );
}

#[test]
fn duplicate_enum_and_enumerator_are_ambiguous() {
    let old = base_dump();
    let mut new = base_dump();
    new.enums.push(enumeration(ENGINE, "ENetRole", &[("X", 1)]));
    let result = diff(&old, &new);
    assert_eq!(
        result.ambiguities,
        [Ambiguity {
            subject: AmbiguousSubject::Enum,
            name: "/Script/Engine.ENetRole".to_owned(),
            old_count: 1,
            new_count: 2
        }]
    );
    assert!(result.changed_enums.is_empty());

    let mut new = base_dump();
    new.enums[0].values.push(crate::dump::EnumValue {
        name: "Authority".to_owned(),
        value: 77,
    });
    let result = diff(&old, &new);
    assert!(matches!(
        result.ambiguities.as_slice(),
        [Ambiguity {
            subject: AmbiguousSubject::EnumValue { .. },
            ..
        }]
    ));
    assert!(result.changed_enums.is_empty());
}

#[test]
fn ambiguities_are_not_repeated() {
    let (old, mut new) = character_chain();
    for _ in 0..2 {
        pawn_mut(&mut new)
            .functions
            .push(function("/Script/Engine.Pawn", "Jump", 0, Vec::new()));
    }
    // A second descendant looking for the same function hits the same duplicate.
    let mut other = old.structs.last().cloned().unwrap();
    other.name = "Walker".to_owned();
    other.path = "/Script/Engine.Walker".to_owned();
    let mut old = old;
    old.structs.push(other.clone());
    other.functions.clear();
    new.structs.push(other);
    let result = diff(&old, &new);
    let mut sorted = result.ambiguities.clone();
    sorted.sort();
    sorted.dedup();
    assert_eq!(result.ambiguities, sorted);
}

// ----- report -----

#[test]
fn display_of_an_empty_diff_says_so() {
    assert_eq!(DumpDiff::default().to_string(), "No differences.\n");
}

#[test]
fn display_names_every_kind_of_difference() {
    let (old, mut new) = character_chain();
    new.structs
        .retain(|def| def.name != "Pawn" || def.package != ENGINE);
    new.structs.push(class(ENGINE, "Brand", None, 8));
    actor_mut(&mut new).size = 0x120;
    actor_mut(&mut new).fields[0].offset = 0x28;
    function_mut(actor_mut(&mut new), "Hit").flags = FUNC_NET_CLIENT;
    new.enums[0].values[0].value = 5;
    new.enums.push(enumeration(ENGINE, "EOther", &[("A", 1)]));
    actor_mut(&mut new)
        .fields
        .push(field("Health", 0x70, PropertyType::Int32));

    let text = diff(&old, &new).to_string();
    for expected in [
        "Types added (1):",
        "Types removed (1):",
        "Types changed",
        "size: 0x100 -> 0x120",
        "field Hit",
        "NetServer -> NetClient",
        "Enums added (1):",
        "Enums changed (1):",
        "Ambiguous, not compared",
        "field Health in /Script/Engine.Actor: 1 in old, 2 in new",
    ] {
        // "field Hit" is not expected: the function line names the function.
        if expected == "field Hit" {
            assert!(text.contains("function Hit: network"), "{text}");
        } else {
            assert!(text.contains(expected), "missing {expected:?} in:\n{text}");
        }
    }
    assert!(text.ends_with('\n'));
}

#[test]
fn diff_serializes_to_json_and_back() {
    let old = base_dump();
    let mut new = base_dump();
    actor_mut(&mut new).size = 0x400;
    function_mut(actor_mut(&mut new), "Hit").params[0].ty = PropertyType::Double;
    let result = diff(&old, &new);
    let json = serde_json::to_string(&result).unwrap();
    assert_eq!(serde_json::from_str::<DumpDiff>(&json).unwrap(), result);
}
