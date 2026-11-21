//! Runs inside a generated SDK: reads a synthetic object through the generated
//! types and checks wrappers against a test executor.

use std::cell::RefCell;

use ufathom_runtime::mem::Snapshot;
use ufathom_runtime::{CallError, ChainError, Executor, IsA, ObjectRef, Ptr, UObjectBase};
use ufathom_sdk::core_uobject::{FVector, UObject, UObjectFields};
use ufathom_sdk::engine::{AActor, AActorFields, APawn, APawnFields, EMode};

const BASE: u64 = 0x7000_0000;
/// Where `second_pawn_bytes` sits.
const SECOND: u64 = BASE + 0x100;

/// A pawn: health 75.5, `bCanTick` set, mode flying, location (1, 2, 3), speed
/// 600. Its `Target` is the second pawn and its `Owner` is null.
fn pawn_bytes() -> Vec<u8> {
    let mut b = vec![0u8; 0x58];
    // The `UObject` header, from the layout: index, class, name, outer.
    b[0x0C..0x10].copy_from_slice(&77i32.to_le_bytes());
    b[0x10..0x18].copy_from_slice(&0x1234u64.to_le_bytes());
    b[0x18..0x1C].copy_from_slice(&5u32.to_le_bytes());
    b[0x1C..0x20].copy_from_slice(&2u32.to_le_bytes());
    b[0x20..0x28].copy_from_slice(&0x5678u64.to_le_bytes());
    b[0x28..0x2C].copy_from_slice(&75.5f32.to_le_bytes());
    b[0x2C] = 0b10;
    b[0x2D] = 1;
    b[0x30..0x34].copy_from_slice(&1.0f32.to_le_bytes());
    b[0x34..0x38].copy_from_slice(&2.0f32.to_le_bytes());
    b[0x38..0x3C].copy_from_slice(&3.0f32.to_le_bytes());
    b[0x40..0x44].copy_from_slice(&600.0f32.to_le_bytes());
    b[0x48..0x50].copy_from_slice(&SECOND.to_le_bytes());
    b
}

/// A second pawn: health 12.5, speed 1.5, no target, and the first pawn as owner.
fn second_pawn_bytes() -> Vec<u8> {
    let mut b = vec![0u8; 0x58];
    b[0x28..0x2C].copy_from_slice(&12.5f32.to_le_bytes());
    b[0x40..0x44].copy_from_slice(&1.5f32.to_le_bytes());
    b[0x50..0x58].copy_from_slice(&BASE.to_le_bytes());
    b
}

fn memory() -> Snapshot {
    Snapshot::builder()
        .region(BASE, pawn_bytes())
        .region(SECOND, second_pawn_bytes())
        .build()
        .unwrap()
}

#[test]
fn layout_is_exact() {
    assert_eq!(size_of::<AActor>(), 0x40);
    assert_eq!(size_of::<APawn>(), 0x58);
    assert_eq!(align_of::<APawn>(), 8);
    assert_eq!(size_of::<FVector>(), 12);
}

#[test]
fn fields_are_read_through_object_ref_and_field_ref() {
    let mem = memory();
    let pawn = ObjectRef::<APawn>::new(&mem, BASE);
    // A field of the pawn itself and fields declared on its parent.
    assert_eq!(pawn.get(APawn::SPEED).unwrap(), 600.0);
    assert_eq!(pawn.get(AActor::HEALTH).unwrap(), 75.5);
    assert_eq!(pawn.get(AActor::MODE).unwrap(), EMode::Flying);
    assert_eq!(pawn.get(AActor::MODE).unwrap().name(), Some("Flying"));
    let location = pawn.get(AActor::LOCATION).unwrap();
    assert_eq!((location.x, location.y, location.z), (1.0, 2.0, 3.0));
    // Bit-field booleans.
    assert!(pawn.get_bit(AActor::B_CAN_TICK).unwrap());
    assert!(!pawn.get_bit(AActor::B_HIDDEN).unwrap());
    // Viewing the same object as its parent.
    assert_eq!(pawn.upcast::<AActor>().get(AActor::HEALTH).unwrap(), 75.5);
}

