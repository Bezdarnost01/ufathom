//! `[preload]` against a fake engine: the calls the plan makes, what is
//! loaded, and the description a runner builds a session from.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "tests may panic"
)]

use std::collections::HashMap;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ufathom_agent::game::{GameTask, GameThreadRunner, engine_desc};
use ufathom_agent::preload::{self, PreloadReport};
use ufathom_agent::{DEFAULT_PRELOAD_CLASSES, PreloadConfig};
use ufathom_mem::{MemoryReader, SelfMemory};
use ufathom_model::{
    Dump, EngineInfo, FieldDef, FunctionDef, PropertyType, StructDef, StructKind, TargetInfo,
    ToolInfo,
};
use ufathom_runtime::engine_mem::ARRAY_CLEAR;
use ufathom_runtime::{CallError, Executor, Ptr, Session, UObjectBase};
use ufathom_testkit::{FixtureBuilder, Profile};

const ASSET_SIZE: usize = 0x20;
/// Offset of the fake asset's id inside `FAssetData`.
const ASSET_ID: usize = 0x10;
/// Offset of `AssetClass` inside `FAssetData`.
const ASSET_CLASS: usize = 0x18;

fn field(name: &str, offset: u32, size: u32, ty: PropertyType) -> FieldDef {
    FieldDef {
        name: name.to_owned(),
        offset,
        ty,
        element_size: size,
        array_dim: 1,
        flags: 0x80,
    }
}

fn native_bool() -> PropertyType {
    PropertyType::Bool {
        byte_offset: 0,
        byte_mask: 0xff,
        field_mask: 0xff,
        field_size: 1,
    }
}

fn class(path: &str, functions: Vec<FunctionDef>, fields: Vec<FieldDef>, size: u32) -> StructDef {
    let (package, name) = path.rsplit_once('.').unwrap();
    StructDef {
        path: path.to_owned(),
        name: name.to_owned(),
        package: package.to_owned(),
        kind: if functions.is_empty() {
            StructKind::ScriptStruct
        } else {
            StructKind::Class
        },
        super_path: None,
        size,
        alignment: 8,
        fields,
        functions,
        flags: 0,
    }
}

fn function(path: &str, params_size: u32, params: Vec<FieldDef>) -> FunctionDef {
    FunctionDef {
        path: path.to_owned(),
        name: path.rsplit_once(':').unwrap().1.to_owned(),
        flags: 0,
        params_size,
        params,
        native_rva: None,
    }
}

/// A dump with the registry functions and an `FAssetData` of 0x20 bytes,
/// laid out as a UE 4.25 build with an 8-byte `FName`.
fn registry_dump() -> Dump {
    let layout = Profile::by_name("ue4.25").unwrap().layout;
    assert_eq!(layout.fname.size, 8);
    let mut dump = Dump::new(
        ToolInfo {
            name: "ufathom".to_owned(),
            version: "test".to_owned(),
        },
        TargetInfo {
            module: "Game.exe".to_owned(),
            build_id: "test".to_owned(),
        },
        EngineInfo {
            claimed: None,
            build: None,
            min_version: None,
            max_version: None,
            evidence: Vec::new(),
        },
        layout,
    );
    let u64_ty = || PropertyType::UInt64;
    dump.structs = vec![
        class(
            preload::HELPERS,
            vec![
                function(
                    preload::GET_ASSET_REGISTRY,
                    16,
                    vec![field("ReturnValue", 0, 16, u64_ty())],
                ),
                function(
                    preload::GET_ASSET,
                    0x28,
                    vec![
                        field("InAssetData", 0, 0x20, u64_ty()),
                        field("ReturnValue", 0x20, 8, u64_ty()),
                    ],
                ),
            ],
            Vec::new(),
            0x28,
        ),
        class(
            "/Script/AssetRegistry.AssetRegistry",
            vec![function(
                preload::GET_ASSETS_BY_PATH,
                0x20,
                vec![
                    field("PackagePath", 0, 8, PropertyType::Name),
                    field("OutAssetData", 8, 16, u64_ty()),
                    field("bRecursive", 0x18, 1, native_bool()),
                    field("bIncludeOnlyOnDiskAssets", 0x19, 1, native_bool()),
                    field("ReturnValue", 0x1a, 1, native_bool()),
                ],
            )],
            Vec::new(),
            0x28,
        ),
        class(
            preload::STRING_LIBRARY,
            vec![function(
                preload::STRING_TO_NAME,
                0x18,
                vec![
                    field("InString", 0, 16, PropertyType::Str),
                    field("ReturnValue", 0x10, 8, PropertyType::Name),
                ],
            )],
            Vec::new(),
            0x28,
        ),
        class(
            preload::ASSET_DATA[1],
            Vec::new(),
            vec![field(
                "AssetClass",
                ASSET_CLASS as u32,
                8,
                PropertyType::Name,
            )],
            ASSET_SIZE as u32,
        ),
    ];
    dump
}

