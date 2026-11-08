//! `ufathom probe` and `ufathom doctor`: discovery without a dump.

use std::time::Instant;

use serde::Serialize;
use ufathom_agent::DiscoveryRecord;
use ufathom_model::EngineInfo;
use ufathom_model::layout::{Location, NameTableLayout, ObjectArrayKind, PropertySystem};
use ufathom_scan::{Discovery, Evidence, LayoutOverrides};

use crate::args::{DoctorArgs, ProbeArgs};
use crate::error::{CliError, Outcome};
use crate::target::Target;

/// What `probe --json` prints: the engine facts, the layout with its evidence
/// and globals (the same record `identity.json` holds), and the warnings.
#[derive(Serialize)]
struct ProbeJson<'a> {
    engine: &'a EngineInfo,
    #[serde(flatten)]
    discovery: DiscoveryRecord,
    warnings: &'a [String],
}

/// `ufathom probe`.
pub fn probe(args: &ProbeArgs) -> Outcome {
    let target = Target::open(&args.target)?;
    let discovery = target.discover(None)?;
    if args.json {
        let report = ProbeJson {
            engine: &discovery.engine,
            discovery: DiscoveryRecord::new(&discovery),
            warnings: &discovery.warnings,
        };
        let text = serde_json::to_string_pretty(&report)
            .map_err(|error| anyhow::anyhow!("cannot write the probe as JSON: {error}"))?;
        println!("{text}");
    } else {
        println!("target:        {}", target.description);
        print_summary(&discovery);
        println!(
            "evidence:      {} solved fields (`ufathom doctor` lists them)",
            discovery.evidence.len()
        );
        print_warnings(&discovery);
    }
    Ok(())
}

/// `ufathom doctor`.
pub fn doctor(args: &DoctorArgs) -> Outcome {
    let target = Target::open(&args.target)?;
    println!("target: {}", target.description);
    let started = Instant::now();
    let discovery = match target.discover(None) {
        Ok(discovery) => discovery,
        Err(error) => {
            println!("\nDISCOVERY FAILED after {:.1?}", started.elapsed());
            return Err(CliError::Failure(error));
        }
    };
    println!("discovery: solved in {:.1?}\n", started.elapsed());
    print_summary(&discovery);

    println!("\nSolved fields ({}):", discovery.evidence.len());
    print_evidence(&discovery.evidence);
    println!();
    print_warnings(&discovery);
    let missing = missing(&discovery);
    if missing.is_empty() {
        println!("Missing: nothing.");
    } else {
        println!("Missing ({}):", missing.len());
        for line in &missing {
            println!("  - {line}");
        }
    }
    let weak = discovery
        .evidence
        .iter()
        .filter(|evidence| is_partial(evidence))
        .count();
    println!(
        "\nVerdict: layout solved; {} warnings, {} fields with partial agreement, {} items missing.",
        discovery.warnings.len(),
        weak,
        missing.len()
    );
    Ok(())
}

fn print_summary(discovery: &Discovery) {
    let engine = &discovery.engine;
    println!(
        "engine:        {}",
        engine
            .claimed
            .as_deref()
            .unwrap_or("not claimed by the binary")
    );
    if let Some(build) = &engine.build {
        println!("build string:  {build}");
    }
    if let Some(minimum) = engine.min_version {
        println!(
            "at least:      {}.{} (from structure)",
            minimum.major, minimum.minor
        );
    }
    let layout = &discovery.layout;
    let objects = &layout.objects;
    let kind = match objects.kind {
        ObjectArrayKind::Fixed => "fixed".to_owned(),
        ObjectArrayKind::Chunked { elements_per_chunk } => {
            format!("chunked, {elements_per_chunk:#x} per chunk")
        }
    };
    println!(
        "object array:  {kind}; item {:#x} bytes; at {}",
        objects.item_size,
        location(&objects.location)
    );
    let names = match &layout.names {
        NameTableLayout::Pool(pool) => format!(
            "name pool, stride {}, at {}",
            pool.stride,
            location(&pool.location)
        ),
        NameTableLayout::Array(array) => {
            format!("name array, at {}", location(&array.location))
        }
    };
    println!("names:         {names}");
    let properties = match layout.properties.system {
        PropertySystem::Legacy => "UProperty objects",
        PropertySystem::Modern { .. } => "FProperty fields",
    };
    println!(
        "layout:        FName {} bytes; UObject {:#x} bytes; properties are {properties}",
        layout.fname.size, layout.uobject.size
    );
    let globals = &discovery.globals;
    println!(
        "globals:       module {}; GWorld {}; GEngine {}; ProcessEvent {}",
        globals.module,
        candidates(globals.gworld.len(), &globals.gworld_slots),
        candidates(globals.gengine.len(), &globals.gengine_slots),
        globals.process_event_index.map_or_else(
            || "not found".to_owned(),
            |index| format!("vtable index {index}")
        ),
    );
}

