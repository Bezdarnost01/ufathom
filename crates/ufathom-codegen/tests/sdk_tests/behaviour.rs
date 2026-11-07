//! Runs inside a generated SDK: reads a synthetic object through the generated
//! types and checks wrappers against a test executor.

use std::cell::RefCell;

use ufathom_runtime::mem::Snapshot;
use ufathom_runtime::{CallError, Executor, IsA, ObjectRef, Ptr, UObjectBase};
use ufathom_sdk::core_u_object::FVector;
use ufathom_sdk::engine::{AActor, APawn, EMode};

const BASE: u64 = 0x7000_0000;

/// A pawn: health 75.5, `bCanTick` set, mode flying, location (1, 2, 3), speed 600.
fn pawn_bytes() -> Vec<u8> {
    let mut b = vec![0u8; 0x48];
    b[0x28..0x2C].copy_from_slice(&75.5f32.to_le_bytes());
    b[0x2C] = 0b10;
    b[0x2D] = 1;
    b[0x30..0x34].copy_from_slice(&1.0f32.to_le_bytes());
    b[0x34..0x38].copy_from_slice(&2.0f32.to_le_bytes());
    b[0x38..0x3C].copy_from_slice(&3.0f32.to_le_bytes());
    b[0x40..0x44].copy_from_slice(&600.0f32.to_le_bytes());
    b
}

fn memory() -> Snapshot {
    Snapshot::builder()
        .region(BASE, pawn_bytes())
        .build()
        .unwrap()
}

#[test]
fn layout_is_exact() {
    assert_eq!(size_of::<AActor>(), 0x40);
    assert_eq!(size_of::<APawn>(), 0x48);
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
fn unreadable_address_is_an_error() {
    let mem = memory();
    assert!(ObjectRef::<APawn>::new(&mem, 0x10).read().is_err());
    assert!(ObjectRef::<APawn>::new(&mem, BASE + 0x40).read().is_err());
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
    needs::<APawn, ufathom_sdk::core_u_object::UObject>();
    needs::<AActor, ufathom_sdk::core_u_object::UObject>();
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
        // Out parameter `OldHealth` at 8 and return value `ReturnValue` at 12.
        params[8..12].copy_from_slice(&25.0f32.to_le_bytes());
        params[12] = 1;
        Ok(())
    }
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
