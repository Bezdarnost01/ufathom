//! A tiny reader written for the tests, deliberately independent of
//! `Fixture::layout`: it uses raw offsets copied from the measured table.

#![allow(
    dead_code,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "each test binary uses a different part of this module; test helpers may panic"
)]

pub mod lr;

use ufathom_mem::{MemoryReader, MemoryReaderExt, Snapshot};
use ufathom_testkit::Fixture;

/// Raw offsets of one measured build, from the calibration table.
pub struct Calib {
    pub name: &'static str,
    pub fname_size: u64,
    pub fname_number: u64,
    pub flags: u64,
    pub index: u64,
    pub class: u64,
    pub obj_name: u64,
    pub outer: u64,
    pub uobject_size: u64,
    pub field_next: u64,
    pub super_struct: u64,
    pub children: u64,
    pub child_properties: Option<u64>,
    pub props_size: u64,
    pub func_flags: u64,
    pub func_ptr: u64,
    pub array_dim: u64,
    pub element_size: u64,
    pub prop_flags: u64,
    pub prop_offset: u64,
    /// `(next, name)` of `FField`, for modern builds.
    pub ffield: Option<(u64, u64)>,
    pub pool: bool,
    pub cdo: Option<u64>,
    pub cast_flags: Option<u64>,
    pub enum_names: Option<u64>,
    pub property_size: Option<u64>,
    pub item_size: u64,
    pub pe_index: Option<u64>,
}

pub const UE4_21: Calib = Calib {
    name: "4.21",
    fname_size: 8,
    fname_number: 4,
    flags: 8,
    index: 0xC,
    class: 0x10,
    obj_name: 0x18,
    outer: 0x20,
    uobject_size: 0x28,
    field_next: 0x28,
    super_struct: 0x30,
    children: 0x38,
    child_properties: None,
    props_size: 0x40,
    func_flags: 0x88,
    func_ptr: 0xB0,
    array_dim: 0x30,
    element_size: 0x34,
    prop_flags: 0x38,
    prop_offset: 0x44,
    ffield: None,
    pool: false,
    cdo: None,
    cast_flags: None,
    enum_names: Some(0x40),
    property_size: Some(0x70),
    item_size: 0x18,
    pe_index: Some(0x40),
};

pub const UE4_22: Calib = Calib {
    name: "4.22 case preserving",
    fname_size: 12,
    fname_number: 8,
    flags: 8,
    index: 0xC,
    class: 0x10,
    obj_name: 0x18,
    outer: 0x28,
    uobject_size: 0x30,
    field_next: 0x30,
    super_struct: 0x48,
    children: 0x50,
    child_properties: None,
    props_size: 0x58,
    func_flags: 0xA0,
    func_ptr: 0xC8,
    array_dim: 0x38,
    element_size: 0x3C,
    prop_flags: 0x40,
    prop_offset: 0x4C,
    ffield: None,
    pool: false,
    cdo: Some(0x108),
    cast_flags: Some(0xC0),
    enum_names: Some(0x48),
    property_size: Some(0x80),
    item_size: 0x18,
    pe_index: Some(0x40),
};

pub const UE4_25: Calib = Calib {
    name: "4.25 case preserving",
    fname_size: 12,
    fname_number: 8,
    flags: 8,
    index: 0xC,
    class: 0x10,
    obj_name: 0x18,
    outer: 0x28,
    uobject_size: 0x30,
    field_next: 0x30,
    super_struct: 0x48,
    children: 0x50,
    child_properties: Some(0x58),
    props_size: 0x60,
    // Flags 0xB8 is inferred from a measured ParmsSize at 0xBE; func 0xE0 is measured.
    func_flags: 0xB8,
    func_ptr: 0xE0,
    array_dim: 0x38,
    element_size: 0x3C,
    prop_flags: 0x40,
    prop_offset: 0x4C,
    ffield: Some((0x20, 0x28)),
    pool: true,
    cdo: None,
    cast_flags: None,
    enum_names: None,
    property_size: None,
    item_size: 0x18,
    pe_index: None,
};

/// Chunked object array: 0x10000 items per chunk.
pub const ITEMS_PER_CHUNK: u64 = 0x1_0000;

/// A raw reader over a fixture's memory.
pub struct Raw<'a> {
    pub mem: &'a Snapshot,
    pub c: &'a Calib,
    pub fx: &'a Fixture,
}

impl<'a> Raw<'a> {
    pub fn new(fx: &'a Fixture, c: &'a Calib) -> Self {
        Self {
            mem: &fx.memory,
            c,
            fx,
        }
    }

    pub fn u32(&self, addr: u64) -> u32 {
        self.mem.read_u32(addr).expect("readable")
    }

    pub fn u64(&self, addr: u64) -> u64 {
        self.mem.read_u64(addr).expect("readable")
    }

    /// Fields of the chunked array, read at the raw offsets of `FChunkedFixedUObjectArray`.
    pub fn num_elements(&self) -> u64 {
        u64::from(self.u32(self.fx.anchors.data.object_array + 0x14))
    }

    pub fn num_chunks(&self) -> u64 {
        u64::from(self.u32(self.fx.anchors.data.object_array + 0x1C))
    }

    pub fn max_elements(&self) -> u64 {
        u64::from(self.u32(self.fx.anchors.data.object_array + 0x10))
    }

