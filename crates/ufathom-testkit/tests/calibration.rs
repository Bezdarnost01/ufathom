//! The measured builds: the image is walked with raw offsets from the
//! calibration table (never `Fixture::layout`), and `Fixture::layout` is
//! asserted against the same table.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::cast_possible_truncation,
    reason = "test code may panic on failure; clippy.toml only exempts #[test] functions, not helpers"
)]

mod common;

use common::{Calib, Raw, UE4_21, UE4_22, UE4_25};
use ufathom_mem::MemoryReaderExt;
use ufathom_model::flags::{
    CPF_RETURN_PARM, FUNC_BLUEPRINT_CALLABLE, FUNC_BLUEPRINT_EVENT, FUNC_NATIVE,
    RF_CLASS_DEFAULT_OBJECT,
};
use ufathom_model::layout::{NameTableLayout, ObjectArrayKind, PropertySystem, UEnumLayout};
use ufathom_testkit::{Fixture, FixtureBuilder, Profile};

const CPF_PARM: u64 = 0x80;

fn build(profile: Profile) -> Fixture {
    FixtureBuilder::new(profile)
        .seed(11)
        .filler_objects(1500)
        .build()
}

fn assert_layout_matches_table(fx: &Fixture, c: &Calib) {
    let l = &fx.layout;
    assert_eq!(u64::from(l.fname.size), c.fname_size, "{}", c.name);
    assert_eq!(l.fname.comparison_index, 0);
    assert_eq!(l.fname.number, Some(c.fname_number as u32));
    assert_eq!(l.fname.display_index.is_some(), c.fname_size == 12);
    if c.fname_size == 12 {
        assert_eq!(l.fname.display_index, Some(4));
    }
    assert_eq!(u64::from(l.uobject.flags), c.flags);
    assert_eq!(u64::from(l.uobject.index), c.index);
    assert_eq!(u64::from(l.uobject.class), c.class);
    assert_eq!(u64::from(l.uobject.name), c.obj_name);
    assert_eq!(u64::from(l.uobject.outer), c.outer);
    assert_eq!(u64::from(l.uobject.size), c.uobject_size);
    assert_eq!(u64::from(l.ufield.next), c.field_next);
    assert_eq!(u64::from(l.ustruct.super_struct), c.super_struct);
    assert_eq!(u64::from(l.ustruct.children), c.children);
    assert_eq!(
        l.ustruct.child_properties.map(u64::from),
        c.child_properties
    );
    assert_eq!(u64::from(l.ufunction.flags), c.func_flags);
    if c.name.starts_with("4.25") {
        // Only func is measured for 4.25; flags is inferred.
        assert_eq!(l.ufunction.native_func, Some(0xE0));
    } else {
        assert_eq!(l.ufunction.native_func.map(u64::from), Some(c.func_ptr));
    }
    assert_eq!(u64::from(l.properties.element_size), c.element_size);
    assert_eq!(u64::from(l.properties.offset_internal), c.prop_offset);
    assert_eq!(l.objects.item_size, 0x18);
    assert_eq!(
        l.objects.kind,
        ObjectArrayKind::Chunked {
            elements_per_chunk: 0x1_0000
        }
    );
    match (&l.properties.system, c.ffield) {
        (PropertySystem::Modern { ffield, .. }, Some((next, name))) => {
            assert_eq!(u64::from(ffield.next), next);
            assert_eq!(u64::from(ffield.name), name);
        }
        (PropertySystem::Legacy, None) => {}
        other => panic!("property system mismatch: {other:?}"),
    }
    match (&l.names, c.pool) {
        (NameTableLayout::Pool(pool), true) => {
            assert_eq!(pool.blocks, 0x10);
            assert_eq!(pool.stride, 4);
            assert_eq!(pool.entry.header_offset, 4);
            assert_eq!(pool.entry.string_offset, 6);
            assert_eq!(pool.entry.len_shift, 1);
        }
        (NameTableLayout::Array(_), false) => {}
        other => panic!("name table mismatch: {other:?}"),
    }
    if c.child_properties.is_none() {
        assert_eq!(u64::from(l.ustruct.properties_size), c.props_size);
    }
    if let Some(offset) = c.enum_names {
        match l.uenum {
            UEnumLayout::PairArray { offset: o, .. } => assert_eq!(u64::from(o), offset),
            UEnumLayout::SplitArrays { .. } => panic!("expected a pair array"),
        }
    }
    if let Some(size) = c.property_size {
        assert_eq!(u64::from(l.properties.size), size);
    }
    assert_eq!(
        l.uclass.cast_flags.map(u64::from),
        c.cast_flags.or(l.uclass.cast_flags.map(u64::from))
    );
    if let Some(cdo) = c.cdo {
        assert_eq!(l.uclass.default_object, Some(cdo as u32));
    }
    if let Some(index) = c.pe_index {
        assert_eq!(u64::from(fx.globals.process_event_index), index);
    }
}

