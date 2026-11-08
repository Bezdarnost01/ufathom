//! `ufathom inspect-exe` and `ufathom ps`: what the executable files say.

use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, anyhow};
use rayon::prelude::*;
use serde::Serialize;
use ufathom_mem::{ProcessInfo, list_processes};
use ufathom_model::EngineInfo;
use ufathom_scan::{ExeFacts, inspect_exe};

use crate::args::{InspectArgs, PsArgs};
use crate::error::Outcome;

/// `ufathom inspect-exe`.
pub fn inspect(args: &InspectArgs) -> Outcome {
    let facts = inspect_exe(&args.file)
        .with_context(|| format!("cannot inspect {}", args.file.display()))?;
    if args.json {
        let json = to_json(&args.file, &facts);
        let text = serde_json::to_string_pretty(&json)
            .map_err(|error| anyhow!("cannot write the result as JSON: {error}"))?;
        println!("{text}");
    } else {
        print_text(&args.file, &facts);
    }
    Ok(())
}

#[derive(Serialize)]
struct SectionJson<'a> {
    name: &'a str,
    rva: u32,
    virtual_size: u32,
    raw_size: u32,
    executable: bool,
    writable: bool,
}

#[derive(Serialize)]
struct MarkerJson<'a> {
    name: &'a str,
    minimum_version: String,
    reason: &'a str,
}

#[derive(Serialize)]
struct InspectJson<'a> {
    file: String,
    bits: u8,
    image_size: u32,
    entry_point_rva: u32,
    sections: Vec<SectionJson<'a>>,
    engine: &'a EngineInfo,
    markers: Vec<MarkerJson<'a>>,
    looks_packed: bool,
    packing_indicators: &'a [String],
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map_or_else(String::new, |name| name.to_string_lossy().into_owned())
}

fn to_json<'a>(path: &Path, facts: &'a ExeFacts) -> InspectJson<'a> {
    InspectJson {
        file: file_name(path),
        bits: if facts.image.is_64_bit { 64 } else { 32 },
        image_size: facts.image.size_of_image,
        entry_point_rva: facts.image.entry_point_rva,
        sections: facts
            .image
            .sections
            .iter()
            .map(|section| SectionJson {
                name: &section.name,
                rva: section.rva,
                virtual_size: section.virtual_size,
                raw_size: section.raw_size,
                executable: section.is_executable(),
                writable: section.is_writable(),
            })
            .collect(),
        engine: &facts.version.engine,
        markers: facts
            .version
            .markers
            .iter()
            .map(|hit| MarkerJson {
                name: hit.name,
                minimum_version: format!("{}.{}", hit.minimum.major, hit.minimum.minor),
                reason: hit.reason,
            })
            .collect(),
        looks_packed: facts.packing.looks_packed(),
        packing_indicators: &facts.packing.indicators,
    }
}

fn print_text(path: &Path, facts: &ExeFacts) {
    let engine = &facts.version.engine;
    println!("file:          {}", file_name(path));
    println!(
        "image:         PE {}-bit, {} sections, {:#x} bytes mapped, entry {:#x}",
        if facts.image.is_64_bit { 64 } else { 32 },
        facts.image.sections.len(),
        facts.image.size_of_image,
        facts.image.entry_point_rva
    );
    println!(
        "engine:        {}",
        engine.claimed.as_deref().unwrap_or("no version claimed")
    );
    println!(
        "build string:  {}",
        engine.build.as_deref().unwrap_or("none found")
    );
    match engine.min_version {
        Some(minimum) => println!(
            "lower bound:   {}.{} (from strings only that version or later has)",
            minimum.major, minimum.minor
        ),
        None => println!("lower bound:   none"),
    }
    println!(
        "packed:        {}",
        if facts.packing.looks_packed() {
            "looks packed; strings and resources may be missing until run time"
        } else {
            "no sign of packing"
        }
    );
    for indicator in &facts.packing.indicators {
        println!("               - {indicator}");
    }
    println!("sections:");
    for section in &facts.image.sections {
        println!(
            "  {:<8} rva {:#010x}  size {:#010x}  {}{}",
            section.name,
            section.rva,
            section.mapped_size(),
            if section.is_executable() { "x" } else { "-" },
            if section.is_writable() { "w" } else { "-" },
        );
    }
    println!("evidence:");
    for line in &engine.evidence {
        println!("  - {line}");
    }
}

/// What `ps` learned about one executable file.
#[derive(Debug, Clone)]
struct Hint {
    claimed: Option<String>,
    has_build_string: bool,
    has_marker: bool,
}

impl Hint {
    /// The file carries something only an engine build carries.
    fn probable(&self) -> bool {
        self.claimed.is_some() || self.has_build_string || self.has_marker
    }
}

fn hint_of(path: &str) -> Option<Hint> {
    let facts = inspect_exe(path).ok()?;
    Some(Hint {
        claimed: facts.version.engine.claimed.clone(),
        has_build_string: !facts.version.build_strings.is_empty(),
        has_marker: !facts.version.markers.is_empty(),
    })
}

