//! Loading assets before a dump (`[preload]` in the config).
//!
//! A dump sees only what the game has loaded. To get classes of other
//! levels, menus and so on, the asset registry is asked for the assets under
//! the configured content paths, and each one whose class matches is loaded
//! with `AssetRegistryHelpers::GetAsset` on the game thread. The dump is then
//! read again.
//!
//! Everything about the calls comes from the dump that was just read: the
//! functions' parameter offsets and block sizes, the size of `FAssetData` and
//! where its class name is. No offset is hard-coded, so the same code works
//! from UE 4.17 (where `GetAssetRegistry` became callable) to UE 5, where
//! the class moved from `AssetClass` to `AssetClassPath`.
//!
//! This is the one place where ufathom calls game functions for a dump, and
//! it is off unless the config asks for it. Loading stalls the game thread
//! for as long as a batch takes. The array of `FAssetData` the registry
//! returns has its buffer freed through the engine, but what each element
//! owns (tag maps, chunk ids) is left allocated: a bounded leak, once per
//! dump.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::time::Duration;

use ufathom_mem::MemoryReader;
use ufathom_model::{Dump, FieldDef, FunctionDef, PropertyType, StructDef};
use ufathom_runtime::{Executor, FStringBuf, Ptr, TArray, UObjectBase, engine_mem};

use crate::config::PreloadConfig;
use crate::game::GameThreadRunner;

/// The class whose default object the static registry helpers run on.
pub const HELPERS: &str = "/Script/AssetRegistry.AssetRegistryHelpers";
/// `UAssetRegistryHelpers::GetAssetRegistry`, which returns the registry.
pub const GET_ASSET_REGISTRY: &str = "/Script/AssetRegistry.AssetRegistryHelpers:GetAssetRegistry";
/// `UAssetRegistryHelpers::GetAsset`, which loads one asset.
pub const GET_ASSET: &str = "/Script/AssetRegistry.AssetRegistryHelpers:GetAsset";
/// `IAssetRegistry::GetAssetsByPath`, which lists the assets under a path.
pub const GET_ASSETS_BY_PATH: &str = "/Script/AssetRegistry.AssetRegistry:GetAssetsByPath";
/// The class whose default object `Conv_StringToName` runs on.
pub const STRING_LIBRARY: &str = "/Script/Engine.KismetStringLibrary";
/// `UKismetStringLibrary::Conv_StringToName`, which makes an `FName`.
pub const STRING_TO_NAME: &str = "/Script/Engine.KismetStringLibrary:Conv_StringToName";
/// Where `FAssetData` is declared: in `CoreUObject` from UE 5.1, before that
/// in `AssetRegistry`.
pub const ASSET_DATA: [&str; 2] = [
    "/Script/CoreUObject.AssetData",
    "/Script/AssetRegistry.AssetData",
];
/// `FTopLevelAssetPath`, the type of `FAssetData::AssetClassPath` from UE 5.1.
pub const TOP_LEVEL_ASSET_PATH: &str = "/Script/CoreUObject.TopLevelAssetPath";

/// Largest parameter block this module builds, in 8-byte words.
const BLOCK_WORDS: usize = 256;
/// Most entries read from one registry answer: a bound on a count the game
/// writes.
const MAX_LISTED: usize = 1_000_000;

/// What a preload did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PreloadReport {
    /// Assets the registry listed under the paths.
    pub listed: usize,
    /// Of those, assets whose class matched.
    pub matched: usize,
    /// Assets `GetAsset` was called for: the matching ones, up to the limit.
    pub attempted: usize,
    /// Calls that returned an object. A cooked Blueprint package can load
    /// its class while the call returns nothing, so this is a lower bound.
    pub returned: usize,
    /// Calls that failed, and why the first one did.
    pub failed: usize,
    /// The first failure, if any.
    pub first_error: Option<String>,
    /// `true` when more assets matched than `max_assets` allows.
    pub truncated: bool,
    /// `true` when the hook was removed and no thread is still inside it.
    pub released: bool,
}