/// Engine state the fake executor keeps.
#[derive(Default)]
struct State {
    names: HashMap<String, u32>,
    /// The engine-owned array `GetAssetsByPath` returns.
    assets: Vec<u8>,
    paths_listed: Vec<(String, bool)>,
    cleared: Vec<u64>,
    loaded: Vec<u64>,
}

struct FakeEngine {
    state: Mutex<State>,
}

const REGISTRY: u64 = 0x5000;
const DEFAULT_OBJECT: u64 = 0x100;

impl FakeEngine {
    /// Assets: (id, class). Names get ids in order of first use.
    fn new(assets: &[(u64, &str)]) -> Self {
        let mut state = State::default();
        for (id, class) in assets {
            let next = u32::try_from(state.names.len() + 1).unwrap();
            let index = *state.names.entry((*class).to_owned()).or_insert(next);
            let mut element = [0u8; ASSET_SIZE];
            element[ASSET_ID..ASSET_ID + 8].copy_from_slice(&id.to_le_bytes());
            element[ASSET_CLASS..ASSET_CLASS + 4].copy_from_slice(&index.to_le_bytes());
            state.assets.extend_from_slice(&element);
        }
        Self {
            state: Mutex::new(state),
        }
    }

    fn name(state: &mut State, text: &str) -> u32 {
        let next = u32::try_from(state.names.len() + 1).unwrap();
        *state.names.entry(text.to_owned()).or_insert(next)
    }

    fn name_text(state: &State, bytes: &[u8]) -> String {
        let index = u32::from_le_bytes(bytes[..4].try_into().unwrap());
        state
            .names
            .iter()
            .find(|(_, i)| **i == index)
            .map(|(n, _)| n.clone())
            .unwrap_or_default()
    }
}