fn candidates(in_module: usize, all: &[Location]) -> String {
    match (in_module, all.len()) {
        (0, 0) => "not found".to_owned(),
        (1, 1) => "found".to_owned(),
        (_, total) => format!("{total} candidates"),
    }
}

/// A location as `module+0x1234` or `0x...`.
fn location(location: &Location) -> String {
    match location {
        Location::Rva { module, rva } => format!("{module}+{rva:#x}"),
        Location::Absolute(address) => format!("{address:#x}"),
    }
}

fn print_warnings(discovery: &Discovery) {
    if discovery.warnings.is_empty() {
        println!("Warnings: none.");
        return;
    }
    println!("Warnings ({}):", discovery.warnings.len());
    for warning in &discovery.warnings {
        println!("  - {warning}");
    }
}

/// Fewer matched samples than examined ones.
fn is_partial(evidence: &Evidence) -> bool {
    evidence.matched < evidence.samples
}

fn print_evidence(records: &[Evidence]) {
    let width = records
        .iter()
        .map(|record| record.field.len())
        .max()
        .unwrap_or(0);
    println!(
        "  {:<width$}  {:>12}  {:>13}  method",
        "field", "value", "agreement"
    );
    for record in records {
        let agreement = if record.samples == 0 {
            "n/a".to_owned()
        } else {
            format!("{}/{}", record.matched, record.samples)
        };
        let flag = if is_partial(record) { " (partial)" } else { "" };
        println!(
            "  {:<width$}  {:>12}  {:>13}  {}{flag}",
            record.field,
            format!("{:#x}", record.value),
            agreement,
            record.method
        );
    }
}

/// What discovery did not establish, in words.
///
/// A name that overrides accept, has no evidence record, and applies to the
/// property system found is listed: it was filled in some other way.
fn missing(discovery: &Discovery) -> Vec<String> {
    let mut lines = Vec::new();
    let globals = &discovery.globals;
    if globals.gworld.is_empty() && globals.gworld_slots.is_empty() {
        lines.push("GWorld: no slot found".to_owned());
    }
    if globals.gengine.is_empty() && globals.gengine_slots.is_empty() {
        lines.push("GEngine: no slot found".to_owned());
    }
    if globals.process_event_index.is_none() {
        lines.push("ProcessEvent: vtable index not found".to_owned());
    }
    if globals.process_event_rva.is_none() {
        lines.push("ProcessEvent: address not found".to_owned());
    }
    if discovery.engine.claimed.is_none() {
        lines.push(
            "engine version: the binary does not claim one (see the lower bound above)".to_owned(),
        );
    }
    if discovery.engine.build.is_none() {
        lines.push("build string: none found in the binary".to_owned());
    }
    let modern = matches!(
        discovery.layout.properties.system,
        PropertySystem::Modern { .. }
    );
    for name in LayoutOverrides::NAMES {
        let applies = modern || !(name.starts_with("ffield.") || name.starts_with("field_class."));
        let solved = discovery
            .evidence
            .iter()
            .any(|evidence| evidence.field == *name);
        if applies && !solved {
            lines.push(format!("{name}: no evidence record"));
        }
    }
    lines
}
