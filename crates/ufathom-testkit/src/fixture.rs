//! The fixture builder: ties profile, schema, writers and corruption
//! together into a finished image plus its ground truth.

use ufathom_mem::{Perms, RegionKind, Snapshot};
use ufathom_model::layout::{Location, NameTableLayout};
use ufathom_model::{EngineLayout, EnumDef, StructDef};

use crate::code::Code;
use crate::corrupt::{self, Corruption};
use crate::data::{self, DataAnchors};
use crate::names::NameTable;
use crate::pe;
use crate::profile::Profile;
use crate::reflect::{self, ChainOrder, Config, ObjectRecord, Reflection};
use crate::rng::Rng;
use crate::schema::build_schema;
use crate::space::{
    Addresses, DATA_RVA, HostedPlacement, Mem, PAGE, Part, PartKind, Placement, RDATA_RVA,
    RSRC_RVA, TEXT_RVA,
};

/// File name of the fake module in snapshot mode.
pub const MODULE_NAME: &str = "Game-Win64-Shipping.exe";

/// Names the engine registers first, in this order. `None` is index 0 and
/// `ByteProperty` index 1, which solvers use to recognise a name table.
const HARDCODED_NAMES: [&str; 24] = [
    "None",
    "ByteProperty",
    "IntProperty",
    "BoolProperty",
    "FloatProperty",
    "ObjectProperty",
    "NameProperty",
    "DelegateProperty",
    "DoubleProperty",
    "ArrayProperty",
    "StructProperty",
    "VectorProperty",
    "RotatorProperty",
    "StrProperty",
    "TextProperty",
    "InterfaceProperty",
    "MulticastDelegateProperty",
    "WeakObjectProperty",
    "LazyObjectProperty",
    "UInt64Property",
    "UInt32Property",
    "UInt16Property",
    "Int64Property",
    "Int16Property",
];

/// Where the expected globals are, as RVAs inside the module.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExpectedGlobals {
    /// Module file name the RVAs are relative to.
    pub module: String,
    /// RVA of the `GWorld` slot.
    pub gworld_rva: u64,
    /// RVA of the `GEngine` slot.
    pub gengine_rva: u64,
    /// Virtual table index of `UObject::ProcessEvent`.
    pub process_event_index: u32,
    /// RVA of `UObject::ProcessEvent`.
    pub process_event_rva: u64,
}

/// Expected engine identification strings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExpectedEngine {
    /// `ProductVersion` in the version resource, for example `4.25.1`. Only
    /// present in snapshot mode; a hosted image has the host's own resources.
    pub claimed: Option<String>,
    /// The UTF-16 build string in `.rdata`, for example `++Game+Live-CL-1234`.
    pub build: String,
    /// The changelist inside [`build`](Self::build).
    pub changelist: u32,
}

/// Expected reflected definitions.
///
/// `StructDef::flags` is 0 everywhere: `EngineLayout` has no offset for
/// `ClassFlags` or `StructFlags`, so a reader cannot fill it. Compare the
/// other fields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Truth {
    /// Classes and script structs, normalised (sorted by path, fields by
    /// offset and name, functions by path, parameters in declared order).
    pub structs: Vec<StructDef>,
    /// Enums, normalised. Each ends with its `_MAX` entry, as in memory.
    pub enums: Vec<EnumDef>,
}

/// Absolute addresses of interesting places. Ground truth for tests only: a
/// solver must find these itself.
#[derive(Debug, Clone)]
pub struct Anchors {
    /// Everything written to `.data`.
    pub data: DataAnchors,
    /// The `ProcessEvent` candidate.
    pub process_event: u64,
    /// Functions that contain only part of the `ProcessEvent` signature.
    pub decoy_functions: Vec<u64>,
    /// Address of the live `World`.
    pub world: u64,
    /// Address of the live `GameEngine`.
    pub engine: u64,
    /// Address of a stale `World` that is not in the object array.
    pub stale_world: u64,
    /// An address inside the reserved heap range that no part covers.
    pub unmapped: u64,
    /// Slot indices of the filler objects.
    pub fillers: Vec<u32>,
    /// RVA of the exec thunk of each native function, by function path.
    pub native_rvas: std::collections::BTreeMap<String, u64>,
}