/// Reads an `FString` header from a parameter block through this process's
/// memory.
fn read_fstring(header: &[u8]) -> String {
    let data = u64::from_le_bytes(header[..8].try_into().unwrap());
    let num = usize::try_from(i32::from_le_bytes(header[8..12].try_into().unwrap())).unwrap();
    let mut units = vec![0u8; num * 2];
    SelfMemory::new().read_exact(data, &mut units).unwrap();
    let wide: Vec<u16> = units
        .chunks(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .take_while(|&u| u != 0)
        .collect();
    String::from_utf16(&wide).unwrap()
}

// SAFETY: a test double; it runs no game code.
unsafe impl Executor for FakeEngine {
    fn process_event(
        &self,
        object: Ptr<UObjectBase>,
        function_path: &str,
        params: &mut [u8],
    ) -> Result<(), CallError> {
        let mut state = self.state.lock().unwrap();
        match function_path {
            preload::GET_ASSET_REGISTRY => {
                assert_eq!(object.addr(), DEFAULT_OBJECT);
                params[..8].copy_from_slice(&REGISTRY.to_le_bytes());
            }
            preload::STRING_TO_NAME => {
                let text = read_fstring(&params[..16]);
                let index = Self::name(&mut state, &text);
                params[0x10..0x14].copy_from_slice(&index.to_le_bytes());
            }
            preload::GET_ASSETS_BY_PATH => {
                assert_eq!(object.addr(), REGISTRY);
                let path = Self::name_text(&state, &params[..8]);
                state.paths_listed.push((path, params[0x18] != 0));
                let data = state.assets.as_ptr() as u64;
                let num = i32::try_from(state.assets.len() / ASSET_SIZE).unwrap();
                params[8..16].copy_from_slice(&data.to_le_bytes());
                params[16..20].copy_from_slice(&num.to_le_bytes());
                params[20..24].copy_from_slice(&num.to_le_bytes());
            }
            ARRAY_CLEAR => {
                let data = u64::from_le_bytes(params[..8].try_into().unwrap());
                state.cleared.push(data);
                params[..8].fill(0);
            }
            preload::GET_ASSET => {
                let id = u64::from_le_bytes(params[ASSET_ID..ASSET_ID + 8].try_into().unwrap());
                state.loaded.push(id);
                // Odd ids return their object, even ones load nothing back.
                let returned = if id % 2 == 1 { 0xabc0 + id } else { 0 };
                params[0x20..0x28].copy_from_slice(&returned.to_le_bytes());
            }
            other => panic!("unexpected call of {other}"),
        }
        Ok(())
    }

    fn default_object(&self, _class_path: &str) -> Result<Ptr<UObjectBase>, CallError> {
        Ok(Ptr::new(DEFAULT_OBJECT))
    }
}

/// Runs tasks at once on the calling thread.
struct Inline {
    engine: FakeEngine,
    attached: Mutex<usize>,
    detached: Mutex<usize>,
}

impl GameThreadRunner for Inline {
    fn attach(&self, dump: &Dump) -> Result<(), String> {
        assert!(!dump.structs.is_empty());
        *self.attached.lock().unwrap() += 1;
        Ok(())
    }

    fn run(&self, task: GameTask, _timeout: Duration) -> Result<(), String> {
        task(&self.engine);
        Ok(())
    }

    fn detach(&self, _timeout: Duration) -> bool {
        *self.detached.lock().unwrap() += 1;
        true
    }
}

fn config(max_assets: usize, batch: usize) -> PreloadConfig {
    PreloadConfig {
        paths: vec!["/Game".to_owned()],
        classes: DEFAULT_PRELOAD_CLASSES.map(str::to_owned).to_vec(),
        max_assets,
        batch,
        timeout_seconds: 5,
    }
}

fn run(assets: &[(u64, &str)], config: &PreloadConfig) -> (PreloadReport, Inline) {
    let runner = Inline {
        engine: FakeEngine::new(assets),
        attached: Mutex::new(0),
        detached: Mutex::new(0),
    };
    let report = preload::run(
        &registry_dump(),
        config,
        &runner,
        Arc::new(SelfMemory::new()),
        &AtomicBool::new(false),
    )
    .unwrap();
    (report, runner)
}

#[test]
fn only_blueprint_assets_under_the_path_are_loaded() {
    let assets = [
        (1, "BlueprintGeneratedClass"),
        (2, "Texture2D"),
        (3, "WidgetBlueprint"),
        (4, "Blueprint"),
    ];
    let (report, runner) = run(&assets, &config(100, 2));
    let state = runner.engine.state.lock().unwrap();
    assert_eq!(state.paths_listed, vec![("/Game".to_owned(), true)]);
    assert_eq!(state.loaded, vec![1, 3, 4]);
    // The registry's array buffer went back to the engine.
    assert_eq!(state.cleared, vec![state.assets.as_ptr() as u64]);
    assert_eq!(
        report,
        PreloadReport {
            listed: 4,
            matched: 3,
            attempted: 3,
            returned: 2,
            failed: 0,
            first_error: None,
            truncated: false,
            released: true,
        }
    );
    assert_eq!(*runner.attached.lock().unwrap(), 1);
    assert_eq!(*runner.detached.lock().unwrap(), 1);
}

#[test]
fn max_assets_limits_the_loads_and_says_so() {
    let assets = [(1, "Blueprint"), (2, "Blueprint"), (3, "Blueprint")];
    let (report, runner) = run(&assets, &config(2, 8));
    assert_eq!(runner.engine.state.lock().unwrap().loaded, vec![1, 2]);
    assert_eq!((report.matched, report.attempted), (3, 2));
    assert!(report.truncated);
}

#[test]
fn an_empty_class_list_loads_every_asset() {
    let assets = [(1, "Texture2D"), (2, "World")];
    let mut every = config(10, 1);
    every.classes.clear();
    let (report, runner) = run(&assets, &every);
    assert_eq!(runner.engine.state.lock().unwrap().loaded, vec![1, 2]);
    assert_eq!(report.attempted, 2);
}

#[test]
fn a_build_without_the_registry_functions_is_refused_before_attaching() {
    let mut dump = registry_dump();
    dump.structs.retain(|s| s.path != preload::HELPERS);
    let runner = Inline {
        engine: FakeEngine::new(&[]),
        attached: Mutex::new(0),
        detached: Mutex::new(0),
    };
    let error = preload::run(
        &dump,
        &config(1, 1),
        &runner,
        Arc::new(SelfMemory::new()),
        &AtomicBool::new(false),
    )
    .unwrap_err();
    assert!(error.contains("GetAssetRegistry"), "{error}");
    assert_eq!(*runner.attached.lock().unwrap(), 0);
}

#[test]
fn the_engine_description_opens_a_session_on_the_build() {
    let fixture = FixtureBuilder::new(Profile::by_name("ue4.25-cp").unwrap())
        .seed(5)
        .filler_objects(20)
        .build();
    let mut dump = registry_dump();
    dump.layout = fixture.layout.clone();
    let desc = Box::leak(Box::new(engine_desc(&dump)));
    let session = Session::new(fixture.memory.clone(), desc).unwrap();
    assert!(session.num_objects().unwrap() > 20);
    let object = session.find_object("/Script/CoreUObject.Object").unwrap();
    assert!(object.is_some());
}