fn walk(fx: &Fixture, c: &Calib) {
    let raw = Raw::new(fx, c);

    // The object array: every occupied slot holds an object that stores its own slot.
    let num = raw.num_elements();
    assert!(num >= fx.objects.len() as u64);
    assert!(num <= raw.max_elements());
    assert_eq!(raw.num_chunks(), num.div_ceil(0x1_0000));
    let mut occupied = 0;
    for i in 0..num {
        if let Some(object) = raw.object(i) {
            assert_eq!(
                u64::from(raw.u32(object + c.index)),
                i,
                "slot {i} stores its index"
            );
            occupied += 1;
        }
    }
    assert_eq!(occupied, fx.objects.len());

    // Names decoded by hand. "None" is index 0.
    assert_eq!(raw.name_at(0), "None");
    let byte_property = if c.pool { 3 } else { 1 };
    assert_eq!(raw.name_at(byte_property), "ByteProperty");

    // The class of Class is Class: the fixed point of the class chain.
    let class_class = raw
        .find(Some("/Script/CoreUObject"), "Class")
        .expect("Class exists");
    assert_eq!(raw.class_of(class_class), class_class);
    assert_eq!(raw.name_of(class_class), "Class");
    let actor = raw
        .find(Some("/Script/Engine"), "Actor")
        .expect("Actor exists");
    let mut chain = actor;
    for _ in 0..4 {
        chain = raw.class_of(chain);
    }
    assert_eq!(chain, class_class);
    let object_class = raw
        .find(Some("/Script/CoreUObject"), "Object")
        .expect("Object exists");
    assert_eq!(raw.u64(actor + c.super_struct), object_class);
    assert_eq!(raw.u64(object_class + c.super_struct), 0);

    // Packages have no outer.
    let package = raw.find(None, "/Script/Engine").expect("package exists");
    assert_eq!(raw.name_of(raw.class_of(package)), "Package");

    // Guid: size 0x10 and A, B, C, D at 0, 4, 8, 12.
    let guid = raw
        .find(Some("/Script/CoreUObject"), "Guid")
        .expect("Guid exists");
    assert_eq!(raw.u32(guid + c.props_size), 0x10);
    let members = raw.properties(guid);
    let names: Vec<&str> = members.iter().map(|(_, n)| n.as_str()).collect();
    assert_eq!(names, ["A", "B", "C", "D"]);
    for (i, (member, _)) in members.iter().enumerate() {
        assert_eq!(u64::from(raw.u32(member + c.prop_offset)), i as u64 * 4);
        assert_eq!(raw.u32(member + c.element_size), 4);
        assert_eq!(raw.u32(member + c.array_dim), 1);
    }
    let color = raw
        .find(Some("/Script/CoreUObject"), "Color")
        .expect("Color exists");
    assert_eq!(raw.u32(color + c.props_size), 4);
    let colors: Vec<String> = raw.properties(color).into_iter().map(|(_, n)| n).collect();
    assert_eq!(colors, ["B", "G", "R", "A"]);

    // Functions of Actor, with flags at the measured offset.
    let functions = raw.functions(actor);
    let find_fn = |name: &str| {
        functions
            .iter()
            .find(|(_, n)| n == name)
            .map(|&(a, _)| a)
            .unwrap_or_else(|| panic!("{name} exists"))
    };
    let destroy = find_fn("K2_DestroyActor");
    let flags = raw.u32(destroy + c.func_flags);
    assert_eq!(flags & FUNC_NATIVE, FUNC_NATIVE);
    assert_eq!(flags & FUNC_BLUEPRINT_CALLABLE, FUNC_BLUEPRINT_CALLABLE);
    let thunk = raw.u64(destroy + c.func_ptr);
    let text = fx
        .parts
        .iter()
        .find(|p| p.kind == ufathom_testkit::PartKind::Text)
        .expect("text");
    assert!(
        thunk >= text.addr && thunk < text.addr + text.bytes.len() as u64,
        "Func points into .text"
    );
    let begin_play = find_fn("ReceiveBeginPlay");
    let flags = raw.u32(begin_play + c.func_flags);
    assert_eq!(flags & FUNC_BLUEPRINT_EVENT, FUNC_BLUEPRINT_EVENT);
    assert_eq!(flags & FUNC_NATIVE, 0);
    assert_eq!(raw.u64(begin_play + c.func_ptr), 0);

    // ReturnValue of GetActorLocation is flagged Parm | ReturnParm; members are not.
    let get_location = find_fn("GetActorLocation");
    let params = raw.properties(get_location);
    let (ret, _) = params
        .iter()
        .find(|(_, n)| n == "ReturnValue")
        .expect("ReturnValue");
    let ret_flags = raw.u64(ret + c.prop_flags);
    assert_eq!(
        ret_flags & (CPF_PARM | CPF_RETURN_PARM),
        CPF_PARM | CPF_RETURN_PARM
    );
    for (member, name) in raw.properties(actor) {
        let member_flags = raw.u64(member + c.prop_flags);
        assert_eq!(
            member_flags & (CPF_PARM | CPF_RETURN_PARM),
            0,
            "{name} is a member"
        );
    }

    // Default objects carry RF_ClassDefaultObject; nothing else does.
    let cdo = raw
        .find(Some("/Script/Engine"), "Default__Actor")
        .expect("CDO");
    assert_ne!(raw.u32(cdo + c.flags) & RF_CLASS_DEFAULT_OBJECT, 0);
    let mut cdo_count = 0;
    for i in 0..num {
        if let Some(object) = raw.object(i) {
            let is_cdo = raw.u32(object + c.flags) & RF_CLASS_DEFAULT_OBJECT != 0;
            assert_eq!(
                is_cdo,
                raw.name_of(object).starts_with("Default__"),
                "slot {i}"
            );
            cdo_count += usize::from(is_cdo);
        }
    }
    assert!(cdo_count > 10);
    if let (Some(cdo_offset), Some(cast_offset)) = (c.cdo, c.cast_flags) {
        assert_eq!(raw.u64(actor + cdo_offset), cdo);
        assert_ne!(raw.u64(actor + cast_offset), 0);
    }

    // GWorld and GEngine point to instances, not default objects.
    let world = raw.u64(fx.anchors.data.gworld);
    assert_eq!(raw.name_of(raw.class_of(world)), "World");
    assert_eq!(raw.u32(world + c.flags) & RF_CLASS_DEFAULT_OBJECT, 0);
    let engine = raw.u64(fx.anchors.data.gengine);
    assert_eq!(raw.name_of(raw.class_of(engine)), "GameEngine");
    assert_eq!(raw.name_of(engine), "GameEngine_0");

    // Object 0's vtable: the ProcessEvent slot points at code with both
    // `test [reg+FunctionFlags], imm32` encodings; the other slots do not.
    let first = raw.object(0).expect("slot 0 is occupied");
    let vtable = raw.u64(first);
    let index = u64::from(fx.globals.process_event_index);
    let pe = raw.u64(vtable + index * 8);
    let disp = (c.func_flags as u32).to_le_bytes();
    let has = |at: u64, imm: [u8; 4]| {
        let mut bytes = [0u8; 128];
        let readable = (0..128)
            .rev()
            .find(|&n| fx.memory_read(at, &mut bytes[..n]).is_ok())
            .unwrap_or(0);
        // A function ends with `add rsp, 28h; ret`; do not read into the next one.
        let end = bytes[..readable]
            .windows(5)
            .position(|w| w == [0x48, 0x83, 0xC4, 0x28, 0xC3])
            .map_or(readable, |p| p + 5);
        bytes[..end].windows(10).any(|w| {
            w[0] == 0xF7 && (0x80..0x88).contains(&w[1]) && w[2..6] == disp && w[6..10] == imm
        })
    };
    assert!(has(pe, [0x00, 0x04, 0x00, 0x00]), "FUNC_Native test");
    assert!(has(pe, [0x00, 0x00, 0x40, 0x00]), "FUNC_HasOutParms test");
    for slot in 0..fx.profile.shape.vtable_slots {
        if u64::from(slot) == index {
            continue;
        }
        let target = raw.u64(vtable + u64::from(slot) * 8);
        assert!(
            !(has(target, [0, 4, 0, 0]) && has(target, [0, 0, 0x40, 0])),
            "slot {slot} must not look like ProcessEvent"
        );
    }
    assert_eq!(
        fx.memory
            .read_u64(fx.at_rva(fx.globals.gworld_rva))
            .unwrap(),
        world
    );
}

