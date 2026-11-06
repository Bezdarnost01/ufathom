//! Filler objects: enough instances of various classes, with realistic names,
//! that object counts and name distributions look like a running game.

use std::collections::BTreeMap;

use super::{FLAGS_LOADED, FLAGS_RUNTIME, Kind, Obj, SpecialNames, Writer};
use crate::schema::ENGINE_PACKAGE;
use crate::space::Pool;

const WORDS_A: [&str; 40] = [
    "Alpha", "Node", "Block", "Item", "Unit", "Field", "Cell", "Core", "Frame", "Group", "Layer",
    "Link", "Mark", "Panel", "Point", "Region", "Slot", "Span", "Tile", "Token", "Vertex", "Zone",
    "Axis", "Beam", "Bundle", "Channel", "Cluster", "Column", "Cube", "Cursor", "Edge", "Entity",
    "Grid", "Handle", "Index", "Label", "Lane", "Matrix", "Module", "Origin",
];

const WORDS_B: [&str; 40] = [
    "Main", "Side", "Upper", "Lower", "Inner", "Outer", "North", "South", "East", "West", "Broken",
    "Intact", "Large", "Small", "Old", "New", "Hidden", "Open", "Closed", "Locked", "Front",
    "Back", "Left", "Right", "Alpha", "Beta", "Gamma", "Delta", "Red", "Blue", "Green", "Grey",
    "Dark", "Bright", "Quiet", "Loud", "Short", "Tall", "Wide", "Narrow",
];

/// Classes filler instances are drawn from, with weights.
const FILLER_CLASSES: [(&str, u64); 5] = [
    ("Actor", 55),
    ("Pawn", 22),
    ("Controller", 10),
    ("PlayerController", 8),
    ("Level", 5),
];

const LEVEL: &str = "/Game/Maps/Main.Main:PersistentLevel";
const WORLD: &str = "/Game/Maps/Main.Main";

impl Writer<'_> {
    fn instance_size(&self, class: &str) -> u32 {
        self.sizes.get(class).map_or(0x40, |s| s.0)
    }

    /// A pool of distinct base names.
    fn base_names(&self, count: usize) -> Vec<String> {
        (0..count)
            .map(|i| {
                let a = WORDS_A.get(i % 40).copied().unwrap_or("Thing");
                let b = WORDS_B.get((i / 40) % 40).copied().unwrap_or("Part");
                let round = i / 1600;
                if round == 0 {
                    format!("BP_{a}{b}_C")
                } else {
                    format!("BP_{a}{b}{round}_C")
                }
            })
            .collect()
    }

    fn full_name(base: &str, number: u32) -> String {
        if number == 0 {
            base.to_owned()
        } else {
            format!("{base}_{}", number - 1)
        }
    }

    fn pick_class(&mut self) -> &'static str {
        let total: u64 = FILLER_CLASSES.iter().map(|&(_, w)| w).sum();
        let mut roll = self.rng.below(total);
        for &(class, weight) in &FILLER_CLASSES {
            if roll < weight {
                return class;
            }
            roll -= weight;
        }
        "Actor"
    }

    /// Plans the special-name objects, `filler_objects` fillers and a stale
    /// world.
    pub(crate) fn plan_fillers(&mut self) {
        let actor_class = format!("{ENGINE_PACKAGE}.Actor");
        let size = self.instance_size(&actor_class);
        // Two objects whose names exercise the text decoding paths. The wide
        // one holds a valid non-ASCII character; the ASCII one is turned into
        // Latin-1 by a corruption.
        let special = SpecialNames {
            ascii: "CafeSpawn".to_owned(),
            wide: "Gr\u{fc}nerEingang".to_owned(),
        };
        for name in [&special.ascii, &special.wide] {
            let path = format!("{LEVEL}.{name}");
            self.add_obj(
                &path,
                &actor_class,
                Some(LEVEL),
                name,
                0,
                size,
                FLAGS_RUNTIME,
                Kind::Instance,
            );
        }
        self.special = Some(special);

        let count = self.cfg.filler_objects;
        let bases = self.base_names(count / 3 + 8);
        let mut next_number: BTreeMap<String, u32> = BTreeMap::new();
        let mut package_serial = 0u32;
        for k in 0..count {
            // A freed slot now and then, like a real object array.
            if count >= 100 && k > 20 && self.rng.below(1000) < 3 {
                self.next_index += 1;
                continue;
            }
            if self.rng.chance(4) {
                package_serial += 1;
                let a = WORDS_A.get(k % 40).copied().unwrap_or("Thing");
                let name = format!("/Game/Content/{a}/Pkg{package_serial:05}");
                let class = Self::class_path("Package");
                let size = self.p.shape.sizeof_upackage;
                let before = self.next_index;
                self.add_obj(
                    &name,
                    &class,
                    None,
                    &name,
                    0,
                    size,
                    FLAGS_LOADED,
                    Kind::Package,
                );
                self.fillers.push(before);
                continue;
            }
            let class_name = self.pick_class();
            let class = format!("{ENGINE_PACKAGE}.{class_name}");
            let base = self
                .rng
                .pick(&bases)
                .cloned()
                .unwrap_or_else(|| "BP_Filler_C".to_owned());
            let first = u32::from(!self.rng.chance(40));
            let number = *next_number.entry(base.clone()).or_insert(first);
            next_number.insert(base.clone(), number + 1);
            let outer = if class_name == "Level" { WORLD } else { LEVEL };
            let full = Self::full_name(&base, number);
            let separator = if class_name == "Level" { ':' } else { '.' };
            let path = format!("{outer}{separator}{full}");
            let size = self.instance_size(&class);
            let before = self.next_index;
            self.add_obj(
                &path,
                &class,
                Some(outer),
                &base,
                number,
                size,
                FLAGS_RUNTIME,
                Kind::Instance,
            );
            self.fillers.push(before);
        }
        self.plan_stale_world();
    }

    /// A `World`-looking object that is not in the object array: its stored
    /// index belongs to another object, as after the slot was reused.
    fn plan_stale_world(&mut self) {
        let world_class = format!("{ENGINE_PACKAGE}.World");
        let size = self
            .instance_size(&world_class)
            .max(self.p.layout.uobject.size);
        let addr = self
            .mem
            .heap
            .alloc(Pool::Objects, usize::try_from(size).unwrap_or(0), 16);
        let stolen = self
            .fillers
            .get(self.fillers.len() / 2)
            .copied()
            .unwrap_or(5);
        self.push_obj(Obj {
            path: "/Game/Maps/MainPrevious.Main".to_owned(),
            class: world_class,
            outer: Some("/Game/Maps/MainPrevious".to_owned()),
            base_name: "Main".to_owned(),
            number: 0,
            flags: FLAGS_RUNTIME,
            kind: Kind::Instance,
            index: None,
            stored_index: stolen,
            addr,
        });
    }
}