/// One parameter of a planned call.
#[derive(Debug, Clone, Copy)]
struct Param {
    offset: usize,
    size: usize,
    /// For a `bool`: the byte inside the parameter and its mask.
    bool_bit: Option<(usize, u8)>,
}

/// A function to call, with its parameter block layout from the dump.
#[derive(Debug, Clone)]
struct Call {
    path: &'static str,
    size: usize,
    params: HashMap<String, Param>,
}

impl Call {
    fn new(dump: &Dump, path: &'static str) -> Result<Self, String> {
        let function = find_function(dump, path)
            .ok_or_else(|| format!("{path} is not in the dump; this build cannot preload"))?;
        let size = usize::try_from(function.params_size).unwrap_or(usize::MAX);
        if size > BLOCK_WORDS * 8 {
            return Err(format!("{path} takes {size} bytes of parameters, too many"));
        }
        let params = function
            .params
            .iter()
            // Keyed in lowercase: parameter names differ in case between
            // builds (UE 4.21 has `inString`, later builds `InString`), and
            // the engine compares names without case.
            .map(|p| (p.name.to_ascii_lowercase(), param(p)))
            .collect();
        Ok(Self { path, size, params })
    }

    fn param(&self, name: &str) -> Result<Param, String> {
        self.params
            .get(&name.to_ascii_lowercase())
            .copied()
            .ok_or_else(|| format!("{} has no parameter `{name}`", self.path))
    }

    /// A zeroed parameter block.
    fn block(&self) -> Block {
        Block {
            words: [0; BLOCK_WORDS],
            len: self.size,
        }
    }
}

fn param(field: &FieldDef) -> Param {
    let size = usize::try_from(field.element_size.saturating_mul(field.array_dim.max(1)))
        .unwrap_or(usize::MAX);
    let bool_bit = match field.ty {
        PropertyType::Bool {
            byte_offset,
            byte_mask,
            ..
        } => Some((usize::from(byte_offset), byte_mask)),
        _ => None,
    };
    Param {
        offset: usize::try_from(field.offset).unwrap_or(usize::MAX),
        size,
        bool_bit,
    }
}

/// An 8-byte aligned parameter block.
struct Block {
    words: [u64; BLOCK_WORDS],
    len: usize,
}

impl Block {
    fn bytes(&mut self) -> &mut [u8] {
        let len = self.len;
        let all = ufathom_runtime::as_bytes_mut(&mut self.words);
        all.get_mut(..len).unwrap_or_default()
    }

    fn put(&mut self, param: Param, value: &[u8]) -> Result<(), String> {
        if value.len() > param.size {
            return Err(format!(
                "a {}-byte value does not fit a {}-byte parameter",
                value.len(),
                param.size
            ));
        }
        self.bytes()
            .get_mut(param.offset..param.offset + value.len())
            .ok_or_else(|| "parameter outside its block".to_owned())?
            .copy_from_slice(value);
        Ok(())
    }

    fn set_bool(&mut self, param: Param, value: bool) -> Result<(), String> {
        let (byte, mask) = param.bool_bit.unwrap_or((0, 0xff));
        let slot = self
            .bytes()
            .get_mut(param.offset + byte)
            .ok_or_else(|| "parameter outside its block".to_owned())?;
        if value {
            *slot |= mask;
        } else {
            *slot &= !mask;
        }
        Ok(())
    }

    fn get(&mut self, param: Param) -> Result<Vec<u8>, String> {
        self.bytes()
            .get(param.offset..param.offset + param.size)
            .map(<[u8]>::to_vec)
            .ok_or_else(|| "parameter outside its block".to_owned())
    }

    fn get_u64(&mut self, param: Param) -> Result<u64, String> {
        let bytes = self.get(param)?;
        let word: [u8; 8] = bytes
            .get(..8)
            .and_then(|b| b.try_into().ok())
            .ok_or_else(|| format!("a {}-byte parameter is not a pointer", param.size))?;
        Ok(u64::from_le_bytes(word))
    }
}

