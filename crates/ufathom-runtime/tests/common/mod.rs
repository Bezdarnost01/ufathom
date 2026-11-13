//! A tiny fake engine laid out at any base address: a name pool, a chunked
//! object array and a handful of objects with an outer chain, a class chain
//! and a function.
#![allow(dead_code, reason = "each test uses a part")]

use ufathom_runtime::{
    EngineDesc, FNameDesc, NamePoolDesc, NamesDesc, ObjectArrayDesc, ObjectDesc, Place,
};

pub const OBJ: u64 = 0x100;
const POOL: u64 = 0x0;
const BLOCK: u64 = 0x40;
const ARRAY: u64 = 0x800;
const CHUNKS: u64 = 0x840;
const ITEMS: u64 = 0x880;
pub const GWORLD: u64 = 0xa00;
pub const VTABLE: u64 = 0xa40;
const OBJECTS: u64 = 0x1000;
pub const SIZE: usize = 0x2000;

pub const PROCESS_EVENT_INDEX: u32 = 3;
pub const PARAMS_SIZE: u32 = 16;

/// Object slots.
pub mod slot {
    pub const CORE: u32 = 0;
    pub const CLASS: u32 = 1;
    pub const PACKAGE: u32 = 2;
    pub const FUNCTION: u32 = 3;
    pub const OBJECT: u32 = 4;
    pub const ENGINE: u32 = 5;
    pub const ACTOR: u32 = 6;
    pub const DESTROY: u32 = 7;
    pub const ACTOR_CDO: u32 = 8;
    pub const ACTOR_1: u32 = 9;
    pub const PAWN: u32 = 10;
    pub const MY_PAWN: u32 = 11;
    pub const COUNT: u32 = 12;
}

pub fn object_addr(base: u64, index: u32) -> u64 {
    base + OBJECTS + u64::from(index) * OBJ
}