/// A finished synthetic image and the answer a solver should find.
#[derive(Debug, Clone)]
pub struct Fixture {
    /// The profile it was built from.
    pub profile: Profile,
    /// The image as a reader sees it.
    pub memory: Snapshot,
    /// The expected solver answer, with locations filled in.
    pub layout: EngineLayout,
    /// Expected globals.
    pub globals: ExpectedGlobals,
    /// Expected version strings.
    pub engine: ExpectedEngine,
    /// Expected reflected definitions.
    pub truth: Truth,
    /// The objects in the global object array.
    pub objects: Vec<ObjectRecord>,
    /// Absolute addresses of interesting places.
    pub anchors: Anchors,
    /// Corruptions applied.
    pub corruptions: Vec<Corruption>,
    /// The same bytes as separate parts, for hosting in another process.
    pub parts: Vec<Part>,
    /// Bytes of address space the heap parts span from the heap base.
    pub heap_span: u64,
    /// Where everything was placed.
    pub addresses: Addresses,
}

impl Fixture {
    /// The module the image presents.
    #[must_use]
    pub fn module_name(&self) -> &str {
        &self.addresses.module_name
    }

    /// Address of the object with a full path, if it is in the array.
    #[must_use]
    pub fn object(&self, path: &str) -> Option<&ObjectRecord> {
        self.objects.iter().find(|r| r.path == path)
    }

    /// Absolute address from an RVA.
    #[must_use]
    pub fn at_rva(&self, rva: u64) -> u64 {
        self.addresses.module_base + rva
    }
}

/// Builds a [`Fixture`].
#[derive(Debug, Clone)]
pub struct FixtureBuilder {
    profile: Profile,
    seed: u64,
    filler_objects: usize,
    name_blocks: usize,
    mangled_display_names: usize,
    corruptions: Vec<Corruption>,
    chain_order: ChainOrder,
    placement: Placement,
}

impl FixtureBuilder {
    /// Starts a builder: seed 0, 6000 filler objects, snapshot placement.
    #[must_use]
    pub fn new(profile: Profile) -> Self {
        Self {
            profile,
            seed: 0,
            filler_objects: 6000,
            name_blocks: 1,
            mangled_display_names: 0,
            corruptions: Vec::new(),
            chain_order: ChainOrder::default(),
            placement: Placement::Snapshot,
        }
    }

    /// Sets the seed. Same inputs and seed give byte-identical output.
    #[must_use]
    pub fn seed(mut self, seed: u64) -> Self {
        self.seed = seed;
        self
    }

    /// Sets the number of filler objects added to the schema objects.
    #[must_use]
    pub fn filler_objects(mut self, count: usize) -> Self {
        self.filler_objects = count;
        self
    }

    /// Pads the name table until it spans at least `blocks` pool blocks or
    /// array chunks.
    #[must_use]
    pub fn name_blocks(mut self, blocks: usize) -> Self {
        self.name_blocks = blocks;
        self
    }

    /// Makes `count` objects carry a comparison name whose text differs from
    /// the display name only in case (`struct` against `Struct`), as
    /// case-preserving builds do. The truth keeps the display text.
    ///
    /// Has an effect only on profiles with a display index, and only on
    /// objects whose name has an uppercase ASCII letter and no instance number.
    #[must_use]
    pub fn mangled_display_names(mut self, count: usize) -> Self {
        self.mangled_display_names = count;
        self
    }

    /// Adds a corruption. May be called several times.
    #[must_use]
    pub fn corrupt(mut self, corruption: Corruption) -> Self {
        self.corruptions.push(corruption);
        self
    }

    /// Chooses how member chains are ordered.
    #[must_use]
    pub fn chain_order(mut self, order: ChainOrder) -> Self {
        self.chain_order = order;
        self
    }

