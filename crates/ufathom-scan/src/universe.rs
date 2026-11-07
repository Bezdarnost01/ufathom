//! The objects of the array, read once, and the CoreUObject anchors found
//! among them.
//!
//! Anchors are compared case-insensitively, because builds differ in how
//! they spell the comparison name of an entry.

use std::collections::HashMap;

use ufathom_engine::NameTable;

use crate::ctx::{Ctx, le_u32, le_u64};
use crate::limits::MAX_UNIVERSE_OBJECTS;
use crate::uobject::ObjectFields;

/// Package that holds the classes every engine has.
pub(crate) const CORE_PACKAGE: &str = "/Script/CoreUObject";

/// Package that holds `Actor`, `World` and the engine classes.
pub(crate) const ENGINE_PACKAGE: &str = "/Script/Engine";

/// One object, as far as the solved header fields describe it.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Obj {
    pub index: u32,
    pub addr: u64,
    pub comparison: u32,
    pub display: u32,
    pub number: u32,
    pub class: u64,
    pub outer: u64,
}

/// Every object read, in index order.
pub(crate) struct Universe {
    pub objs: Vec<Obj>,
    by_addr: HashMap<u64, usize>,
}

impl Universe {
    /// Reads the header of every object up to [`MAX_UNIVERSE_OBJECTS`].
    pub(crate) fn scan(ctx: &Ctx<'_>, slots: &[(u32, u64)], fields: &ObjectFields) -> Self {
        let fname = &fields.fname;
        let cmp = fields.name + fname.comparison_index;
        let display = fname.display_index.map(|d| fields.name + d);
        let number = fname.number.map(|n| fields.name + n);
        let offsets = [
            Some(cmp),
            display,
            number,
            Some(fields.class),
            Some(fields.outer),
        ];
        let low = offsets.iter().flatten().copied().min().unwrap_or(0);
        let high = offsets.iter().flatten().copied().max().unwrap_or(0) + 8;
        let span = usize::try_from(high - low).unwrap_or(0);
        let rel = |offset: u32| (offset - low) as usize;
        let mut objs = Vec::new();
        let mut by_addr = HashMap::new();
        for &(index, addr) in slots.iter().take(MAX_UNIVERSE_OBJECTS) {
            let Some(bytes) = ctx.read(addr + u64::from(low), span) else {
                continue;
            };
            let comparison = le_u32(&bytes, rel(cmp)).unwrap_or(0);
            let obj = Obj {
                index,
                addr,
                comparison,
                display: display
                    .and_then(|d| le_u32(&bytes, rel(d)))
                    .unwrap_or(comparison),
                number: number.and_then(|n| le_u32(&bytes, rel(n))).unwrap_or(0),
                class: le_u64(&bytes, rel(fields.class)).unwrap_or(0),
                outer: le_u64(&bytes, rel(fields.outer)).unwrap_or(0),
            };
            by_addr.insert(addr, objs.len());
            objs.push(obj);
        }
        Self { objs, by_addr }
    }

    pub(crate) fn get(&self, addr: u64) -> Option<&Obj> {
        self.by_addr.get(&addr).and_then(|&i| self.objs.get(i))
    }

    /// The name of an object without its instance number.
    pub(crate) fn name<'t>(&self, table: &'t NameTable, obj: &Obj) -> Option<&'t str> {
        table
            .resolve_index(obj.display)
            .or_else(|| table.resolve_index(obj.comparison))
    }

    /// Name of the object at `addr`, if it was read.
    pub(crate) fn name_of<'t>(&self, table: &'t NameTable, addr: u64) -> Option<&'t str> {
        self.name(table, self.get(addr)?)
    }

    /// Objects whose name is one of `wanted` (case-insensitive, no instance
    /// number), by lower-case name.
    pub(crate) fn named(&self, table: &NameTable, wanted: &[&str]) -> HashMap<String, Vec<usize>> {
        let mut by_len: HashMap<usize, Vec<&str>> = HashMap::new();
        for name in wanted {
            by_len.entry(name.len()).or_default().push(name);
        }
        let mut indices: HashMap<u32, String> = HashMap::new();
        for (index, text) in table.iter() {
            if let Some(candidates) = by_len.get(&text.len())
                && candidates.iter().any(|w| w.eq_ignore_ascii_case(text))
            {
                indices.insert(index, text.to_ascii_lowercase());
            }
        }
        let mut out: HashMap<String, Vec<usize>> = HashMap::new();
        for (position, obj) in self.objs.iter().enumerate() {
            if obj.number != 0 {
                continue;
            }
            let hit = indices
                .get(&obj.display)
                .or_else(|| indices.get(&obj.comparison));
            if let Some(name) = hit {
                out.entry(name.clone()).or_default().push(position);
            }
        }
        out
    }
}

