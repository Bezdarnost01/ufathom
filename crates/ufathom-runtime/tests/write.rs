//! Field writes through `LiveObject` against the fake engine placed in this
//! process's own memory, which is what `SelfMemory` writes to.
#![cfg(feature = "inproc")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "tests may panic"
)]

mod common;

use common::{desc, image, object_addr, slot};
use ufathom_runtime::mem::{MemError, SelfMemory};
use ufathom_runtime::{BitRef, EngineDesc, FieldRef, IsA, Pod, Ptr, Session, UBool, WriteError};

/// Offset of `PropertiesSize` in a class object, from the fake description.
const PROPERTIES_SIZE_AT: u64 = 0x50;
/// `PropertiesSize` of the fake pawn class. The pawn objects are 0x100 apart.
const PAWN_SIZE: u32 = 0xa0;
/// `PropertiesSize` of the fake actor class, smaller than the pawn's.
const ACTOR_SIZE: u32 = 0x78;

/// A pawn-shaped type: the owner of the fields below.
struct Pawn;
// SAFETY: reflexive.
unsafe impl IsA<Pawn> for Pawn {}

/// A one-byte enum as generated code spells it.
#[repr(transparent)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Mode(u8);
// SAFETY: a transparent `u8`, valid for every bit pattern.
unsafe impl Pod for Mode {}

const HEALTH: FieldRef<Pawn, f32> = FieldRef::new(0x80);
const ALIVE: FieldRef<Pawn, UBool> = FieldRef::new(0x84);
const MODE: FieldRef<Pawn, Mode> = FieldRef::new(0x85);
const SCORE: FieldRef<Pawn, u64> = FieldRef::new(0x88);
const LAST_BYTES: FieldRef<Pawn, u32> = FieldRef::new(PAWN_SIZE - 4);
const PAST_END: FieldRef<Pawn, u32> = FieldRef::new(PAWN_SIZE - 3);
const FAR_AWAY: FieldRef<Pawn, u32> = FieldRef::new(u32::MAX);
const FLAGS_BYTE: u32 = 0x90;
const BIT_0: BitRef<Pawn> = BitRef::new(FLAGS_BYTE, 0x01);
const BIT_1: BitRef<Pawn> = BitRef::new(FLAGS_BYTE, 0x02);
const BIT_7: BitRef<Pawn> = BitRef::new(FLAGS_BYTE, 0x80);
const BITS_0_AND_2: BitRef<Pawn> = BitRef::new(FLAGS_BYTE, 0x05);
const NO_BIT: BitRef<Pawn> = BitRef::new(FLAGS_BYTE, 0);

struct World {
    session: Session<SelfMemory>,
    /// Keeps the fake engine alive; it is only ever touched through `session`.
    _memory: Vec<u64>,
    base: u64,
}

impl World {
    fn pawn(&self) -> Ptr<Pawn> {
        Ptr::new(object_addr(self.base, slot::MY_PAWN))
    }

    /// The address of the object array item for `index`.
    fn item(&self, index: u32) -> u64 {
        // The fake array is described by `desc`: items follow the chunk table.
        self.base + 0x880 + u64::from(index) * 24
    }
}

fn world() -> World {
    let mut memory = vec![0u64; common::SIZE / 8];
    let base = memory.as_ptr() as u64;
    let bytes = image(base, 0);
    for (word, chunk) in memory.iter_mut().zip(bytes.chunks_exact(8)) {
        *word = u64::from_le_bytes(chunk.try_into().unwrap());
    }
    // Give the pawn and actor classes a size: the fake image only sizes the function.
    let size_word = |memory: &mut Vec<u64>, class: u32, size: u32| {
        let at = object_addr(base, class) - base + PROPERTIES_SIZE_AT;
        memory[(at / 8) as usize] = u64::from(size);
    };
    size_word(&mut memory, slot::PAWN, PAWN_SIZE);
    size_word(&mut memory, slot::ACTOR, ACTOR_SIZE);
    let d: &'static EngineDesc = Box::leak(Box::new(desc(base)));
    let session = Session::new(SelfMemory::new(), d).unwrap();
    World {
        session,
        _memory: memory,
        base,
    }
}