trait ReadHelper {
    fn memory_read(&self, addr: u64, buf: &mut [u8]) -> Result<(), ufathom_mem::MemError>;
}

impl ReadHelper for Fixture {
    fn memory_read(&self, addr: u64, buf: &mut [u8]) -> Result<(), ufathom_mem::MemError> {
        use ufathom_mem::MemoryReader;
        self.memory.read_exact(addr, buf)
    }
}

#[test]
fn ue4_21_measured_values() {
    let fx = build(Profile::ue4_21_measured());
    assert_layout_matches_table(&fx, &UE4_21);
    walk(&fx, &UE4_21);
}

#[test]
fn ue4_22_case_preserving_measured_values() {
    let fx = build(Profile::ue4_22_case_preserving_measured());
    assert_layout_matches_table(&fx, &UE4_22);
    walk(&fx, &UE4_22);
    // Typed payloads start at 0x80.
    let t = &fx.layout.properties.typed;
    let bool_layout = t.bool_property.as_ref().expect("bool");
    assert_eq!(
        [
            bool_layout.field_size,
            bool_layout.byte_offset,
            bool_layout.byte_mask,
            bool_layout.field_mask
        ],
        [0x80, 0x81, 0x82, 0x83]
    );
    assert_eq!(t.script_struct, Some(0x80));
    assert_eq!(t.array_inner, Some(0x80));
    assert_eq!(t.object_class, Some(0x80));
    assert_eq!((t.map_key, t.map_value), (Some(0x80), Some(0x88)));
    assert_eq!((t.enum_underlying, t.enum_type), (Some(0x80), Some(0x88)));
    assert_eq!(t.class_meta, Some(0x88));
    assert_eq!(fx.layout.uclass.cast_flags, Some(0xC0));
    assert_eq!(fx.layout.uclass.default_object, Some(0x108));
}

