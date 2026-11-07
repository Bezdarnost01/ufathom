//! The entry point: runs the solving steps in order and assembles the layout.

use ufathom_mem::MemoryReader;
use ufathom_model::layout::{FNameLayout, ObjectArrayKind, UObjectLayout, UStructLayout};
use ufathom_model::layout::{NameTableLayout, PropertySystem, UEnumLayout};
use ufathom_model::{EngineInfo, EngineLayout, EngineVersion};

use crate::ctx::Ctx;
use crate::error::ScanError;
use crate::globals::{GlobalsInput, find_globals};
use crate::members::{solve_enum, solve_members};
use crate::names::Names;
use crate::objects::{find_object_array, load_slots};
use crate::options::DiscoverOptions;
use crate::pe::PeImage;
use crate::props::{self, Env};
use crate::result::Discovery;
use crate::structs::{solve_structs, structs_of};
use crate::universe::{Anchors, Universe};
use crate::uobject::solve_object_fields;
use crate::validate::validate;
use crate::version::detect_version;

/// Solves the layout of the target from invariants and returns it with the
/// evidence for every value.
///
/// Reads only through `mem`. Nothing is written, executed or hooked. The
/// module that holds the engine globals is `options.module`, or the first
/// `.exe` module the reader lists.
///
/// # Errors
///
/// A [`ScanError`] names what failed:
///
/// - [`ScanError::ModuleNotFound`] and [`ScanError::NoWritableData`] when the
///   target has nothing to search.
/// - [`ScanError::ObjectArrayNotFound`] and [`ScanError::NameTableNotFound`]
///   when the two global tables are not found. The first message advises a
///   `pointer_transform` when the pointer may be scrambled.
/// - [`ScanError::MissingEvidence`] for an essential field with no evidence,
///   and [`ScanError::Ambiguous`] when evidence fits several values equally.
/// - [`ScanError::OverrideContradiction`] when an override differs from the
///   target.
/// - [`ScanError::ValidationFailed`] when the solved layout breaks a global
///   invariant of the engine for more than 2% of the items checked.
pub fn discover(
    mem: &dyn MemoryReader,
    options: &DiscoverOptions<'_>,
) -> Result<Discovery, ScanError> {
    let ctx = Ctx::new(mem, options)?;

    // Step 1: the object array.
    let array = find_object_array(&ctx)?;
    let slots = load_slots(&ctx, &array.layout)?;

    // Steps 2 and 3: the name table, `FName` and the `UObject` header.
    let names = Names::find(&ctx)?;
    let fields = solve_object_fields(&ctx, &names, &slots, array.index_offset)?;
    drop(names);

    // The objects, and the CoreUObject anchors among them.
    let universe = Universe::scan(&ctx, &slots, &fields);
    let anchors = Anchors::find(&universe, &fields.table, fields.metaclass)
        .map_err(|detail| ScanError::missing("coreuobject anchors", detail))?;

    // Step 4: structs.
    let structs = solve_structs(
        &ctx,
        &universe,
        &fields.table,
        &fields,
        &anchors,
        array.index_offset,
    )?;

    // Step 5: properties, and the size of `FName`.
    let env = Env {
        ctx: &ctx,
        universe: &universe,
        table: &fields.table,
        fname: fields.fname.clone(),
        name_offset: fields.name,
        anchors: &anchors,
        fields: &structs,
        sizes: structs.sizes,
    };
    let solved = props::solve(&env)?;
    let fname = FNameLayout {
        size: solved.fname_size,
        ..fields.fname.clone()
    };
    let env = Env {
        fname: fname.clone(),
        ..env
    };

    // The rest of step 4: classes, functions and enums.
    let members = solve_members(&env, &solved.props, solved.layout.flags)?;
    let uenum = solve_enum(&env, &solved.props)?;

    let layout = EngineLayout {
        pointer_size: 8,
        names: fields.names.clone(),
        objects: array.layout.clone(),
        fname,
        uobject: UObjectLayout {
            flags: structs.uobject_flags,
            index: array.index_offset,
            class: fields.class,
            name: fields.name,
            outer: fields.outer,
            size: structs.uobject_size,
        },
        ufield: structs.ufield.clone(),
        ustruct: UStructLayout {
            child_properties: solved.child_properties,
            ..structs.ustruct.clone()
        },
        uclass: members.uclass,
        ufunction: members.ufunction,
        uenum,
        properties: solved.layout.clone(),
    };

    // Step 6: globals.
    let globals = find_globals(
        &ctx,
        &GlobalsInput {
            universe: &universe,
            super_offset: layout.ustruct.super_struct,
            index_offset: layout.uobject.index,
            object_flags_offset: layout.uobject.flags,
            function_flags_offset: layout.ufunction.flags,
            world: anchors.engine.get("world").copied(),
            game_engine: anchors.engine.get("gameengine").copied(),
            engine: anchors.engine.get("engine").copied(),
        },
    );

    // Step 7: validation.
    let struct_objects = structs_of(&universe, &fields.table, &anchors);
    validate(&ctx, &layout, &universe, &anchors, &struct_objects)?;

    let engine = merge_version_facts(&ctx, mem, engine_facts(&layout));
    let (evidence, warnings) = ctx.take_records();
    Ok(Discovery {
        layout,
        engine,
        globals,
        evidence,
        warnings,
    })
}