#[test]
fn set_writes_a_field_and_leaves_its_neighbours() {
    let world = world();
    let pawn = world.session.live_object(world.pawn());
    pawn.set(SCORE, 0x1122_3344_5566_7788).unwrap();
    pawn.set(HEALTH, 75.5).unwrap();
    let read = pawn.read_only();
    assert_eq!(read.get(HEALTH).unwrap(), 75.5);
    assert_eq!(read.get(SCORE).unwrap(), 0x1122_3344_5566_7788);
    // Neighbours of the f32 were not touched.
    assert_eq!(read.get(ALIVE).unwrap(), UBool(0));
    assert_eq!(read.get(MODE).unwrap(), Mode(0));
}

#[test]
fn bool_and_enum_newtypes_write_their_raw_representation() {
    let world = world();
    let pawn = world.session.live_object(world.pawn());
    // A byte that is neither 0 nor 1 survives the round trip unchanged.
    pawn.set(ALIVE, UBool(0x80)).unwrap();
    pawn.set(MODE, Mode(7)).unwrap();
    let read = pawn.read_only();
    assert_eq!(read.get(ALIVE).unwrap(), UBool(0x80));
    assert!(read.get(ALIVE).unwrap().get());
    assert_eq!(read.get(MODE).unwrap(), Mode(7));
    pawn.set(ALIVE, UBool::from(true)).unwrap();
    assert_eq!(read.get(ALIVE).unwrap(), UBool(1));
    pawn.set(ALIVE, UBool::from(false)).unwrap();
    assert_eq!(read.get(ALIVE).unwrap(), UBool(0));
}

#[test]
fn modify_reads_changes_writes_and_returns_the_old_value() {
    let world = world();
    let pawn = world.session.live_object(world.pawn());
    pawn.set(HEALTH, 100.0).unwrap();
    let before = pawn.modify(HEALTH, |health| *health *= 1.1).unwrap();
    assert_eq!(before, 100.0);
    let after = pawn.read_only().get(HEALTH).unwrap();
    assert!((after - 110.0).abs() < 0.001, "{after}");
}

#[test]
fn modify_does_not_run_the_closure_for_a_dead_object() {
    let world = world();
    let pawn = world
        .session
        .live_object(Ptr::<Pawn>::new(object_addr(world.base, 1) + 8));
    let mut ran = false;
    let result = pawn.modify(HEALTH, |_| ran = true);
    assert!(matches!(result, Err(WriteError::DeadObject { .. })));
    assert!(!ran);
}

#[test]
fn set_bit_changes_only_the_masked_bit_of_one_byte() {
    let world = world();
    let pawn = world.session.live_object(world.pawn());
    let read = pawn.read_only();
    let byte = |expected: u8| {
        let at = FieldRef::<Pawn, u8>::new(FLAGS_BYTE);
        assert_eq!(read.get(at).unwrap(), expected, "flags byte");
    };
    // Surrounding bytes must stay as they were.
    pawn.set(FieldRef::<Pawn, u8>::new(FLAGS_BYTE), 0b1010_0100)
        .unwrap();
    pawn.set(FieldRef::<Pawn, u8>::new(FLAGS_BYTE - 1), 0xaa)
        .unwrap();
    pawn.set(FieldRef::<Pawn, u8>::new(FLAGS_BYTE + 1), 0x55)
        .unwrap();

    pawn.set_bit(BIT_1, true).unwrap();
    byte(0b1010_0110);
    pawn.set_bit(BIT_1, true).unwrap();
    byte(0b1010_0110);
    pawn.set_bit(BIT_7, false).unwrap();
    byte(0b0010_0110);
    pawn.set_bit(BIT_0, false).unwrap();
    byte(0b0010_0110);
    pawn.set_bit(BIT_0, true).unwrap();
    byte(0b0010_0111);
    // A mask with several bits sets or clears all of them.
    pawn.set_bit(BITS_0_AND_2, false).unwrap();
    byte(0b0010_0010);
    pawn.set_bit(BITS_0_AND_2, true).unwrap();
    byte(0b0010_0111);

    assert!(read.get_bit(BIT_1).unwrap());
    assert!(!read.get_bit(BIT_7).unwrap());
    assert_eq!(
        read.get(FieldRef::<Pawn, u8>::new(FLAGS_BYTE - 1)).unwrap(),
        0xaa
    );
    assert_eq!(
        read.get(FieldRef::<Pawn, u8>::new(FLAGS_BYTE + 1)).unwrap(),
        0x55
    );
    // No bit selected: refused, nothing written.
    assert!(matches!(
        pawn.set_bit(NO_BIT, true),
        Err(WriteError::EmptyMask)
    ));
    byte(0b0010_0111);
}