    /// The object in slot `i`, or `None` for a free slot.
    pub fn object(&self, i: u64) -> Option<u64> {
        let table = self.u64(self.fx.anchors.data.object_array);
        let chunk = self.u64(table + (i / ITEMS_PER_CHUNK) * 8);
        let item = chunk + (i % ITEMS_PER_CHUNK) * self.c.item_size;
        let ptr = self.u64(item);
        (ptr != 0).then_some(ptr)
    }

    pub fn serial(&self, i: u64) -> u32 {
        let table = self.u64(self.fx.anchors.data.object_array);
        let chunk = self.u64(table + (i / ITEMS_PER_CHUNK) * 8);
        self.u32(chunk + (i % ITEMS_PER_CHUNK) * self.c.item_size + 0x10)
    }

    /// Decodes the name with stored index `index`, by hand.
    pub fn name_at(&self, index: u32) -> String {
        let anchor = self.fx.anchors.data.name_anchor;
        if self.c.pool {
            // FNamePool: blocks at +0x10, stride 4, 16 offset bits; entry
            // header at +4, characters at +6, length = header >> 1.
            let block = u64::from(index >> 16);
            let offset = u64::from(index & 0xFFFF) * 4;
            let base = self.u64(anchor + 0x10 + block * 8);
            let entry = base + offset;
            let header = u64::from(self.mem.read_u16(entry + 4).expect("header"));
            let len = (header >> 1) as usize;
            let wide = header & 1 != 0;
            self.string(entry + 6, len, wide)
        } else {
            // TNameEntryArray: GNames points to the array; 16384 entries per
            // chunk; index at +0 (bit 0 = wide), characters at +0x10.
            let array = self.u64(anchor);
            let chunk = self.u64(array + (u64::from(index) / 16384) * 8);
            let entry = self.u64(chunk + (u64::from(index) % 16384) * 8);
            let stored = self.u32(entry);
            assert_eq!(stored >> 1, index, "entry stores its own index");
            let wide = stored & 1 != 0;
            let mut len = 0usize;
            loop {
                let unit = if wide {
                    u64::from(
                        self.mem
                            .read_u16(entry + 0x10 + len as u64 * 2)
                            .expect("chars"),
                    )
                } else {
                    u64::from(self.mem.read_u8(entry + 0x10 + len as u64).expect("chars"))
                };
                if unit == 0 {
                    break;
                }
                len += 1;
            }
            self.string(entry + 0x10, len, wide)
        }
    }

    fn string(&self, addr: u64, len: usize, wide: bool) -> String {
        if wide {
            let units: Vec<u16> = (0..len)
                .map(|i| self.mem.read_u16(addr + i as u64 * 2).expect("chars"))
                .collect();
            String::from_utf16(&units).expect("valid UTF-16")
        } else {
            let mut bytes = vec![0u8; len];
            self.mem.read_exact(addr, &mut bytes).expect("chars");
            String::from_utf8(bytes).expect("valid UTF-8")
        }
    }

    /// Decodes an `FName` stored at `addr` (comparison index at +0).
    pub fn fname(&self, addr: u64) -> String {
        let index = self.u32(addr);
        let number = self.u32(addr + self.c.fname_number);
        let base = self.name_at(index);
        if number == 0 {
            base
        } else {
            format!("{base}_{}", number - 1)
        }
    }

    pub fn name_of(&self, object: u64) -> String {
        self.fname(object + self.c.obj_name)
    }

    pub fn class_of(&self, object: u64) -> u64 {
        self.u64(object + self.c.class)
    }

    pub fn outer_of(&self, object: u64) -> u64 {
        self.u64(object + self.c.outer)
    }

    /// Finds the object called `name` whose outer is called `outer`
    /// (`None` for a package).
    pub fn find(&self, outer: Option<&str>, name: &str) -> Option<u64> {
        (0..self.num_elements()).find_map(|i| {
            let object = self.object(i)?;
            if self.name_of(object) != name {
                return None;
            }
            let outer_object = self.outer_of(object);
            let matches = match outer {
                None => outer_object == 0,
                Some(wanted) => outer_object != 0 && self.name_of(outer_object) == wanted,
            };
            matches.then_some(object)
        })
    }

    /// The members linked from a list head through `next`.
    pub fn chain(&self, head: u64, next: u64) -> Vec<u64> {
        let mut out = Vec::new();
        let mut at = head;
        while at != 0 && out.len() < 200 {
            out.push(at);
            at = self.u64(at + next);
        }
        out
    }

    /// The property chain of a struct or function: `UProperty` objects in
    /// `Children` (legacy) or `FField`s in `ChildProperties` (modern).
    /// Returns `(address, name)` of each property.
    pub fn properties(&self, owner: u64) -> Vec<(u64, String)> {
        match (self.c.child_properties, self.c.ffield) {
            (Some(child_properties), Some((next, name))) => {
                let head = self.u64(owner + child_properties);
                self.chain(head, next)
                    .into_iter()
                    .map(|f| (f, self.fname(f + name)))
                    .collect()
            }
            _ => {
                let head = self.u64(owner + self.c.children);
                self.chain(head, self.c.field_next)
                    .into_iter()
                    .filter(|&m| self.name_of(self.class_of(m)) != "Function")
                    .map(|m| (m, self.name_of(m)))
                    .collect()
            }
        }
    }

    /// The `UFunction` members of a class.
    pub fn functions(&self, class: u64) -> Vec<(u64, String)> {
        let head = self.u64(class + self.c.children);
        self.chain(head, self.c.field_next)
            .into_iter()
            .filter(|&m| self.name_of(self.class_of(m)) == "Function")
            .map(|m| (m, self.name_of(m)))
            .collect()
    }
}