/// Whether `path` is under the Windows folder, whose executables are never
/// Unreal Engine games and are not worth reading.
fn is_system_path(path: &str) -> bool {
    ["SystemRoot", "windir"]
        .iter()
        .filter_map(|variable| std::env::var(variable).ok())
        .any(|root| {
            let root = root.trim_end_matches(['\\', '/']).to_ascii_lowercase();
            !root.is_empty() && path.to_ascii_lowercase().starts_with(&root)
        })
}

/// `ufathom ps`.
pub fn ps(args: &PsArgs) -> Outcome {
    let mut processes = list_processes().context("cannot list the running processes")?;
    processes.sort_by(|a, b| {
        a.name
            .to_ascii_lowercase()
            .cmp(&b.name.to_ascii_lowercase())
            .then(a.pid.cmp(&b.pid))
    });

    // Each distinct file is read once, in parallel; a game's helper processes
    // share one path.
    let mut paths: Vec<&str> = processes
        .iter()
        .filter_map(|process| process.path.as_deref())
        .filter(|path| !is_system_path(path))
        .collect();
    paths.sort_unstable();
    paths.dedup();
    let hints: HashMap<&str, Option<Hint>> = paths
        .par_iter()
        .map(|path| (*path, hint_of(path)))
        .collect();

    let rows = table(&processes, &hints, args.all);
    for row in &rows {
        println!("{row}");
    }
    let shown = rows.len().saturating_sub(1);
    let hidden = processes.len().saturating_sub(shown);
    if hidden > 0 {
        eprintln!("{hidden} other processes not shown; --all lists every process");
    }
    if shown == 0 {
        eprintln!("no probable Unreal Engine process found");
    }
    Ok(())
}

/// The header and the rows to print: every probable target, and with `all`
/// every process.
fn table(processes: &[ProcessInfo], hints: &HashMap<&str, Option<Hint>>, all: bool) -> Vec<String> {
    let mut rows = vec![format!(
        "{:>7}  {:<4}  {:<10}  {:<28}  path",
        "pid", "UE?", "engine", "name"
    )];
    for process in processes {
        let hint = process
            .path
            .as_deref()
            .and_then(|path| hints.get(path))
            .and_then(Option::as_ref);
        let probable = hint.is_some_and(Hint::probable);
        if !probable && !all {
            continue;
        }
        rows.push(format!(
            "{:>7}  {:<4}  {:<10}  {:<28}  {}",
            process.pid,
            if probable { "yes" } else { "" },
            hint.and_then(|hint| hint.claimed.as_deref()).unwrap_or(""),
            process.name,
            process.path.as_deref().unwrap_or("(path hidden)")
        ));
    }
    rows
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "test code may panic on failure"
)]
mod tests {
    use super::*;

    fn process(pid: u32, name: &str, path: Option<&str>) -> ProcessInfo {
        ProcessInfo {
            pid,
            name: name.to_owned(),
            path: path.map(str::to_owned),
        }
    }

    fn engine_hint() -> Option<Hint> {
        Some(Hint {
            claimed: Some("4.25.1".to_owned()),
            has_build_string: true,
            has_marker: false,
        })
    }

    #[test]
    fn probable_means_a_claim_a_build_string_or_a_marker() {
        let none = Hint {
            claimed: None,
            has_build_string: false,
            has_marker: false,
        };
        assert!(!none.probable());
        for hint in [
            Hint {
                claimed: Some("5.4".to_owned()),
                ..none.clone()
            },
            Hint {
                has_build_string: true,
                ..none.clone()
            },
            Hint {
                has_marker: true,
                ..none
            },
        ] {
            assert!(hint.probable());
        }
    }

    #[test]
    fn only_probable_targets_are_listed_unless_all_is_asked_for() {
        let processes = vec![
            process(1, "editor.exe", Some("/apps/editor.exe")),
            process(2, "game.exe", Some("/games/game.exe")),
            process(3, "hidden.exe", None),
            process(4, "unreadable.exe", Some("/other/unreadable.exe")),
        ];
        let mut hints: HashMap<&str, Option<Hint>> = HashMap::new();
        hints.insert("/apps/editor.exe", None);
        hints.insert("/games/game.exe", engine_hint());

        let rows = table(&processes, &hints, false);
        assert_eq!(rows.len(), 2, "{rows:?}");
        assert!(rows[1].contains("game.exe"), "{rows:?}");
        assert!(rows[1].contains("yes"), "{rows:?}");
        assert!(rows[1].contains("4.25.1"), "{rows:?}");

        let rows = table(&processes, &hints, true);
        assert_eq!(rows.len(), 5, "{rows:?}");
        assert!(rows[3].contains("(path hidden)"), "{rows:?}");
        assert!(
            !rows[1].contains("yes") && !rows[4].contains("yes"),
            "{rows:?}"
        );
    }

    #[test]
    fn the_windows_folder_is_never_read() {
        if let Ok(root) = std::env::var("SystemRoot") {
            assert!(is_system_path(&format!(r"{root}\System32\cmd.exe")));
            assert!(is_system_path(&format!(
                r"{}\notepad.exe",
                root.to_uppercase()
            )));
            assert!(!is_system_path(r"D:\games\game.exe"));
        }
    }
}