/// Everything the calls need, taken from the dump.
#[derive(Debug, Clone)]
pub struct Plan {
    get_registry: Call,
    by_path: Call,
    get_asset: Call,
    to_name: Call,
    /// `sizeof(FAssetData)`.
    asset_size: usize,
    /// Offset in `FAssetData` of the `FName` holding the class short name.
    class_name: usize,
    /// Where an `FName` keeps the bytes that decide equality: the comparison
    /// index and, when stored inline, the number.
    name_keys: Vec<(usize, usize)>,
}

impl Plan {
    /// Finds the functions and types a preload calls in `dump`.
    ///
    /// # Errors
    ///
    /// Which function, type or parameter is missing: the build cannot
    /// preload.
    pub fn from_dump(dump: &Dump) -> Result<Self, String> {
        let asset = ASSET_DATA
            .iter()
            .find_map(|path| dump.structs.iter().find(|s| s.path == *path))
            .ok_or("FAssetData is not in the dump")?;
        let class_name = class_name_offset(dump, asset)?;
        let f = &dump.layout.fname;
        let mut name_keys = vec![(usize::try_from(f.comparison_index).unwrap_or(0), 4)];
        if let Some(number) = f.number {
            name_keys.push((usize::try_from(number).unwrap_or(0), 4));
        }
        let plan = Self {
            get_registry: Call::new(dump, GET_ASSET_REGISTRY)?,
            by_path: Call::new(dump, GET_ASSETS_BY_PATH)?,
            get_asset: Call::new(dump, GET_ASSET)?,
            to_name: Call::new(dump, STRING_TO_NAME)?,
            asset_size: usize::try_from(asset.size).unwrap_or(usize::MAX),
            class_name,
            name_keys,
        };
        for (call, names) in [
            (&plan.get_registry, &["ReturnValue"][..]),
            (
                &plan.by_path,
                &[
                    "PackagePath",
                    "OutAssetData",
                    "bRecursive",
                    "bIncludeOnlyOnDiskAssets",
                ][..],
            ),
            (&plan.get_asset, &["InAssetData", "ReturnValue"][..]),
            (&plan.to_name, &["InString", "ReturnValue"][..]),
        ] {
            for name in names {
                call.param(name)?;
            }
        }
        if plan.get_asset.param("InAssetData")?.size < plan.asset_size {
            return Err("GetAsset's InAssetData is smaller than FAssetData".to_owned());
        }
        Ok(plan)
    }

    /// The bytes of an `FName` that decide equality.
    fn name_key(&self, name: &[u8]) -> Option<Vec<u8>> {
        let mut key = Vec::with_capacity(8);
        for &(offset, len) in &self.name_keys {
            key.extend_from_slice(name.get(offset..offset + len)?);
        }
        Some(key)
    }
}

fn find_function<'a>(dump: &'a Dump, path: &str) -> Option<&'a FunctionDef> {
    let (owner, _) = path.split_once(':')?;
    dump.structs
        .iter()
        .find(|s| s.path == owner)?
        .functions
        .iter()
        .find(|f| f.path == path)
}

/// Offset in `FAssetData` of the `FName` with the asset class's short name:
/// `AssetClassPath.AssetName` from UE 5.1, `AssetClass` before.
fn class_name_offset(dump: &Dump, asset: &StructDef) -> Result<usize, String> {
    let field =
        |owner: &StructDef, name: &str| owner.fields.iter().find(|f| f.name == name).cloned();
    if let Some(path) = field(asset, "AssetClassPath") {
        let top = dump
            .structs
            .iter()
            .find(|s| s.path == TOP_LEVEL_ASSET_PATH)
            .ok_or("FTopLevelAssetPath is not in the dump")?;
        let name = field(top, "AssetName").ok_or("FTopLevelAssetPath has no AssetName")?;
        return usize::try_from(path.offset + name.offset).map_err(|e| e.to_string());
    }
    let class =
        field(asset, "AssetClass").ok_or("FAssetData has neither AssetClassPath nor AssetClass")?;
    if !matches!(class.ty, PropertyType::Name) {
        return Err("FAssetData::AssetClass is not an FName".to_owned());
    }
    usize::try_from(class.offset).map_err(|e| e.to_string())
}