    /// Chooses where the parts of the image go.
    #[must_use]
    pub fn placement(mut self, placement: Placement) -> Self {
        self.placement = placement;
        self
    }

    fn addresses(&self) -> Addresses {
        match &self.placement {
            Placement::Snapshot => {
                let mut rng = Rng::new(self.seed).fork("placement");
                let base = 0x7FF6_0000_0000 + rng.below(0x40) * 0x100_0000;
                let heap = 0x0000_01F4_0000_0000 + rng.below(0x40) * 0x1000_0000;
                Addresses {
                    module_name: MODULE_NAME.to_owned(),
                    module_base: base,
                    text: base + TEXT_RVA,
                    rdata: base + RDATA_RVA,
                    data: base + DATA_RVA,
                    rsrc: base + RSRC_RVA,
                    heap,
                }
            }
            Placement::Hosted(HostedPlacement {
                module_name,
                module_base,
                text,
                rdata,
                data,
                heap,
            }) => Addresses {
                module_name: module_name.clone(),
                module_base: *module_base,
                text: *text,
                rdata: *rdata,
                data: *data,
                rsrc: 0,
                heap: *heap,
            },
        }
    }

    /// Builds the image.
    ///
    /// # Panics
    ///
    /// If the profile fails [`Profile::validate`], or the schema refers to a
    /// type that does not exist. Both are mistakes in the builder inputs, not
    /// in target data.
    #[must_use]
    #[allow(
        clippy::expect_used,
        reason = "a build-time mistake in test infrastructure"
    )]
    pub fn build(self) -> Fixture {
        if let Err(problem) = self.profile.validate() {
            panic!("invalid engine profile {}: {problem}", self.profile.name);
        }
        let profile = self.profile.clone();
        let addrs = self.addresses();
        let mut mem = Mem::new(addrs.clone());
        let rng = Rng::new(self.seed);

        let mut code = Code::new(&mut mem, &profile, rng.fork("code"));
        let schema = build_schema(&profile);
        let mut names =
            NameTable::new(&profile.layout.names, &profile.layout.fname, &profile.shape);
        for name in HARDCODED_NAMES {
            let _ = names.intern(name);
        }
        let reflection = reflect::write(
            &profile,
            schema,
            &mut mem,
            &mut names,
            &mut code,
            rng.fork("reflect"),
            Config {
                filler_objects: self.filler_objects,
                chain_order: self.chain_order,
                mangled_display_names: self.mangled_display_names,
            },
        );
        code.noise(&mut mem, 60);
        names.pad_to_blocks(self.name_blocks, &mut rng.fork("names"));
        let mut data_rng = rng.fork("data");
        let anchors = data::write_data(
            &mut mem,
            &profile,
            &reflection,
            &mut names,
            &code,
            &mut data_rng,
        );
        write_strings(&mut mem, &profile, &mut rng.fork("strings"));

        // Corruption works on the finished memory, before it is split into parts.
        let unmapped = mem.heap.unmapped_address();
        let mut corrupt_rng = rng.fork("corrupt");
        for &corruption in &self.corruptions {
            let ctx = corrupt::Context {
                profile: &profile,
                reflection: &reflection,
                data: &anchors,
                names: &names,
                unmapped,
            };
            corrupt::apply(&mut mem, &ctx, corruption, &mut corrupt_rng);
        }

        let lens = [mem.text.len(), mem.rdata.len(), mem.data.len()];
        let (mut parts, heap_span, _) = mem.into_parts();
        let rsrc = pe::build_rsrc(&profile.identity);
        let in_snapshot = matches!(self.placement, Placement::Snapshot);
        let module_size = if in_snapshot {
            RSRC_RVA + (rsrc.len() as u64).next_multiple_of(PAGE)
        } else {
            let end = addrs.text.max(addrs.rdata).max(addrs.data);
            (end.saturating_sub(addrs.module_base) + 0x4_0000).max(PAGE)
        };
        let mut region_parts = parts.clone();
        if in_snapshot {
            let entry = code.entry - addrs.module_base;
            let header = pe::build_headers(&addrs, [lens[0], lens[1], lens[2], rsrc.len()], entry);
            let mut rsrc_bytes = rsrc.clone();
            rsrc_bytes.resize(rsrc_bytes.len().next_multiple_of(PAGE as usize), 0);
            region_parts.push(Part {
                kind: PartKind::Headers,
                addr: addrs.module_base,
                bytes: header,
                perms: Perms::R,
                region_kind: RegionKind::Image,
            });
            region_parts.push(Part {
                kind: PartKind::Rsrc,
                addr: addrs.rsrc,
                bytes: rsrc_bytes,
                perms: Perms::R,
                region_kind: RegionKind::Image,
            });
        }
        let mut snapshot = Snapshot::builder();
        for part in &region_parts {
            snapshot =
                snapshot.region_with(part.addr, part.bytes.clone(), part.perms, part.region_kind);
        }
        let memory = snapshot
            .module(&addrs.module_name, addrs.module_base, module_size)
            .build()
            .expect("parts do not overlap");
        parts.sort_by_key(|part| part.addr);

        let rva = |addr: u64| addrs.rva(addr);
        let mut layout = profile.layout.clone();
        layout.objects.location = Location::Rva {
            module: addrs.module_name.clone(),
            rva: rva(anchors.object_array),
        };
        let names_location = Location::Rva {
            module: addrs.module_name.clone(),
            rva: rva(anchors.name_anchor),
        };
        match &mut layout.names {
            NameTableLayout::Pool(pool) => pool.location = names_location,
            NameTableLayout::Array(array) => array.location = names_location,
        }
        let globals = ExpectedGlobals {
            module: addrs.module_name.clone(),
            gworld_rva: rva(anchors.gworld),
            gengine_rva: rva(anchors.gengine),
            process_event_index: profile.shape.process_event_index,
            process_event_rva: rva(code.process_event),
        };
        let engine = ExpectedEngine {
            claimed: in_snapshot.then(|| profile.identity.product_version.clone()),
            build: profile.identity.build_string(),
            changelist: profile.identity.changelist,
        };
        let (structs, enums) = reflection.schema.normalized();
        let Reflection {
            records,
            world,
            engine: engine_object,
            stale_world,
            native_rvas,
            fillers,
            ..
        } = reflection;
        Fixture {
            profile,
            memory,
            layout,
            globals,
            engine,
            truth: Truth { structs, enums },
            objects: records,
            anchors: Anchors {
                data: anchors,
                process_event: code.process_event,
                decoy_functions: code.decoys.clone(),
                world,
                engine: engine_object,
                stale_world,
                unmapped,
                fillers,
                native_rvas,
            },
            corruptions: self.corruptions,
            parts,
            heap_span,
            addresses: addrs,
        }
    }
}