#[test]
fn the_uobject_header_comes_from_the_layout() {
    let mem = memory();
    let pawn = ObjectRef::<APawn>::new(&mem, BASE);
    assert_eq!(pawn.get(UObject::INTERNAL_INDEX).unwrap(), 77);
    assert_eq!(pawn.get(UObject::CLASS_PRIVATE).unwrap().addr(), 0x1234);
    assert_eq!(pawn.get(UObject::OUTER_PRIVATE).unwrap().addr(), 0x5678);
    let name = pawn.get(UObject::NAME_PRIVATE).unwrap();
    assert_eq!((name.comparison_index, name.number), (5, 2));
    let whole: APawn = pawn.read().unwrap();
    assert_eq!(whole.internal_index, 77);
}

#[test]
fn unreadable_address_is_an_error() {
    let mem = memory();
    assert!(ObjectRef::<APawn>::new(&mem, 0x10).read().is_err());
    assert!(ObjectRef::<APawn>::new(&mem, BASE + 0x40).read().is_err());
}

#[test]
fn chains_read_two_levels_through_pointer_fields() {
    let mem = memory();
    let pawn = ObjectRef::<APawn>::new(&mem, BASE);
    // One link: the pointer field is followed to an object of its class.
    assert_eq!(pawn.target().unwrap().addr(), SECOND);
    assert_eq!(pawn.target().unwrap().speed().unwrap(), 1.5);
    // Two links, the second a field of the parent class, through the child.
    assert_eq!(pawn.target().unwrap().health().unwrap(), 12.5);
    // Three links, through a pointer to the parent class and back.
    let owner = pawn.target().unwrap().owner().unwrap();
    assert_eq!(owner.addr(), BASE);
    assert_eq!(owner.health().unwrap(), 75.5);
    // The same chain written with `?`.
    fn chain(pawn: ObjectRef<'_, APawn>) -> Result<f32, ChainError> {
        Ok(pawn.target()?.owner()?.health()?)
    }
    assert_eq!(chain(pawn).unwrap(), 75.5);
}

#[test]
fn inherited_fields_are_read_through_a_child() {
    let mem = memory();
    let pawn = ObjectRef::<APawn>::new(&mem, BASE);
    // Declared on `AActor`, read on an `APawn` handle.
    assert_eq!(pawn.health().unwrap(), 75.5);
    assert_eq!(pawn.mode().unwrap(), EMode::Flying);
    assert_eq!(pawn.location().unwrap().z, 3.0);
    assert!(pawn.b_can_tick().unwrap());
    assert!(!pawn.b_hidden().unwrap());
    // Declared on `UObject`, two levels up.
    assert_eq!(pawn.internal_index().unwrap(), 77);
    assert_eq!(pawn.class_private().unwrap().addr(), 0x1234);
    // The parent's own handle reads the same field.
    assert_eq!(pawn.upcast::<AActor>().health().unwrap(), 75.5);
}

#[test]
fn a_null_pointer_in_a_chain_names_the_field() {
    let mem = memory();
    let pawn = ObjectRef::<APawn>::new(&mem, BASE);
    // `Owner` is null on the first pawn.
    match pawn.owner() {
        Err(ChainError::Null { field }) => assert_eq!(field, "APawn::Owner"),
        other => panic!("expected a null error, got {other:?}"),
    }
    // The chain breaks at the second link; the first succeeded.
    fn chain(pawn: ObjectRef<'_, APawn>) -> Result<f32, ChainError> {
        Ok(pawn.target()?.target()?.health()?)
    }
    let error = chain(pawn).unwrap_err();
    assert!(matches!(error, ChainError::Null { field: "APawn::Target" }));
    assert_eq!(
        error.to_string(),
        "null object pointer in field APawn::Target"
    );
}

#[test]
fn a_wild_pointer_in_a_chain_is_a_memory_error() {
    let mut bytes = pawn_bytes();
    bytes[0x50..0x58].copy_from_slice(&0xDEAD_0000u64.to_le_bytes());
    let mem = Snapshot::builder().region(BASE, bytes).build().unwrap();
    let pawn = ObjectRef::<APawn>::new(&mem, BASE);
    // Following a pointer reads only the pointer; the target is read next.
    let owner = pawn.owner().unwrap();
    assert_eq!(owner.addr(), 0xDEAD_0000);
    assert!(matches!(owner.health(), Err(ufathom_runtime::MemError::Unreadable { .. })));
}