/// Adds the version facts of the module's PE header, resources and strings to
/// the structural ones. A module without a readable header leaves the
/// structural facts as they are: missing version facts never fail discovery.
fn merge_version_facts(
    ctx: &Ctx<'_>,
    mem: &dyn MemoryReader,
    mut engine: EngineInfo,
) -> EngineInfo {
    let image = match PeImage::parse_from_memory(mem, ctx.module_base) {
        Ok(image) => image,
        Err(error) => {
            engine.evidence.push(format!(
                "version facts not read: no PE header at the module base ({error})"
            ));
            return engine;
        }
    };
    let facts = detect_version(mem, &image).engine;
    engine.claimed = facts.claimed;
    engine.build = facts.build;
    engine.min_version = engine.min_version.max(facts.min_version);
    engine.evidence.extend(facts.evidence);
    if let (Some(low), Some(high)) = (engine.min_version, engine.max_version)
        && low > high
    {
        engine.evidence.push(format!(
            "contradictory: the version facts ask for at least {low} and the structure for at most {high}"
        ));
    }
    engine
}

fn version(major: u16, minor: u16) -> EngineVersion {
    EngineVersion {
        major,
        minor,
        patch: 0,
    }
}

/// Facts about the engine version that follow from the structure alone.
fn engine_facts(layout: &EngineLayout) -> EngineInfo {
    let mut lows: Vec<(EngineVersion, &str)> = Vec::new();
    let mut highs: Vec<(EngineVersion, &str)> = Vec::new();
    let mut evidence: Vec<String> = Vec::new();
    match &layout.names {
        NameTableLayout::Pool(_) => lows.push((version(4, 23), "names are an FNamePool")),
        NameTableLayout::Array(_) => {
            highs.push((version(4, 22), "names are a TNameEntryArray"));
        }
    }
    match layout.properties.system {
        PropertySystem::Modern { .. } => lows.push((version(4, 25), "properties are FField")),
        PropertySystem::Legacy => highs.push((version(4, 24), "properties are UProperty objects")),
    }
    match layout.objects.kind {
        ObjectArrayKind::Chunked { .. } => {
            lows.push((version(4, 21), "the object array is chunked"));
        }
        ObjectArrayKind::Fixed => highs.push((version(4, 20), "the object array is fixed")),
    }
    if layout.fname.display_index.is_some() {
        lows.push((version(4, 22), "FName has a display index"));
    }
    if matches!(layout.uenum, UEnumLayout::SplitArrays { .. }) {
        lows.push((version(5, 7), "UEnum stores split name and value arrays"));
    }
    let min_version = lows.iter().map(|&(v, _)| v).max();
    let max_version = highs.iter().map(|&(v, _)| v).min();
    for (v, why) in lows {
        evidence.push(format!("at least {}.{}: {why}", v.major, v.minor));
    }
    for (v, why) in highs {
        evidence.push(format!("at most {}.{}: {why}", v.major, v.minor));
    }
    if let (Some(low), Some(high)) = (min_version, max_version)
        && low > high
    {
        evidence.push(format!(
            "contradictory: the structure asks for at least {low} and at most {high}"
        ));
    }
    EngineInfo {
        claimed: None,
        build: None,
        min_version,
        max_version,
        evidence,
    }
}
