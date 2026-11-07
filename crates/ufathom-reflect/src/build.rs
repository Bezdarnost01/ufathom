//! The entry point: runs the phases in order and assembles the dump.

use std::collections::HashMap;
use std::time::Instant;

use ufathom_engine::{LoadOptions, NameTable, ObjectArrayView, for_layout};
use ufathom_mem::{MemoryReader, PageCache};
use ufathom_model::layout::Location;
use ufathom_model::{Dump, EngineLayout, Severity, TargetInfo, ToolInfo};
use ufathom_scan::Discovery;

use crate::classify::classify;
use crate::ctx::{Ctx, Tally, diagnostic};
use crate::error::ReflectError;
use crate::objects::read_table;
use crate::options::{ReflectOptions, ReflectOutput, ReflectStats};
use crate::par::Exec;
use crate::parse::{Env, ModuleSpan, Reader};
use crate::types::{build_paths, parse_all};

/// Builds a [`Dump`] of everything `discovery` describes.
///
/// Reads only through `mem`; nothing is written, called or hooked. A problem
/// with one object becomes a diagnostic in the dump, never an error. See the
/// [crate documentation](crate) for the phases and for the choices the dump
/// format leaves open.
///
/// The pool in `options` is the only source of threads, and the global rayon
/// pool is never touched. The dump does not depend on the pool, on its size
/// or on scheduling.
///
/// # Errors
///
/// - [`ReflectError::Cancelled`] when `options.cancel` was set.
/// - [`ReflectError::Names`] when the name table cannot be loaded.
/// - [`ReflectError::ObjectArray`] when the object array header is unreadable
///   or inconsistent, for example a chunk count above the chunk table.
/// - [`ReflectError::NoObjects`] for an empty array.
/// - [`ReflectError::MissingCoreClass`] when `Class`, `ScriptStruct`, `Enum` or
///   `Function` of CoreUObject is not among the objects.
pub fn build_dump(
    mem: &dyn MemoryReader,
    discovery: &Discovery,
    options: &ReflectOptions<'_>,
) -> Result<ReflectOutput, ReflectError> {
    let started = Instant::now();
    let layout = &discovery.layout;
    let mut stats = ReflectStats::default();

    // One cache per pass: dropped with it, so no dump sees an older one's pages.
    let cache = PageCache::new(mem);
    let cached: &dyn MemoryReader = &cache;
    let ctx = Ctx {
        raw: mem,
        cached,
        layout,
        exec: Exec(options.pool),
        cancel: options.cancel,
    };
    ctx.check()?;

    let phase = Instant::now();
    let names = NameTable::load(
        mem,
        &layout.names,
        &LoadOptions::for_layout(layout, options.pool),
    )
    .map_err(|source| ReflectError::Names { source })?;
    stats.timings.names = phase.elapsed();
    let mut tally = Tally::default();
    note_name_problems(&names, &mut tally);
    ctx.check()?;

    let view = ObjectArrayView::new(mem, &layout.objects)
        .map_err(|source| ReflectError::ObjectArray { source })?;
    if view.num() == 0 {
        return Err(ReflectError::NoObjects);
    }

    let phase = Instant::now();
    let table = read_table(&ctx, &view, &mut tally)?;
    stats.timings.headers = phase.elapsed();
    stats.objects = table.occupied;
    stats.null_slots = table.null_slots;
    tracing::debug!(
        objects = table.occupied,
        valid = table.entries.len(),
        "object headers read"
    );

    let phase = Instant::now();
    let (classes, mut packages) = classify(&ctx, &names, &table, &mut tally)?;
    stats.timings.classify = phase.elapsed();

    let phase = Instant::now();
    let (paths, types) = build_paths(&ctx, &names, &table, &classes, &mut tally)?;
    stats.timings.paths = phase.elapsed();
    tracing::debug!(types = types.len(), paths = paths.len(), "types found");

    let phase = Instant::now();
    let env = Env {
        layout,
        names: &names,
        table: &table,
        paths: &paths,
        view: &view,
        raw: mem,
        module: native_module(mem, layout),
    };
    let cached_reader = Reader {
        mem: cached,
        props: for_layout(cached, layout, &names),
    };
    let raw_reader = Reader {
        mem,
        props: for_layout(mem, layout, &names),
    };
    let mut parsed = parse_all(&ctx, &env, &cached_reader, &raw_reader, &types, &mut tally)?;
    stats.timings.parse = phase.elapsed();

    let phase = Instant::now();
    finish_enum_sizes(&mut parsed.enums, &parsed.enum_uses);
    packages.extend(parsed.structs.iter().map(|def| def.package.clone()));
    packages.extend(parsed.enums.iter().map(|def| def.package.clone()));

    let mut dump = Dump::new(
        ToolInfo {
            name: options.tool_name.clone(),
            version: options.tool_version.clone(),
        },
        target_info(discovery),
        discovery.engine.clone(),
        layout.clone(),
    );
    for def in &parsed.structs {
        match def.kind {
            ufathom_model::StructKind::Class => stats.classes += 1,
            ufathom_model::StructKind::ScriptStruct => stats.structs += 1,
        }
    }
    stats.enums = parsed.enums.len() as u64;
    stats.functions = parsed.functions;
    dump.packages = packages;
    dump.structs = parsed.structs;
    dump.enums = parsed.enums;
    dump.diagnostics = tally.diagnostics;
    dump.normalize();
    stats.timings.assemble = phase.elapsed();

    stats.failures = tally.failures;
    stats.rereads = tally.rereads;
    let cache_stats = cache.stats();
    stats.cache_hits = cache_stats.hits;
    stats.cache_misses = cache_stats.misses;
    let degraded = stats.failures as f64 > options.degraded_threshold * stats.objects as f64;
    stats.timings.total = started.elapsed();
    Ok(ReflectOutput {
        dump,
        degraded,
        stats,
    })
}