#[test]
fn whole_object_read_has_working_accessors_and_deref() {
    let mem = memory();
    let mut pawn: APawn = ObjectRef::<APawn>::new(&mem, BASE).read().unwrap();
    // `Deref` to the parent class.
    assert_eq!(pawn.health, 75.5);
    assert_eq!(pawn.speed, 600.0);
    assert!(pawn.b_can_tick());
    assert!(!pawn.b_hidden());
    pawn.set_b_hidden(true);
    pawn.set_b_can_tick(false);
    assert!(pawn.b_hidden());
    assert!(!pawn.b_can_tick());
    assert_eq!(pawn.mode, EMode::Flying);
    assert_eq!(pawn.mode, EMode(1));
    assert_eq!(format!("{:?}", EMode(9)), "EMode(9)");
}

#[test]
fn is_a_covers_self_and_ancestors() {
    fn needs<T: IsA<U>, U>() {}
    needs::<APawn, APawn>();
    needs::<APawn, AActor>();
    needs::<APawn, ufathom_sdk::core_uobject::UObject>();
    needs::<AActor, ufathom_sdk::core_uobject::UObject>();
}

struct Recorder {
    calls: RefCell<Vec<(u64, String, Vec<u8>)>>,
    fail: bool,
}

// SAFETY: a test double that never touches a game.
unsafe impl Executor for Recorder {
    fn process_event(
        &self,
        object: Ptr<UObjectBase>,
        function_path: &str,
        params: &mut [u8],
    ) -> Result<(), CallError> {
        if self.fail {
            return Err(CallError::Refused("test".to_owned()));
        }
        self.calls
            .borrow_mut()
            .push((object.addr(), function_path.to_owned(), params.to_vec()));
        if params.len() == 4 {
            // `GetActorCount`: the return value at 0.
            params.copy_from_slice(&7i32.to_le_bytes());
        } else {
            // Out parameter `OldHealth` at 8 and return value `ReturnValue` at 12.
            params[8..12].copy_from_slice(&25.0f32.to_le_bytes());
            params[12] = 1;
        }
        Ok(())
    }

    fn default_object(&self, class_path: &str) -> Result<Ptr<UObjectBase>, CallError> {
        assert_eq!(class_path, "/Script/Engine.Actor");
        Ok(Ptr::new(CDO))
    }
}

const CDO: u64 = 0xCD0;

#[test]
fn static_wrapper_runs_on_the_class_default_object() {
    let exec = Recorder {
        calls: RefCell::new(Vec::new()),
        fail: false,
    };
    assert_eq!(AActor::get_actor_count(&exec).unwrap(), 7);
    let calls = exec.calls.borrow();
    assert_eq!(calls[0].0, CDO);
    assert_eq!(calls[0].1, "/Script/Engine.Actor:GetActorCount");
}

#[test]
fn wrapper_fills_the_block_and_returns_results() {
    let exec = Recorder {
        calls: RefCell::new(Vec::new()),
        fail: false,
    };
    let (ok, old) = AActor::set_health(Ptr::new(BASE), &exec, 50.0, true).unwrap();
    assert!(ok);
    assert_eq!(old, 25.0);
    let calls = exec.calls.borrow();
    let (object, path, block) = &calls[0];
    assert_eq!(*object, BASE);
    assert_eq!(path, "/Script/Engine.Actor:SetHealth");
    assert_eq!(block.len(), 0x10);
    assert_eq!(&block[0..4], &50.0f32.to_le_bytes());
    assert_eq!(block[4], 1);
    // Out parameter and return value start zeroed.
    assert_eq!(&block[8..13], &[0; 5]);
}

#[test]
fn wrapper_reports_an_executor_error() {
    let exec = Recorder {
        calls: RefCell::new(Vec::new()),
        fail: true,
    };
    let result = AActor::set_health(Ptr::new(BASE), &exec, 1.0, false);
    assert!(matches!(result, Err(CallError::Refused(_))));
}

