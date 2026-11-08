//! Writes the reflection data: packages, classes, structs, enums, functions,
//! properties, default objects, instances and filler objects.
//!
//! Everything is written the way the profile's [`EngineLayout`] describes.
//! The writer reads only the layout and the schema; it never consults ground
//! truth, so what lands in memory is what a real engine would hold.

mod bodies;
mod fillers;
mod links;
mod props;

use std::collections::BTreeMap;

use ufathom_model::flags::{
    RF_ARCHETYPE_OBJECT, RF_CLASS_DEFAULT_OBJECT, RF_PUBLIC, RF_STANDALONE, RF_TRANSIENT,
};
use ufathom_model::{EnumDef, StructDef, StructKind};

use crate::code::Code;
use crate::names::NameTable;
use crate::profile::Profile;
use crate::rng::Rng;
use crate::schema::{
    CORE_PACKAGE, ENGINE_PACKAGE, PROPERTY_CLASSES, Schema, display_class_name,
    property_class_exists,
};
use crate::space::{Mem, Pool};

use props::Node;

/// `RF_MarkAsNative`.
const RF_MARK_AS_NATIVE: u32 = 0x4;
/// `RF_MarkAsRootSet`.
const RF_MARK_AS_ROOT_SET: u32 = 0x80;
/// `RF_WasLoaded`.
const RF_WAS_LOADED: u32 = 0x4_0000;
/// `RF_LoadCompleted`.
const RF_LOAD_COMPLETED: u32 = 0x8_0000;

/// Flags of a native `UClass`.
const FLAGS_NATIVE_CLASS: u32 =
    RF_PUBLIC | RF_STANDALONE | RF_TRANSIENT | RF_MARK_AS_NATIVE | RF_MARK_AS_ROOT_SET;
/// Flags of a native package.
const FLAGS_PACKAGE: u32 = RF_PUBLIC | RF_TRANSIENT | RF_MARK_AS_NATIVE;
/// Flags of a member of a native class.
const FLAGS_MEMBER: u32 = RF_PUBLIC | RF_TRANSIENT | RF_MARK_AS_NATIVE;
/// Flags of a class default object. The only objects with
/// `RF_ClassDefaultObject` set.
const FLAGS_CDO: u32 = RF_PUBLIC | RF_ARCHETYPE_OBJECT | RF_TRANSIENT | RF_CLASS_DEFAULT_OBJECT;
/// Flags of a loaded asset-like instance.
const FLAGS_LOADED: u32 = RF_PUBLIC | RF_WAS_LOADED | RF_LOAD_COMPLETED;
/// Flags of a transient runtime instance.
const FLAGS_RUNTIME: u32 = RF_TRANSIENT | RF_WAS_LOADED;

/// How the members of a struct are linked, where the engine could do either.
///
/// The order matters to a reader: the legacy `Children` list mixes properties
/// and functions, and function parameters are only correctly ordered if the
/// reader sorts them by offset when the chain is reversed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ChainOrder {
    /// Members in declaration order. In the legacy system properties come
    /// first, then functions.
    #[default]
    Declaration,
    /// The whole chain reversed, as when each member is prepended on
    /// registration. Parameters are reversed too.
    Reversed,
    /// Legacy only: functions before properties, in declaration order. Modern
    /// builds keep functions and properties in separate lists, so this equals
    /// [`Declaration`](Self::Declaration) there.
    FunctionsFirst,
}

/// Settings for [`write`].
#[derive(Debug, Clone)]
pub struct Config {
    /// Number of filler objects in addition to the schema objects.
    pub filler_objects: usize,
    /// Member chain order.
    pub chain_order: ChainOrder,
    /// Number of objects whose comparison name is a case-mangled twin of
    /// the display name.
    pub mangled_display_names: usize,
}

/// One object that is in the global object array, as ground truth.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectRecord {
    /// Slot in the global object array.
    pub index: u32,
    /// Address of the object.
    pub address: u64,
    /// Full path, for example `/Script/Engine.Actor:K2_DestroyActor`.
    pub path: String,
    /// Full path of its class.
    pub class_path: String,
    /// Path of its outer, or `None` for a package.
    pub outer_path: Option<String>,
    /// `true` for a class default object.
    pub is_default_object: bool,
}

/// One slot of the object array. `object == 0` is a free slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Slot {
    /// Object address, or 0.
    pub object: u64,
    /// Serial number.
    pub serial: u32,
    /// Internal object flags.
    pub flags: u32,
}