/// The description of the fake build at `base`.
pub fn desc(base: u64) -> EngineDesc {
    EngineDesc {
        names: NamesDesc::Pool(NamePoolDesc {
            location: Place::Absolute(base + POOL),
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
            elements_per_chunk: Some(8),
            objects: 0,
            num_elements: 0x14,
            item_size: 24,
            item_object: 0,
            item_serial: Some(16),
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
            index: 12,
            class: 16,
            name: 24,
            outer: 32,
            super_struct: 0x40,
            properties_size: 0x50,
            default_object: Some(0x60),
        },
        process_event_index: Some(PROCESS_EVENT_INDEX),
        gworld: Some(Place::Absolute(base + GWORLD)),
        gengine: None,
    }
}

/// The bytes of the fake build at `base`. `process_event` is written to the
/// virtual table slot every object shares.
pub fn image(base: u64, process_event: u64) -> Vec<u8> {
    let mut img = Image {
        bytes: vec![0; SIZE],
    };
    // Names.
    img.u64(POOL + 0x10, base + BLOCK);
    let mut cursor = 0u64;
    let mut name = |img: &mut Image, text: &str| -> u32 {
        let at = BLOCK + cursor;
        img.u16(at, (text.len() as u16) << 6);
        img.put(at + 2, text.as_bytes());
        let index = (cursor / 2) as u32;
        cursor += (2 + text.len() as u64).div_ceil(2) * 2;
        index
    };
    let n_core = name(&mut img, "/Script/CoreUObject");
    let n_class = name(&mut img, "Class");
    let n_package = name(&mut img, "Package");
    let n_function = name(&mut img, "Function");
    let n_object = name(&mut img, "Object");
    let n_engine = name(&mut img, "/Script/Engine");
    let n_actor = name(&mut img, "Actor");
    let n_destroy = name(&mut img, "K2_DestroyActor");
    let n_default = name(&mut img, "Default__Actor");
    let n_pawn = name(&mut img, "Pawn");
    let n_my_pawn = name(&mut img, "MyPawn");

    // Object array: two chunks of eight.
    img.u64(ARRAY, base + CHUNKS);
    img.u32(ARRAY + 0x14, slot::COUNT);
    img.u64(CHUNKS, base + ITEMS);
    img.u64(CHUNKS + 8, base + ITEMS + 8 * 24);

    img.u64(GWORLD, object_addr(base, slot::MY_PAWN));
    img.u64(VTABLE + u64::from(PROCESS_EVENT_INDEX) * 8, process_event);

    let o = |i: u32| object_addr(base, i);
    use slot::*;
    // (slot, name, number, class, outer, flags)
    let objects = [
        (CORE, n_core, 0, PACKAGE, None, 0),
        (CLASS, n_class, 0, CLASS, Some(CORE), 0),
        (PACKAGE, n_package, 0, CLASS, Some(CORE), 0),
        (FUNCTION, n_function, 0, CLASS, Some(CORE), 0),
        (OBJECT, n_object, 0, CLASS, Some(CORE), 0),
        (ENGINE, n_engine, 0, PACKAGE, None, 0),
        (ACTOR, n_actor, 0, CLASS, Some(ENGINE), 0),
        (DESTROY, n_destroy, 0, FUNCTION, Some(ACTOR), 0),
        (ACTOR_CDO, n_default, 0, ACTOR, Some(ENGINE), 0x10),
        (ACTOR_1, n_actor, 2, ACTOR, Some(ENGINE), 0),
        (PAWN, n_pawn, 0, CLASS, Some(ENGINE), 0),
        (MY_PAWN, n_my_pawn, 0, PAWN, Some(ENGINE), 0),
    ];
    for (index, name, number, class, outer, flags) in objects {
        let at = OBJECTS + u64::from(index) * OBJ;
        let item = ITEMS + u64::from(index) * 24;
        img.u64(item, o(index));
        img.u32(item + 16, 100 + index);
        img.u64(at, base + VTABLE);
        img.u32(at + 8, flags);
        img.u32(at + 12, index);
        img.u64(at + 16, o(class));
        img.u32(at + 24, name);
        img.u32(at + 28, number);
        img.u64(at + 32, outer.map_or(0, o));
    }
    // Class chain and the class default object.
    img.u64(OBJECTS + u64::from(ACTOR) * OBJ + 0x40, o(OBJECT));
    img.u64(OBJECTS + u64::from(PAWN) * OBJ + 0x40, o(ACTOR));
    img.u64(OBJECTS + u64::from(ACTOR) * OBJ + 0x60, o(ACTOR_CDO));
    // The function's parameter block size.
    img.u32(OBJECTS + u64::from(DESTROY) * OBJ + 0x50, PARAMS_SIZE);
    img.bytes
}

struct Image {
    bytes: Vec<u8>,
}

impl Image {
    fn put(&mut self, at: u64, data: &[u8]) {
        let at = at as usize;
        self.bytes[at..at + data.len()].copy_from_slice(data);
    }
    fn u16(&mut self, at: u64, v: u16) {
        self.put(at, &v.to_le_bytes());
    }
    fn u32(&mut self, at: u64, v: u32) {
        self.put(at, &v.to_le_bytes());
    }
    fn u64(&mut self, at: u64, v: u64) {
        self.put(at, &v.to_le_bytes());
    }
}

/// Generated-style types for the tests.
pub mod types {
    use ufathom_runtime::TypePath;

    pub struct UObject;
    impl TypePath for UObject {
        const PATH: &'static str = "/Script/CoreUObject.Object";
    }
    pub struct AActor;
    impl TypePath for AActor {
        const PATH: &'static str = "/Script/Engine.Actor";
    }
    pub struct APawn;
    impl TypePath for APawn {
        const PATH: &'static str = "/Script/Engine.Pawn";
    }
    pub struct AMissing;
    impl TypePath for AMissing {
        const PATH: &'static str = "/Script/Engine.Missing";
    }
}