/// Each function names itself by its parameter struct, with the path its
/// wrapper calls and the `FUNC_Native` flag from the dump.
#[cfg(feature = "inproc")]
#[test]
fn typed_hook_markers_carry_the_path_and_the_native_flag() {
    use ufathom_runtime::inproc::{GameFunction, NativeFunction};
    use ufathom_sdk::engine::{AActor_GetActorCount_Params, AActor_SetHealth_Params};

    fn requires_native<F: NativeFunction>() -> &'static str {
        F::PATH
    }

    assert_eq!(
        <AActor_SetHealth_Params as GameFunction>::PATH,
        "/Script/Engine.Actor:SetHealth"
    );
    assert!(<AActor_SetHealth_Params as GameFunction>::IS_NATIVE);
    // Compiles only because the dump marks the function native.
    assert_eq!(
        requires_native::<AActor_SetHealth_Params>(),
        "/Script/Engine.Actor:SetHealth"
    );
    assert_eq!(
        <AActor_GetActorCount_Params as GameFunction>::PATH,
        "/Script/Engine.Actor:GetActorCount"
    );
    assert!(!<AActor_GetActorCount_Params as GameFunction>::IS_NATIVE);
    assert_eq!(
        size_of::<<AActor_SetHealth_Params as GameFunction>::Params>(),
        0x10
    );
}