/// What the writer produced.
#[derive(Debug)]
pub struct Reflection {
    /// Every registered object, in index order.
    pub records: Vec<ObjectRecord>,
    /// The object array contents, dense up to `NumElements`.
    pub slots: Vec<Slot>,
    /// Address of every object and legacy or modern property by path.
    pub addr: BTreeMap<String, u64>,
    /// RVA of the exec thunk of every native function, by function path.
    pub native_rvas: BTreeMap<String, u64>,
    /// The live `World` instance.
    pub world: u64,
    /// The live `GameEngine` instance.
    pub engine: u64,
    /// A `World`-looking instance that is not in the object array.
    pub stale_world: u64,
    /// Indices of filler objects.
    pub fillers: Vec<u32>,
    /// The schema that was written, with `native_rva` filled in.
    pub schema: Schema,
    /// Names of the two objects used to test non-UTF-8 name handling.
    pub special_names: SpecialNames,
}

/// Names reserved for the encoding corruptions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpecialNames {
    /// An ASCII name that a corruption turns into Latin-1 bytes.
    pub ascii: String,
    /// A valid wide (UTF-16) name that a corruption makes invalid.
    pub wide: String,
}

/// A planned object.
#[derive(Debug, Clone)]
pub(crate) struct Obj {
    pub path: String,
    pub class: String,
    pub outer: Option<String>,
    pub base_name: String,
    pub number: u32,
    pub flags: u32,
    pub kind: Kind,
    /// `None` for an object that exists in memory but not in the array.
    pub index: Option<u32>,
    /// Index stored in the object itself.
    pub stored_index: u32,
    pub addr: u64,
}

#[derive(Debug, Clone)]
pub(crate) enum Kind {
    Package,
    Struct(usize),
    Enum(usize),
    Function { owner: usize, func: usize },
    Property,
    Cdo,
    Instance,
}

pub(crate) struct Writer<'a> {
    pub p: &'a Profile,
    pub mem: &'a mut Mem,
    pub names: &'a mut NameTable,
    pub code: &'a mut Code,
    pub rng: Rng,
    pub cfg: Config,
    pub schema: Schema,
    pub sizes: BTreeMap<String, (u32, u32)>,
    pub objs: Vec<Obj>,
    pub by_path: BTreeMap<String, usize>,
    pub nodes: Vec<Node>,
    pub node_by_path: BTreeMap<String, usize>,
    /// Top-level property nodes per owner (struct or function) path.
    pub chains: BTreeMap<String, Vec<usize>>,
    pub field_classes: BTreeMap<String, u64>,
    pub next_index: u32,
    pub super_of: BTreeMap<String, String>,
    pub vt_cache: BTreeMap<String, u64>,
    /// Per owner path: head of the `Children` list and of the child
    /// properties list.
    pub heads: BTreeMap<String, (u64, u64)>,
    /// Per owner path: heads of the four property link chains.
    pub link_heads: BTreeMap<String, links::LinkHeads>,
    pub native_rvas: BTreeMap<String, u64>,
    pub fillers: Vec<u32>,
    pub special: Option<SpecialNames>,
    /// Objects still to receive a mangled comparison name.
    pub mangled_left: usize,
}

/// Path of the package part of a class path, for CDO naming.
fn default_object_path(def: &StructDef) -> String {
    format!("{}.Default__{}", def.package, def.name)
}

/// Writes everything and returns the ground truth.
#[allow(
    clippy::too_many_arguments,
    reason = "one entry point wiring the shared builders together"
)]
pub fn write(
    profile: &Profile,
    schema: Schema,
    mem: &mut Mem,
    names: &mut NameTable,
    code: &mut Code,
    rng: Rng,
    cfg: Config,
) -> Reflection {
    let mut sizes = BTreeMap::new();
    for def in &schema.structs {
        sizes.insert(def.path.clone(), (def.size, def.alignment));
    }
    let super_of = schema
        .structs
        .iter()
        .filter_map(|def| def.super_path.clone().map(|s| (def.path.clone(), s)))
        .collect();
    let mangled_left = cfg.mangled_display_names;
    let mut writer = Writer {
        p: profile,
        mem,
        names,
        code,
        rng,
        cfg,
        schema,
        sizes,
        objs: Vec::new(),
        by_path: BTreeMap::new(),
        nodes: Vec::new(),
        node_by_path: BTreeMap::new(),
        chains: BTreeMap::new(),
        field_classes: BTreeMap::new(),
        next_index: 0,
        super_of,
        vt_cache: BTreeMap::new(),
        heads: BTreeMap::new(),
        link_heads: BTreeMap::new(),
        native_rvas: BTreeMap::new(),
        fillers: Vec::new(),
        special: None,
        mangled_left,
    };
    writer.plan_types();
    writer.plan_instances();
    writer.plan_fillers();
    writer.write_objects();
    writer.finish()
}

