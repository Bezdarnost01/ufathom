//! The entry point: runs the solving steps in order and assembles the layout.

use ufathom_mem::MemoryReader;
use ufathom_model::layout::{FNameLayout, Location, ObjectArrayKind, UObjectLayout, UStructLayout};
use ufathom_model::layout::{NameTableLayout, PropertySystem, UEnumLayout};
use ufathom_model::{EngineInfo, EngineLayout, EngineVersion};

use crate::ctx::Ctx;
use crate::error::{RejectedCandidate, ScanError};
use crate::globals::{GlobalsInput, find_globals};
use crate::members::{solve_enum, solve_members};
use crate::names::Names;
use crate::objects::{
    ArrayCandidate, build_result, load_slots, not_found_hint, search_current_module,
};
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
/// Reads only through `mem`. Nothing is written, executed or hooked.
///
/// The object array and the name table are searched for in every module, not
/// only one: a monolithic game keeps them in its executable, a modular build
/// in separate libraries (the object array in `CoreUObject`, the name table in
/// `Core`). `options.module`, or the first `.exe` module the reader lists, is
/// searched first, then the modules whose name says `Core`, then the rest.
/// Each result records its own module in its location.
///
/// Candidates are ranked and tried in order. A candidate that a later step
/// rejects (its objects have no `UObject` header, say) is set aside and the
/// next one is tried; when none works, the error lists every candidate that
/// was rejected and why.
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
/// - [`ScanError::CandidatesRejected`] when several candidate arrays were
///   found and every one failed a later step.
/// - [`ScanError::ValidationFailed`] when the solved layout breaks a global
///   invariant of the engine for more than 5% of the items checked (and at
///   least eight).
pub fn discover(
    mem: &dyn MemoryReader,
    options: &DiscoverOptions<'_>,
) -> Result<Discovery, ScanError> {
    let ctx = Ctx::new(mem, options)?;
    let mut rejected: Vec<RejectedCandidate> = Vec::new();
    let mut slots_seen = 0usize;
    let mut shapes_seen = 0usize;
    let mut with_data = 0usize;
    // The name table is found once and shared by every candidate array.
    let mut names: Option<Result<Names<'_, '_>, String>> = None;
    for index in 0..ctx.module_count() {
        if !ctx.use_module(index) {
            continue;
        }
        with_data += 1;
        // Step 1: the object arrays of this module, best first.
        let search = search_current_module(&ctx);
        slots_seen += search.slots;
        shapes_seen += search.shapes;
        for candidate in &search.candidates {
            match attempt(&ctx, mem, candidate, &mut names) {
                Ok(found) => return Ok(found),
                // A caller's mistake is the same for every candidate.
                Err(
                    error @ (ScanError::OverrideContradiction { .. }
                    | ScanError::InvalidOverride { .. }),
                ) => return Err(error),
                Err(error) => {
                    // Whatever a rejected candidate recorded is not evidence.
                    let _ = ctx.take_records();
                    tracing::debug!(
                        module = index,
                        slot = candidate.slot(),
                        objects = candidate.num(),
                        %error,
                        "candidate object array rejected"
                    );
                    let module = ctx
                        .module(index)
                        .map_or_else(String::new, |m| m.name.clone());
                    rejected.push(RejectedCandidate {
                        module,
                        slot: candidate.slot(),
                        objects: candidate.num(),
                        error: Box::new(error),
                    });
                }
            }
        }
    }
    match rejected.len() {
        0 if with_data == 0 => Err(ScanError::NoWritableData {
            module: ctx.primary().name.clone(),
        }),
        0 => Err(ScanError::ObjectArrayNotFound {
            slots: slots_seen,
            shape_candidates: shapes_seen,
            confirmed: 0,
            hint: not_found_hint(&ctx),
        }),
        1 => match rejected.pop() {
            Some(only) => Err(*only.error),
            None => Err(ScanError::NoWritableData {
                module: ctx.primary().name.clone(),
            }),
        },
        _ => Err(ScanError::CandidatesRejected { rejected }),
    }
}

/// Runs steps 2 to 7 for one candidate object array.
#[allow(
    clippy::too_many_lines,
    reason = "one pass over the solving steps in order"
)]
fn attempt<'c, 'a>(
    ctx: &'c Ctx<'a>,
    mem: &dyn MemoryReader,
    candidate: &ArrayCandidate,
    names: &mut Option<Result<Names<'c, 'a>, String>>,
) -> Result<Discovery, ScanError> {
    let array = build_result(ctx, candidate)?;
    let slots = load_slots(ctx, &array.layout)?;

    // Steps 2 and 3: the name table, `FName` and the `UObject` header.
    if names.is_none() {
        *names = Some(Names::find(ctx).map_err(|error| error.to_string()));
    }
    let found = match names {
        Some(Ok(found)) => found,
        Some(Err(reason)) => {
            return Err(ScanError::NameTableNotFound {
                reason: reason.clone(),
            });
        }
        None => {
            return Err(ScanError::NameTableNotFound {
                reason: "not searched".to_owned(),
            });
        }
    };
    let fields = solve_object_fields(ctx, found, &slots, array.index_offset)?;

    // The objects, and the CoreUObject anchors among them.
    let universe = Universe::scan(ctx, &slots, &fields);
    let anchors = Anchors::find(&universe, &fields.table, fields.metaclass)
        .map_err(|detail| ScanError::missing("coreuobject anchors", detail))?;

    // Step 4: structs.
    let structs = solve_structs(
        ctx,
        &universe,
        &fields.table,
        &fields,
        &anchors,
        array.index_offset,
    )?;

    // Step 5: properties, and the size of `FName`.
    let env = Env {
        ctx,
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
        ctx,
        &GlobalsInput {
            universe: &universe,
            super_offset: layout.ustruct.super_struct,
            index_offset: layout.uobject.index,
            object_flags_offset: layout.uobject.flags,
            function_flags_offset: layout.ufunction.flags,
            world: anchors.engine.get("world").copied(),
            game_engine: anchors.engine.get("gameengine").copied(),
            engine: anchors.engine.get("engine").copied(),
            module: match &layout.objects.location {
                Location::Rva { module, .. } => module.clone(),
                Location::Absolute(_) => ctx.primary().name.clone(),
            },
        },
    );

    // Step 7: validation.
    let struct_objects = structs_of(&universe, &fields.table, &anchors);
    validate(ctx, &layout, &universe, &anchors, &struct_objects)?;

    let engine = merge_version_facts(ctx, mem, engine_facts(&layout));
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
    let image = match PeImage::parse_from_memory(mem, ctx.primary().base) {
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