#[test]
fn ue4_25_case_preserving_measured_values() {
    let fx = build(Profile::ue4_25_case_preserving_measured());
    assert_layout_matches_table(&fx, &UE4_25);
    walk(&fx, &UE4_25);
}

#[test]
fn measured_profile_names_are_engine_versions_only() {
    for profile in [
        Profile::ue4_21_measured(),
        Profile::ue4_22_case_preserving_measured(),
        Profile::ue4_25_case_preserving_measured(),
        Profile::ue5_case_preserving_assumed(),
    ] {
        assert!(profile.name.starts_with("ue"), "{}", profile.name);
        assert!(profile.identity.product_version.starts_with(['4', '5']));
    }
}

#[test]
fn ue5_assumed_profile_is_double_based_with_a_64_bit_ffield() {
    let profile = Profile::ue5_case_preserving_assumed();
    assert!(profile.shape.vector_double);
    assert_eq!(profile.layout.fname.size, 12);
    let ffield = profile.shape.ffield.as_ref().expect("FField");
    assert_eq!(ffield.owner_is_uobject, None, "no owner bool");
    let fx = build(profile);
    for name in ["Vector", "Rotator", "Quat"] {
        let def = fx
            .truth
            .structs
            .iter()
            .find(|d| d.name == name)
            .unwrap_or_else(|| panic!("{name} exists"));
        assert!(
            def.fields
                .iter()
                .all(|f| f.ty == ufathom_model::PropertyType::Double),
            "{name} holds doubles"
        );
    }
}