/// The CoreUObject and engine objects the solver anchors on.
#[derive(Debug, Clone, Default)]
pub(crate) struct Anchors {
    pub object: u64,
    pub field: u64,
    pub struct_: u64,
    pub class: u64,
    pub function: u64,
    pub script_struct: u64,
    pub enum_: u64,
    pub package: u64,
    /// Script structs by lower-case name: guid, color, vector, transform, ...
    pub structs: HashMap<String, u64>,
    /// Engine classes by lower-case name: actor, world, gameengine, engine.
    pub engine: HashMap<String, u64>,
}

const CORE_CLASSES: [&str; 8] = [
    "Object",
    "Field",
    "Struct",
    "Class",
    "Function",
    "ScriptStruct",
    "Enum",
    "Package",
];

const CORE_STRUCTS: [&str; 7] = [
    "Guid",
    "Color",
    "LinearColor",
    "Vector",
    "Rotator",
    "Quat",
    "Transform",
];

const ENGINE_CLASSES: [&str; 5] = ["Actor", "World", "GameEngine", "Engine", "Pawn"];

impl Anchors {
    /// Finds the anchors. The core classes are mandatory; the others are
    /// found when present.
    pub(crate) fn find(
        universe: &Universe,
        table: &NameTable,
        metaclass: u64,
    ) -> Result<Self, &'static str> {
        let wanted: Vec<&str> = CORE_CLASSES
            .iter()
            .chain(&CORE_STRUCTS)
            .chain(&ENGINE_CLASSES)
            .copied()
            .collect();
        let named = universe.named(table, &wanted);
        let in_package = |obj: &Obj, package: &str| {
            universe
                .name_of(table, obj.outer)
                .is_some_and(|name| name.eq_ignore_ascii_case(package))
        };
        // `class` of zero accepts any class: engine classes are found by name
        // and package alone, so a damaged class link on one of them does not
        // hide it.
        let pick = |name: &str, class: u64, package: &str| -> Option<u64> {
            named
                .get(&name.to_ascii_lowercase())?
                .iter()
                .filter_map(|&i| universe.objs.get(i))
                .find(|obj| (class == 0 || obj.class == class) && in_package(obj, package))
                .map(|obj| obj.addr)
        };
        let core = |name: &str| pick(name, metaclass, CORE_PACKAGE);
        let mut anchors = Self {
            class: metaclass,
            ..Self::default()
        };
        let (Some(object), Some(field), Some(struct_), Some(function)) = (
            core("Object"),
            core("Field"),
            core("Struct"),
            core("Function"),
        ) else {
            return Err("Object, Field, Struct or Function is missing from /Script/CoreUObject");
        };
        let (Some(script_struct), Some(enum_), Some(package)) =
            (core("ScriptStruct"), core("Enum"), core("Package"))
        else {
            return Err("ScriptStruct, Enum or Package is missing from /Script/CoreUObject");
        };
        anchors.object = object;
        anchors.field = field;
        anchors.struct_ = struct_;
        anchors.function = function;
        anchors.script_struct = script_struct;
        anchors.enum_ = enum_;
        anchors.package = package;
        for name in CORE_STRUCTS {
            if let Some(addr) = pick(name, script_struct, CORE_PACKAGE) {
                anchors.structs.insert(name.to_ascii_lowercase(), addr);
            }
        }
        for name in ENGINE_CLASSES {
            if let Some(addr) = pick(name, 0, ENGINE_PACKAGE) {
                anchors.engine.insert(name.to_ascii_lowercase(), addr);
            }
        }
        Ok(anchors)
    }
}