impl Writer<'_> {
    /// Registers an object: assigns the next slot (or none) and an address.
    #[allow(clippy::too_many_arguments, reason = "mirrors the fields of an object")]
    pub(crate) fn add_obj(
        &mut self,
        path: &str,
        class: &str,
        outer: Option<&str>,
        base: &str,
        number: u32,
        size: u32,
        flags: u32,
        kind: Kind,
    ) -> usize {
        let index = self.next_index;
        self.next_index += 1;
        let size = size.max(self.p.layout.uobject.size);
        let addr = self
            .mem
            .heap
            .alloc(Pool::Objects, usize::try_from(size).unwrap_or(0), 16);
        self.push_obj(Obj {
            path: path.to_owned(),
            class: class.to_owned(),
            outer: outer.map(str::to_owned),
            base_name: base.to_owned(),
            number,
            flags,
            kind,
            index: Some(index),
            stored_index: index,
            addr,
        })
    }

    pub(crate) fn push_obj(&mut self, obj: Obj) -> usize {
        let position = self.objs.len();
        self.by_path.insert(obj.path.clone(), position);
        self.objs.push(obj);
        position
    }

    pub(crate) fn class_path(name: &str) -> String {
        format!("{CORE_PACKAGE}.{name}")
    }

    /// Plans packages, classes, structs, enums, functions, properties and
    /// default objects.
    fn plan_types(&mut self) {
        let package_class = Self::class_path("Package");
        for package in [CORE_PACKAGE, ENGINE_PACKAGE] {
            let size = self.p.shape.sizeof_upackage;
            self.add_obj(
                package,
                &package_class,
                None,
                package,
                0,
                size,
                FLAGS_PACKAGE,
                Kind::Package,
            );
        }
        self.plan_field_classes();

        let structs = self.schema.structs.clone();
        for (i, def) in structs.iter().enumerate() {
            let class = match def.kind {
                StructKind::Class => Self::class_path("Class"),
                StructKind::ScriptStruct => Self::class_path("ScriptStruct"),
            };
            let size = match def.kind {
                StructKind::Class => self.p.shape.sizeof_uclass,
                StructKind::ScriptStruct => self.p.shape.sizeof_uscriptstruct,
            };
            self.add_obj(
                &def.path,
                &class,
                Some(&def.package),
                &def.name,
                0,
                size,
                FLAGS_NATIVE_CLASS,
                Kind::Struct(i),
            );
            for field in &def.fields {
                self.plan_property(&def.path, field);
            }
            for (f, function) in def.functions.iter().enumerate() {
                let size = self.p.shape.sizeof_ufunction;
                let class = Self::class_path("Function");
                self.add_obj(
                    &function.path,
                    &class,
                    Some(&def.path),
                    &function.name,
                    0,
                    size,
                    FLAGS_MEMBER,
                    Kind::Function { owner: i, func: f },
                );
                for param in &function.params {
                    self.plan_property(&function.path, param);
                }
            }
        }

        let enums: Vec<EnumDef> = self.schema.enums.clone();
        let enum_class = Self::class_path("Enum");
        for (i, def) in enums.iter().enumerate() {
            let size = self.p.shape.sizeof_uenum;
            self.add_obj(
                &def.path,
                &enum_class,
                Some(&def.package),
                &def.name,
                0,
                size,
                FLAGS_NATIVE_CLASS,
                Kind::Enum(i),
            );
        }

        for def in &structs {
            if def.kind != StructKind::Class || self.is_property_class(&def.name) {
                continue;
            }
            let path = default_object_path(def);
            let name = format!("Default__{}", def.name);
            self.add_obj(
                &path,
                &def.path,
                Some(&def.package),
                &name,
                0,
                def.size,
                FLAGS_CDO,
                Kind::Cdo,
            );
        }
    }

    pub(crate) fn is_property_class(&self, name: &str) -> bool {
        !self.p.is_modern()
            && PROPERTY_CLASSES.iter().any(|&(class, _)| {
                property_class_exists(class, self.p) && display_class_name(class, self.p) == name
            })
    }

    /// Plans the live world, engine and a stale world.
    fn plan_instances(&mut self) {
        let package_class = Self::class_path("Package");
        let psize = self.p.shape.sizeof_upackage;
        let world_class = format!("{ENGINE_PACKAGE}.World");
        let level_class = format!("{ENGINE_PACKAGE}.Level");
        let engine_class = format!("{ENGINE_PACKAGE}.GameEngine");
        let size_of = |this: &Self, class: &str| this.sizes.get(class).map_or(0, |s| s.0);

        let map = "/Game/Maps/Main";
        let transient = "/Engine/Transient";
        let old_map = "/Game/Maps/MainPrevious";
        for package in [map, transient, old_map] {
            self.add_obj(
                package,
                &package_class,
                None,
                package,
                0,
                psize,
                FLAGS_LOADED,
                Kind::Package,
            );
        }
        let world_size = size_of(self, &world_class);
        self.add_obj(
            &format!("{map}.Main"),
            &world_class,
            Some(map),
            "Main",
            0,
            world_size,
            FLAGS_LOADED,
            Kind::Instance,
        );
        let level_size = size_of(self, &level_class);
        let world_path = format!("{map}.Main");
        self.add_obj(
            &format!("{world_path}:PersistentLevel"),
            &level_class,
            Some(&world_path),
            "PersistentLevel",
            0,
            level_size,
            FLAGS_LOADED,
            Kind::Instance,
        );
        let engine_size = size_of(self, &engine_class);
        self.add_obj(
            &format!("{transient}.GameEngine_0"),
            &engine_class,
            Some(transient),
            "GameEngine",
            1,
            engine_size,
            FLAGS_RUNTIME,
            Kind::Instance,
        );
    }

    fn write_objects(&mut self) {
        for i in 0..self.objs.len() {
            self.write_header(i);
        }
        self.compute_chains();
        self.compute_links();
        for i in 0..self.objs.len() {
            self.write_body(i);
        }
        self.write_properties();
    }

    fn finish(mut self) -> Reflection {
        let mut records = Vec::new();
        let mut slots = Vec::new();
        for obj in &self.objs {
            let Some(index) = obj.index else {
                continue;
            };
            let slot = usize::try_from(index).unwrap_or(0);
            if slots.len() <= slot {
                slots.resize(slot + 1, Slot::default());
            }
            let serial = index.wrapping_mul(7).wrapping_add(1) & 0x3FFF_FFFF;
            if let Some(entry) = slots.get_mut(slot) {
                *entry = Slot {
                    object: obj.addr,
                    serial,
                    flags: 0,
                };
            }
            records.push(ObjectRecord {
                index,
                address: obj.addr,
                path: obj.path.clone(),
                class_path: obj.class.clone(),
                outer_path: obj.outer.clone(),
                is_default_object: matches!(obj.kind, Kind::Cdo),
            });
        }
        // Free slots keep a serial like a real array does.
        for (i, slot) in slots.iter_mut().enumerate() {
            if slot.object == 0 {
                slot.serial = u32::try_from(i)
                    .unwrap_or(0)
                    .wrapping_mul(7)
                    .wrapping_add(1)
                    & 0x3FFF_FFFF;
            }
        }
        records.sort_by_key(|r| r.index);
        let mut addr: BTreeMap<String, u64> =
            self.objs.iter().map(|o| (o.path.clone(), o.addr)).collect();
        for node in &self.nodes {
            addr.insert(node.path.clone(), node.addr);
        }
        let world = addr.get("/Game/Maps/Main.Main").copied().unwrap_or(0);
        let engine = addr
            .get("/Engine/Transient.GameEngine_0")
            .copied()
            .unwrap_or(0);
        let stale_world = addr
            .get("/Game/Maps/MainPrevious.Main")
            .copied()
            .unwrap_or(0);

        let mut schema = self.schema.clone();
        for def in &mut schema.structs {
            for function in &mut def.functions {
                function.native_rva = self.native_rvas.get(&function.path).copied();
            }
        }
        Reflection {
            records,
            slots,
            addr,
            native_rvas: std::mem::take(&mut self.native_rvas),
            world,
            engine,
            stale_world,
            fillers: std::mem::take(&mut self.fillers),
            schema,
            special_names: self.special.take().unwrap_or(SpecialNames {
                ascii: String::new(),
                wide: String::new(),
            }),
        }
    }
}