#[test]
fn a_cleared_slot_is_a_dead_object() {
    let world = world();
    let pawn = world.session.live_object(world.pawn());
    pawn.set(HEALTH, 1.0).unwrap();
    // The slot is emptied, as when the engine frees the object.
    world
        .session
        .memory()
        .write_exact(world.item(slot::MY_PAWN), &0u64.to_le_bytes())
        .unwrap();
    let error = pawn.set(HEALTH, 2.0).unwrap_err();
    assert!(
        matches!(error, WriteError::DeadObject { addr } if addr == pawn.addr()),
        "{error}"
    );
    assert!(matches!(
        pawn.set_bit(BIT_0, true),
        Err(WriteError::DeadObject { .. })
    ));
    assert!(matches!(
        pawn.modify(SCORE, |score| *score += 1),
        Err(WriteError::DeadObject { .. })
    ));
    // Nothing was written.
    assert_eq!(pawn.read_only().get(HEALTH).unwrap(), 1.0);
}

#[test]
fn a_reused_slot_is_a_dead_object() {
    let world = world();
    let pawn = world.session.live_object(world.pawn());
    pawn.set(HEALTH, 1.0).unwrap();
    // Another object now occupies the pawn's slot.
    let other = object_addr(world.base, slot::ACTOR_1);
    world
        .session
        .memory()
        .write_exact(world.item(slot::MY_PAWN), &other.to_le_bytes())
        .unwrap();
    assert!(matches!(
        pawn.set(HEALTH, 2.0),
        Err(WriteError::DeadObject { .. })
    ));
    assert_eq!(pawn.read_only().get(HEALTH).unwrap(), 1.0);
}

#[test]
fn null_and_wild_pointers_are_dead_objects() {
    let world = world();
    for addr in [0u64, 8, 0xdead_0000, u64::MAX - 7] {
        let object = world.session.live_object(Ptr::<Pawn>::new(addr));
        assert!(matches!(
            object.set(HEALTH, 2.0),
            Err(WriteError::DeadObject { .. })
        ));
    }
}

#[test]
fn a_field_outside_the_object_is_refused() {
    let world = world();
    let pawn = world.session.live_object(world.pawn());
    // The last four bytes of the class are fine.
    pawn.set(LAST_BYTES, 0xfeed_beef).unwrap();
    assert_eq!(pawn.read_only().get(LAST_BYTES).unwrap(), 0xfeed_beef);
    // One byte further spills past `PropertiesSize`.
    let error = pawn.set(PAST_END, 1).unwrap_err();
    assert!(
        matches!(
            error,
            WriteError::OutOfBounds {
                offset,
                size: 4,
                class_size,
                ..
            } if offset == PAWN_SIZE - 3 && class_size == PAWN_SIZE as usize
        ),
        "{error}"
    );
    assert!(matches!(
        pawn.set(FAR_AWAY, 1),
        Err(WriteError::OutOfBounds { .. })
    ));
    assert!(matches!(
        pawn.set_bit(BitRef::<Pawn>::new(PAWN_SIZE, 1), true),
        Err(WriteError::OutOfBounds { .. })
    ));
    assert!(matches!(
        pawn.modify(PAST_END, |value| *value = 1),
        Err(WriteError::OutOfBounds { .. })
    ));
}

#[test]
fn the_class_of_the_live_object_sets_the_bounds_not_the_static_type() {
    let world = world();
    // An actor, viewed as a `Pawn`: `SCORE` is inside a pawn but past an
    // actor's `PropertiesSize`.
    let actor = world
        .session
        .live_object(Ptr::<Pawn>::new(object_addr(world.base, slot::ACTOR_1)));
    assert!(matches!(
        actor.set(SCORE, 1),
        Err(WriteError::OutOfBounds {
            class_size,
            ..
        }) if class_size == ACTOR_SIZE as usize
    ));
    // A field inside the actor is accepted.
    actor.set(FieldRef::<Pawn, u32>::new(0x70), 5).unwrap();
}

#[test]
fn write_errors_name_their_cause() {
    assert!(
        WriteError::DeadObject { addr: 0x10 }
            .to_string()
            .contains("0x10")
    );
    let memory = WriteError::from(MemError::NotWritable { addr: 0x20, len: 4 });
    assert!(matches!(
        memory,
        WriteError::Memory(MemError::NotWritable { .. })
    ));
    assert!(memory.to_string().contains("0x20"));
}