/// The width of an enum is the widest underlying integer of any property that
/// uses it.
fn finish_enum_sizes(enums: &mut [ufathom_model::EnumDef], uses: &[(String, u32)]) {
    let mut widest: HashMap<&str, u32> = HashMap::new();
    for (path, size) in uses {
        let slot = widest.entry(path.as_str()).or_insert(0);
        *slot = (*slot).max(*size);
    }
    for def in enums {
        if let Some(&size) = widest.get(def.path.as_str()) {
            def.underlying_size = size.max(1);
        }
    }
}

/// The module and build string a dump says it was taken from.
fn target_info(discovery: &Discovery) -> TargetInfo {
    let module = if discovery.globals.module.is_empty() {
        match &discovery.layout.objects.location {
            Location::Rva { module, .. } => module.clone(),
            Location::Absolute(_) => String::new(),
        }
    } else {
        discovery.globals.module.clone()
    };
    TargetInfo {
        module,
        build_id: discovery
            .engine
            .build
            .clone()
            .unwrap_or_else(|| "unknown".to_owned()),
    }
}

/// The module that holds the object array: the one the layout names, or for
/// an absolute location the one that contains the address.
fn native_module(mem: &dyn MemoryReader, layout: &EngineLayout) -> Option<ModuleSpan> {
    let modules = mem.modules().ok()?;
    let found = match &layout.objects.location {
        Location::Rva { module, .. } => {
            let wanted = file_name(module);
            modules
                .iter()
                .find(|candidate| file_name(&candidate.name).eq_ignore_ascii_case(wanted))
        }
        Location::Absolute(addr) => modules.iter().find(|candidate| {
            addr.checked_sub(candidate.base)
                .is_some_and(|offset| offset < candidate.size)
        }),
    }?;
    Some(ModuleSpan {
        base: found.base,
        size: found.size,
    })
}

fn file_name(path: &str) -> &str {
    path.rsplit(['\\', '/']).next().unwrap_or(path)
}

/// Name blocks that did not load completely leave some names unresolved. The
/// objects that need them are reported when they fail; this says why.
fn note_name_problems(names: &NameTable, tally: &mut Tally) {
    let stats = names.stats();
    let blocks = stats
        .unreadable_blocks
        .iter()
        .map(|problem| ("unreadable", problem))
        .chain(
            stats
                .damaged_blocks
                .iter()
                .map(|problem| ("damaged", problem)),
        );
    for (what, problem) in blocks {
        tally.diagnostics.push(diagnostic(
            "",
            Severity::Warning,
            format!(
                "name block {} at {:#x} is {what}, its names are missing or cut short: {}",
                problem.block, problem.addr, problem.reason
            ),
        ));
    }
    if stats.unresolved_numbered > 0 || stats.unreadable_entries > 0 {
        tally.diagnostics.push(diagnostic(
            "",
            Severity::Warning,
            format!(
                "{} numbered names and {} name entries could not be resolved",
                stats.unresolved_numbered, stats.unreadable_entries
            ),
        ));
    }
}
