//! The entry point: runs the phases in order and assembles the dump.

use std::collections::HashMap;
use std::time::Instant;

use ufathom_engine::{LoadOptions, NameTable, ObjectArrayView, for_layout};
use ufathom_mem::{MemoryReader, PageCache};
use ufathom_model::layout::Location;
use ufathom_model::{Dump, EngineLayout, RuntimeGlobals, Severity, TargetInfo, ToolInfo};
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
    dump.globals = runtime_globals(&discovery.globals);
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

/// The width of an enum is the widest of: the smallest integer that holds
/// all its values (see [`values_width`]), and the underlying integer of any
/// property that uses it. The enum object does not store its width, so an enum
/// no property refers to would otherwise get 1 byte whatever its values.
fn finish_enum_sizes(enums: &mut [ufathom_model::EnumDef], uses: &[(String, u32)]) {
    let mut widest: HashMap<&str, u32> = HashMap::new();
    for (path, size) in uses {
        let slot = widest.entry(path.as_str()).or_insert(0);
        *slot = (*slot).max(*size);
    }
    for def in enums {
        let used = widest.get(def.path.as_str()).copied().unwrap_or(0);
        def.underlying_size = used.max(values_width(&def.values)).max(1);
    }
}

/// The smallest width in bytes (1, 2, 4 or 8) that holds every value, as a
/// signed integer when any value is negative.
///
/// The trailing `_MAX` entry that the engine appends is left out when it is
/// exactly one past the range of the others (`256` for a byte enum): it is a
/// count, not a value the enum can hold, and counting it would widen every
/// byte enum.
fn values_width(values: &[ufathom_model::EnumValue]) -> u32 {
    let signed = values.iter().any(|v| v.value < 0);
    let fit = |values: &mut dyn Iterator<Item = i64>| -> u32 {
        let (mut min, mut max) = (0i64, 0i64);
        for v in values {
            min = min.min(v);
            max = max.max(v);
        }
        [1u32, 2, 4]
            .into_iter()
            .find(|&w| {
                let bits = 8 * w;
                if signed {
                    min >= -(1i64 << (bits - 1)) && max < (1i64 << (bits - 1))
                } else {
                    max < (1i64 << bits)
                }
            })
            .unwrap_or(8)
    };
    let all = fit(&mut values.iter().map(|v| v.value));
    let Some(top) = values.iter().max_by_key(|v| v.value) else {
        return 1;
    };
    if !top.name.ends_with("_MAX") {
        return all;
    }
    let others = fit(&mut values
        .iter()
        .filter(|v| !std::ptr::eq(*v, top))
        .map(|v| v.value));
    let bits = 8 * others;
    let one_past = if signed {
        1i64 << (bits - 1)
    } else {
        1i64 << bits
    };
    if others < 8 && top.value == one_past {
        others
    } else {
        all
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

/// The globals a generated SDK uses in process. A global with several
/// candidate slots is left out: guessing would hand the SDK a wrong object.
fn runtime_globals(globals: &ufathom_scan::Globals) -> RuntimeGlobals {
    let only = |slots: &[Location]| match slots {
        [one] => Some(one.clone()),
        _ => None,
    };
    RuntimeGlobals {
        process_event_index: globals.process_event_index,
        gworld: only(&globals.gworld_slots),
        gengine: only(&globals.gengine_slots),
    }
}

#[cfg(test)]
mod width_tests {
    use super::*;
    use ufathom_model::{EnumDef, EnumValue};

    fn values(list: &[(&str, i64)]) -> Vec<EnumValue> {
        list.iter()
            .map(|&(name, value)| EnumValue {
                name: name.to_owned(),
                value,
            })
            .collect()
    }

    #[test]
    fn width_holds_every_value() {
        assert_eq!(values_width(&values(&[("A", 0), ("B", 255)])), 1);
        assert_eq!(values_width(&values(&[("A", 0), ("B", 256)])), 2);
        assert_eq!(values_width(&values(&[("A", 0), ("B", 65_536)])), 4);
        assert_eq!(values_width(&values(&[("A", 1_934_772_034)])), 4);
        assert_eq!(values_width(&values(&[("A", 0), ("B", 1 << 40)])), 8);
        assert_eq!(values_width(&[]), 1);
    }

    #[test]
    fn negative_values_make_it_signed() {
        assert_eq!(values_width(&values(&[("A", -1), ("B", 127)])), 1);
        assert_eq!(values_width(&values(&[("A", -1), ("B", 128)])), 2);
        assert_eq!(values_width(&values(&[("A", -129), ("B", 1)])), 2);
        assert_eq!(values_width(&values(&[("A", -1), ("B", 40_000)])), 4);
    }

    #[test]
    fn a_trailing_max_one_past_the_range_does_not_widen() {
        assert_eq!(
            values_width(&values(&[("A", 0), ("B", 255), ("E_MAX", 256)])),
            1
        );
        assert_eq!(
            values_width(&values(&[("A", 0), ("B", 65_535), ("E_MAX", 65_536)])),
            2
        );
        // A `_MAX` that is a real, larger value still counts.
        assert_eq!(
            values_width(&values(&[("A", 0), ("B", 3), ("E_MAX", 300)])),
            2
        );
        // Only a trailing `_MAX` is special.
        assert_eq!(values_width(&values(&[("A", 0), ("Big", 256)])), 2);
    }

    #[test]
    fn unreferenced_enum_takes_its_width_from_its_values() {
        let mut enums = vec![
            EnumDef {
                path: "/P.Wide".to_owned(),
                name: "Wide".to_owned(),
                package: "/P".to_owned(),
                underlying_size: 1,
                values: values(&[("A", 1_934_772_034)]),
            },
            EnumDef {
                path: "/P.Used".to_owned(),
                name: "Used".to_owned(),
                package: "/P".to_owned(),
                underlying_size: 1,
                values: values(&[("A", 1)]),
            },
        ];
        finish_enum_sizes(&mut enums, &[("/P.Used".to_owned(), 4)]);
        assert_eq!(enums[0].underlying_size, 4);
        // The property's width wins when it is wider than the values need.
        assert_eq!(enums[1].underlying_size, 4);
    }
}