/// Writes strings and tables into `.rdata`: the UTF-16 build string, the
/// engine version string, and some ordinary text.
fn write_strings(mem: &mut Mem, profile: &Profile, rng: &mut Rng) {
    let build = profile.identity.build_string();
    let wide: Vec<u8> = build
        .encode_utf16()
        .chain(std::iter::once(0))
        .flat_map(u16::to_le_bytes)
        .collect();
    let addr = mem.rdata.alloc(wide.len(), 8);
    mem.put(addr, &wide);

    let ascii = format!(
        "{}-{}+{}\0",
        profile.identity.product_version, profile.identity.changelist, profile.identity.branch
    );
    let addr = mem.rdata.alloc(ascii.len(), 8);
    mem.put(addr, ascii.as_bytes());

    const TEXTS: [&str; 6] = [
        "Assertion failed: %s [File:%s] [Line: %d]",
        "LogInit: Display: Engine initialised",
        "Failed to load package '%s'",
        "Unable to find class '%s'",
        "%s:%s",
        "bAllowGarbageCollection",
    ];
    for _ in 0..12 {
        let text = rng.pick(&TEXTS).copied().unwrap_or("text");
        let addr = mem.rdata.alloc(text.len() + 1, 4);
        mem.put(addr, text.as_bytes());
    }
}