/// What the listing step found, sent back from the game thread.
struct Listing {
    listed: usize,
    matched: usize,
    assets: Vec<Vec<u8>>,
    truncated: bool,
}

/// Calls `function` on `object` with a block filled by `fill`, and returns
/// the block.
fn call(
    exec: &dyn Executor,
    object: Ptr<UObjectBase>,
    function: &Call,
    fill: impl FnOnce(&mut Block) -> Result<(), String>,
) -> Result<Block, String> {
    let mut block = function.block();
    fill(&mut block)?;
    exec.process_event(object, function.path, block.bytes())
        .map_err(|e| e.to_string())?;
    Ok(block)
}

/// `FName` bytes of `text`, made by the engine.
fn name_of(
    exec: &dyn Executor,
    plan: &Plan,
    library: Ptr<UObjectBase>,
    text: &str,
) -> Result<Vec<u8>, String> {
    let string = FStringBuf::new(text);
    let mut value = string.as_fstring();
    let bytes = ufathom_runtime::as_bytes_mut(&mut value).to_vec();
    let to_name = &plan.to_name;
    let mut block = call(exec, library, to_name, |b| {
        b.put(to_name.param("InString")?, &bytes)
    })?;
    // `string` owns the characters the call read; it lives until here.
    drop(string);
    block.get(to_name.param("ReturnValue")?)
}

/// Lists the matching assets. Runs on the game thread.
fn list(
    exec: &dyn Executor,
    reader: &dyn MemoryReader,
    plan: &Plan,
    config: &PreloadConfig,
) -> Result<Listing, String> {
    let helpers = exec.default_object(HELPERS).map_err(|e| e.to_string())?;
    let library = exec
        .default_object(STRING_LIBRARY)
        .map_err(|e| e.to_string())?;
    let mut block = call(exec, helpers, &plan.get_registry, |_| Ok(()))?;
    let registry = block.get_u64(plan.get_registry.param("ReturnValue")?)?;
    if registry == 0 {
        return Err("GetAssetRegistry returned no registry".to_owned());
    }
    let registry = Ptr::<UObjectBase>::new(registry);

    let mut wanted = Vec::with_capacity(config.classes.len());
    for class in &config.classes {
        let name = name_of(exec, plan, library, class)?;
        wanted.push(
            plan.name_key(&name)
                .ok_or("an FName is shorter than its layout")?,
        );
    }
    let mut listing = Listing {
        listed: 0,
        matched: 0,
        assets: Vec::new(),
        truncated: false,
    };
    let by_path = &plan.by_path;
    let out = by_path.param("OutAssetData")?;
    for path in &config.paths {
        let package_path = name_of(exec, plan, library, path)?;
        let mut block = call(exec, registry, by_path, |b| {
            b.put(by_path.param("PackagePath")?, &package_path)?;
            b.set_bool(by_path.param("bRecursive")?, true)?;
            b.set_bool(by_path.param("bIncludeOnlyOnDiskAssets")?, false)
        })?;
        let header = block.get(out)?;
        let mut array: TArray<u8> = TArray {
            data: Ptr::new(word(&header, 0)?),
            num: i32::from_le_bytes(
                header
                    .get(8..12)
                    .and_then(|b| b.try_into().ok())
                    .ok_or("short TArray")?,
            ),
            max: i32::from_le_bytes(
                header
                    .get(12..16)
                    .and_then(|b| b.try_into().ok())
                    .ok_or("short TArray")?,
            ),
        };
        let count = usize::try_from(array.num).unwrap_or(0).min(MAX_LISTED);
        listing.listed += count;
        let mut element = vec![0u8; plan.asset_size];
        for i in 0..count {
            let addr = (i as u64)
                .checked_mul(plan.asset_size as u64)
                .and_then(|o| array.data.addr().checked_add(o))
                .ok_or("asset list address overflows")?;
            if reader.read_exact(addr, &mut element).is_err() {
                continue;
            }
            let matches = wanted.is_empty()
                || element
                    .get(plan.class_name..)
                    .and_then(|name| plan.name_key(name))
                    .is_some_and(|key| wanted.contains(&key));
            if !matches {
                continue;
            }
            listing.matched += 1;
            if listing.assets.len() < config.max_assets {
                listing.assets.push(element.clone());
            } else {
                listing.truncated = true;
            }
        }
        // The elements are not destroyed; see the module documentation.
        engine_mem::free_array(exec, &mut array).map_err(|e| e.to_string())?;
    }
    Ok(listing)
}