/// Writes through generated `FIELD` constants into a fake engine placed in
/// this process's memory, then reads the result back through the same
/// constants.
#[cfg(feature = "inproc")]
#[test]
fn fields_are_written_through_live_object_and_read_back() {
    use ufathom_runtime::mem::SelfMemory;
    use ufathom_runtime::{
        EngineDesc, FNameDesc, LiveObject, NamePoolDesc, NamesDesc, ObjectArrayDesc, ObjectDesc,
        Place, Session, WriteError,
    };
    use ufathom_sdk::engine::{AActorLiveFields, APawnLiveFields};

    const ARRAY: u64 = 0x00;
    const ITEMS: u64 = 0x40;
    const PAWN_AT: u64 = 0x100;
    const CLASS_AT: u64 = 0x200;
    const ITEM_SIZE: u64 = 16;

    let mut words = vec![0u64; 0x400 / 8];
    let base = words.as_ptr() as u64;
    let mut bytes = vec![0u8; 0x400];
    let mut put = |at: u64, data: &[u8]| {
        bytes[at as usize..at as usize + data.len()].copy_from_slice(data);
    };
    // Object array: flat storage with two items, count 2.
    put(ARRAY, &(base + ITEMS).to_le_bytes());
    put(ARRAY + 8, &2u32.to_le_bytes());
    put(ITEMS, &(base + CLASS_AT).to_le_bytes());
    put(ITEMS + ITEM_SIZE, &(base + PAWN_AT).to_le_bytes());
    // The pawn is in slot 1 and its class is in slot 0.
    let mut pawn = pawn_bytes();
    // The pawn is its own target, so a live chain can stay inside the fake engine.
    pawn[0x48..0x50].copy_from_slice(&(base + PAWN_AT).to_le_bytes());
    pawn[0x0C..0x10].copy_from_slice(&1u32.to_le_bytes());
    pawn[0x10..0x18].copy_from_slice(&(base + CLASS_AT).to_le_bytes());
    put(PAWN_AT, &pawn);
    // The class: slot 0, `PropertiesSize` (0x40 in this layout) of the pawn.
    put(CLASS_AT + 0x0C, &0u32.to_le_bytes());
    put(CLASS_AT + 0x40, &0x58u32.to_le_bytes());
    for (word, chunk) in words.iter_mut().zip(bytes.chunks_exact(8)) {
        *word = u64::from_le_bytes(chunk.try_into().unwrap());
    }
    let desc: &'static EngineDesc = Box::leak(Box::new(EngineDesc {
        names: NamesDesc::Pool(NamePoolDesc {
            location: Place::Absolute(base),
            blocks: 0x10,
            stride: 2,
            block_offset_bits: 16,
            header_offset: 0,
            string_offset: 2,
            len_shift: 6,
            wide_mask: 1,
        }),
        objects: ObjectArrayDesc {
            location: Place::Absolute(base + ARRAY),
            elements_per_chunk: None,
            objects: 0,
            num_elements: 8,
            item_size: ITEM_SIZE as u32,
            item_object: 0,
            item_serial: None,
            pointer_transform: &[],
        },
        fname: FNameDesc {
            size: 8,
            comparison_index: 0,
            display_index: None,
            number: Some(4),
        },
        object: ObjectDesc {
            flags: 8,
            index: 0x0C,
            class: 0x10,
            name: 0x18,
            outer: 0x20,
            super_struct: 0x30,
            properties_size: 0x40,
            default_object: None,
        },
        process_event_index: None,
        gworld: None,
        gengine: None,
    }));
    let session = Session::new(SelfMemory::new(), desc).unwrap();
    let pawn = session.live_object(Ptr::<APawn>::new(base + PAWN_AT));

    // A field of the pawn itself and fields declared on its parent.
    pawn.set(APawn::SPEED, 900.0).unwrap();
    pawn.set(AActor::HEALTH, 110.0).unwrap();
    pawn.set(AActor::MODE, EMode(1)).unwrap();
    pawn.set(AActor::LOCATION, FVector { x: 4.0, y: 5.0, z: 6.0 })
        .unwrap();
    pawn.set_bit(AActor::B_HIDDEN, true).unwrap();
    pawn.set_bit(AActor::B_CAN_TICK, false).unwrap();
    let old = pawn.modify(AActor::HEALTH, |health| *health += 1.0).unwrap();
    assert_eq!(old, 110.0);

    let read = session.object(Ptr::<APawn>::new(base + PAWN_AT));
    assert_eq!(read.get(APawn::SPEED).unwrap(), 900.0);
    assert_eq!(read.get(AActor::HEALTH).unwrap(), 111.0);
    assert_eq!(read.get(AActor::MODE).unwrap(), EMode(1));
    let location = read.get(AActor::LOCATION).unwrap();
    assert_eq!((location.x, location.y, location.z), (4.0, 5.0, 6.0));
    assert!(read.get_bit(AActor::B_HIDDEN).unwrap());
    assert!(!read.get_bit(AActor::B_CAN_TICK).unwrap());

    // The same writes through the typed accessors, inherited ones included.
    pawn.set_speed(950.0).unwrap();
    pawn.set_health(120.0).unwrap();
    pawn.set_mode(EMode(2)).unwrap();
    pawn.set_location(FVector { x: 7.0, y: 8.0, z: 9.0 }).unwrap();
    pawn.set_b_hidden(false).unwrap();
    pawn.set_b_can_tick(true).unwrap();
    assert_eq!(read.speed().unwrap(), 950.0);
    assert_eq!(read.health().unwrap(), 120.0);
    assert_eq!(read.mode().unwrap(), EMode(2));
    assert_eq!(read.location().unwrap().y, 8.0);
    assert!(!read.b_hidden().unwrap());
    assert!(read.b_can_tick().unwrap());

    // A write chain: follow `Target` (the pawn itself) and write there.
    pawn.target().unwrap().set_health(33.0).unwrap();
    assert_eq!(read.target().unwrap().health().unwrap(), 33.0);
    assert_eq!(read.health().unwrap(), 33.0);
    // A null link reports the field, and `ChainError` takes a `WriteError` too.
    assert!(matches!(
        pawn.owner(),
        Err(ChainError::Null { field: "APawn::Owner" })
    ));
    fn chain(pawn: LiveObject<'_, APawn>) -> Result<(), ChainError> {
        pawn.target()?.set_speed(5.0)?;
        pawn.owner()?.set_health(6.0)?;
        Ok(())
    }
    assert!(matches!(chain(pawn), Err(ChainError::Null { .. })));
    assert_eq!(read.speed().unwrap(), 5.0);
    // A dead object is refused by the typed setter too.
    let ghost = session.live_object(Ptr::<APawn>::new(base + PAWN_AT + 8));
    assert!(matches!(
        ghost.set_health(1.0),
        Err(WriteError::DeadObject { .. })
    ));

    // An object that is not in the array is refused and nothing is written.
    let stray = session.live_object(Ptr::<APawn>::new(base + PAWN_AT + 8));
    assert!(matches!(
        stray.set(AActor::HEALTH, 1.0),
        Err(WriteError::DeadObject { .. })
    ));
    drop(words);
}
