//! `Session` over a snapshot of a fake engine.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "tests may panic"
)]

mod common;

use common::types::{AActor, AMissing, APawn, UObject};
use common::{desc, image, object_addr, slot};
use ufathom_runtime::mem::Snapshot;
use ufathom_runtime::{EngineDesc, Ptr, Session, SessionError, UObjectBase};

const BASE: u64 = 0x7000_0000;

fn session() -> Session<Snapshot> {
    let mem = Snapshot::builder()
        .region(BASE, image(BASE, 0))
        .build()
        .unwrap();
    let desc: &'static EngineDesc = Box::leak(Box::new(desc(BASE)));
    Session::new(mem, desc).unwrap()
}

fn obj(index: u32) -> Ptr<UObjectBase> {
    Ptr::new(object_addr(BASE, index))
}

#[test]
fn walks_the_object_array() {
    let s = session();
    assert_eq!(s.num_objects().unwrap(), slot::COUNT);
    let all = s.objects().unwrap();
    assert_eq!(all.len(), slot::COUNT as usize);
    assert_eq!(all[9], obj(9), "second chunk is reached");
    assert_eq!(s.object_at(slot::PAWN).unwrap(), Some(obj(slot::PAWN)));
    assert_eq!(s.object_at(slot::COUNT).unwrap(), None);
    assert_eq!(s.serial_at(slot::PAWN).unwrap(), Some(100 + slot::PAWN));
}

#[test]
fn names_paths_and_full_names() {
    let s = session();
    assert_eq!(s.name_of(obj(slot::ACTOR)).unwrap(), "Actor");
    assert_eq!(s.name_of(obj(slot::ACTOR_1)).unwrap(), "Actor_1");
    assert_eq!(s.path_of(obj(slot::ENGINE)).unwrap(), "/Script/Engine");
    assert_eq!(s.path_of(obj(slot::ACTOR)).unwrap(), "/Script/Engine.Actor");
    assert_eq!(
        s.path_of(obj(slot::DESTROY)).unwrap(),
        "/Script/Engine.Actor:K2_DestroyActor"
    );
    assert_eq!(
        s.full_name_of(obj(slot::DESTROY)).unwrap(),
        "Function /Script/Engine.Actor:K2_DestroyActor"
    );
    assert_eq!(
        s.full_name_of(obj(slot::MY_PAWN)).unwrap(),
        "Pawn /Script/Engine.MyPawn"
    );
}

#[test]
fn finds_objects_by_path_and_name() {
    let s = session();
    let path = "/Script/Engine.Actor:K2_DestroyActor";
    assert_eq!(s.find_object(path).unwrap(), Some(obj(slot::DESTROY)));
    // A second lookup is served from the cache and gives the same answer.
    assert_eq!(s.find_object(path).unwrap(), Some(obj(slot::DESTROY)));
    assert_eq!(s.find_object("/Script/Engine.Nothing").unwrap(), None);
    assert_eq!(
        s.find_by_name("MyPawn", None).unwrap(),
        Some(obj(slot::MY_PAWN))
    );
    assert_eq!(
        s.find_by_name("Actor_1", Some("/Script/Engine.Actor"))
            .unwrap(),
        Some(obj(slot::ACTOR_1))
    );
    assert_eq!(
        s.find_by_name("MyPawn", Some("/Script/Engine.Missing"))
            .unwrap(),
        None
    );
}

#[test]
fn types_classes_and_casts() {
    let s = session();
    assert_eq!(s.static_class::<AActor>().unwrap(), obj(slot::ACTOR));
    assert!(matches!(
        s.static_class::<AMissing>(),
        Err(SessionError::Invalid { .. })
    ));
    assert_eq!(
        s.default_object::<AActor>().unwrap().addr(),
        object_addr(BASE, slot::ACTOR_CDO)
    );

    let pawn = obj(slot::MY_PAWN);
    assert!(s.is_a(pawn, obj(slot::ACTOR)).unwrap());
    assert!(s.is_a(pawn, obj(slot::OBJECT)).unwrap());
    assert!(!s.is_a(obj(slot::ACTOR_1), obj(slot::PAWN)).unwrap());
    assert!(s.cast::<APawn, _>(pawn).unwrap().is_some());
    assert!(s.cast::<APawn, _>(obj(slot::ACTOR_1)).unwrap().is_none());
    assert!(
        s.cast::<UObject, _>(Ptr::<UObjectBase>::NULL)
            .unwrap()
            .is_none()
    );

    // Instances of Actor and subclasses, without the class default object.
    let actors: Vec<u64> = s
        .objects_of::<AActor>()
        .unwrap()
        .iter()
        .map(|p| p.addr())
        .collect();
    assert_eq!(
        actors,
        vec![
            object_addr(BASE, slot::ACTOR_1),
            object_addr(BASE, slot::MY_PAWN)
        ]
    );
}

#[test]
fn globals_and_validity() {
    let s = session();
    assert_eq!(s.gworld().unwrap(), Some(obj(slot::MY_PAWN)));
    assert!(matches!(s.gengine(), Err(SessionError::Unknown("GEngine"))));
    assert!(s.is_valid(obj(slot::PAWN)));
    assert!(!s.is_valid(Ptr::<UObjectBase>::NULL));
    assert!(!s.is_valid(Ptr::<UObjectBase>::new(BASE + 0x5000)));
    // An object whose index names another object's slot is stale.
    assert!(!s.is_valid(Ptr::<UObjectBase>::new(object_addr(BASE, 1) + 8)));
    assert_eq!(s.params_size(obj(slot::DESTROY)).unwrap(), 16);
}

#[test]
fn unknown_module_is_an_error() {
    let mem = Snapshot::builder()
        .region(BASE, image(BASE, 0))
        .build()
        .unwrap();
    let mut d = desc(BASE);
    d.objects.location = ufathom_runtime::Place::Rva {
        module: "Game.exe",
        rva: 0,
    };
    let d: &'static EngineDesc = Box::leak(Box::new(d));
    assert!(matches!(
        Session::new(mem, d),
        Err(SessionError::ModuleNotFound(_))
    ));
}