fn word(bytes: &[u8], at: usize) -> Result<u64, String> {
    bytes
        .get(at..at + 8)
        .and_then(|b| b.try_into().ok())
        .map(u64::from_le_bytes)
        .ok_or_else(|| "short value".to_owned())
}

/// Loads `assets`. Runs on the game thread. Returns how many calls returned
/// an object, and the errors of the others.
fn load(exec: &dyn Executor, plan: &Plan, assets: &[Vec<u8>]) -> (usize, Vec<String>) {
    let mut returned = 0;
    let mut errors = Vec::new();
    let helpers = match exec.default_object(HELPERS) {
        Ok(helpers) => helpers,
        Err(error) => return (0, vec![error.to_string(); assets.len()]),
    };
    let get_asset = &plan.get_asset;
    for asset in assets {
        let result = get_asset.param("InAssetData").and_then(|input| {
            let mut block = call(exec, helpers, get_asset, |b| b.put(input, asset))?;
            block.get_u64(get_asset.param("ReturnValue")?)
        });
        match result {
            Ok(0) => {}
            Ok(_) => returned += 1,
            Err(error) => errors.push(error),
        }
    }
    (returned, errors)
}

/// Runs `job` on the game thread and returns what it produced.
fn on_game_thread<R: Send + 'static>(
    runner: &dyn GameThreadRunner,
    timeout: Duration,
    job: impl FnOnce(&dyn Executor) -> R + Send + 'static,
) -> Result<R, String> {
    let (sender, receiver) = mpsc::channel();
    runner.run(
        Box::new(move |exec| {
            let _ = sender.send(job(exec));
        }),
        timeout,
    )?;
    receiver
        .try_recv()
        .map_err(|_| "the game thread task ended without a result".to_owned())
}

/// Lists and loads the assets `config` asks for, then removes the hook.
///
/// `cancel` is checked between batches.
///
/// # Errors
///
/// When the build cannot preload or listing fails. Failures of single
/// assets are counted in the report instead.
pub fn run(
    dump: &Dump,
    config: &PreloadConfig,
    runner: &dyn GameThreadRunner,
    reader: Arc<dyn MemoryReader>,
    cancel: &AtomicBool,
) -> Result<PreloadReport, String> {
    let plan = Arc::new(Plan::from_dump(dump)?);
    runner.attach(dump)?;
    let timeout = Duration::from_secs(config.timeout_seconds);
    let mut report = PreloadReport::default();
    let result = (|| {
        let listing = {
            let (plan, config, reader) = (Arc::clone(&plan), config.clone(), Arc::clone(&reader));
            on_game_thread(runner, timeout, move |exec| {
                list(exec, &*reader, &plan, &config)
            })??
        };
        report.listed = listing.listed;
        report.matched = listing.matched;
        report.truncated = listing.truncated;
        for batch in listing.assets.chunks(config.batch.max(1)) {
            if cancel.load(Ordering::Relaxed) {
                break;
            }
            let (plan, batch) = (Arc::clone(&plan), batch.to_vec());
            let count = batch.len();
            let (returned, errors) =
                on_game_thread(runner, timeout, move |exec| load(exec, &plan, &batch))?;
            report.attempted += count;
            report.returned += returned;
            report.failed += errors.len();
            if report.first_error.is_none() {
                report.first_error = errors.into_iter().next();
            }
        }
        Ok::<(), String>(())
    })();
    report.released = runner.detach(timeout);
    result.map(|()| report)
}
