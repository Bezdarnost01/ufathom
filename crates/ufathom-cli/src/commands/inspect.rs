//! `ufathom inspect-exe` and `ufathom ps`: what the executable files say.

use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, anyhow};
use rayon::prelude::*;
use serde::Serialize;
use ufathom_mem::{ProcessInfo, list_processes};
use ufathom_model::EngineInfo;
use ufathom_scan::version::BuildString;
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
    /// An engine grade build string, see [`BuildString::is_engine_grade`].
    has_build_string: bool,
    has_marker: bool,
}

impl Hint {
    /// The file carries strong evidence of an engine build: a version claim,
    /// a marker string, or an engine grade build string (a bare `++A+B` is
    /// not enough).
    fn probable(&self) -> bool {
        self.claimed.is_some() || self.has_build_string || self.has_marker
    }
}

fn hint_of(path: &str) -> Option<Hint> {
    inspect_exe(path).ok().map(|facts| hint_from(&facts))
}

fn hint_from(facts: &ExeFacts) -> Hint {
    Hint {
        claimed: facts.version.engine.claimed.clone(),
        has_build_string: facts
            .version
            .build_strings
            .iter()
            .any(BuildString::is_engine_grade),
        has_marker: !facts.version.markers.is_empty(),
    }
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
    // This process is never hinted: its own binary carries the marker strings
    // the solver searches for.
    let own_pid = std::process::id();
    let mut paths: Vec<&str> = processes
        .iter()
        .filter(|process| process.pid != own_pid)
        .filter_map(|process| process.path.as_deref())
        .filter(|path| !is_system_path(path))
        .collect();
    paths.sort_unstable();
    paths.dedup();
    let hints: HashMap<&str, Option<Hint>> = paths
        .par_iter()
        .map(|path| (*path, hint_of(path)))
        .collect();

    let rows = table(&processes, &hints, args.all, own_pid);
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
/// every process. The process `own_pid` is never a target, whatever its file
/// says; with `all` it is listed without a flag.
fn table(
    processes: &[ProcessInfo],
    hints: &HashMap<&str, Option<Hint>>,
    all: bool,
    own_pid: u32,
) -> Vec<String> {
    let mut rows = vec![format!(
        "{:>7}  {:<4}  {:<10}  {:<28}  path",
        "pid", "UE?", "engine", "name"
    )];
    for process in processes {
        let hint = process
            .path
            .as_deref()
            .filter(|_| process.pid != own_pid)
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
    use ufathom_mem::MemoryReaderExt;
    use ufathom_scan::PeImage;
    use ufathom_scan::version::StringEncoding;

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

    /// A build string as the scanner records it, split the same way.
    fn build(text: &str) -> BuildString {
        let body = text.strip_prefix("++").unwrap();
        let (project, rest) = body.split_once('+').unwrap();
        let (branch, changelist) = match rest.rsplit_once("-CL-") {
            Some((branch, digits)) => (branch, digits.parse().ok()),
            None => (rest, None),
        };
        BuildString {
            text: text.to_owned(),
            encoding: StringEncoding::Utf16,
            project: project.to_owned(),
            branch: branch.to_owned(),
            changelist,
            engine_release: None,
            section: ".rdata".to_owned(),
            rva: 0,
        }
    }

    /// Facts of an executable that states nothing about an engine, except
    /// the given build strings.
    fn facts_with_builds(texts: &[&str]) -> ExeFacts {
        // Any PE will do: write the testkit's module out as a file.
        let fx = ufathom_testkit::FixtureBuilder::new(
            ufathom_testkit::Profile::by_name("ue4.25-cp").unwrap(),
        )
        .filler_objects(10)
        .build();
        let base = fx.addresses.module_base;
        let image = PeImage::parse_from_memory(&fx.memory, base).unwrap();
        let mut file = fx
            .memory
            .read_vec::<u8>(base, image.size_of_headers as usize)
            .unwrap();
        for section in &image.sections {
            let data = fx
                .memory
                .read_vec::<u8>(base + u64::from(section.rva), section.raw_size as usize)
                .unwrap();
            file.resize(section.raw_offset as usize, 0);
            file.extend_from_slice(&data);
        }
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("module.exe");
        std::fs::write(&path, file).unwrap();
        let mut facts = inspect_exe(&path).unwrap();
        // Drop what the fixture says; keep only what the test wants.
        facts.version.engine.claimed = None;
        facts.version.markers.clear();
        facts.version.build_strings = texts.iter().map(|text| build(text)).collect();
        facts
    }

    #[test]
    fn a_bare_build_string_is_not_enough_to_flag_a_process() {
        for text in ["++Game+Live", "++VQLHqe8RT6q9OKPv+RKY9ji9i0qVQBDb6Thi"] {
            let hint = hint_from(&facts_with_builds(&[text]));
            assert!(!hint.probable(), "{text}");
        }
        for text in [
            "++Game+Live-CL-123",
            "++UE5+Release-5.4-CL-1",
            "++Game+Release-1.2",
        ] {
            let hint = hint_from(&facts_with_builds(&[text]));
            assert!(hint.probable(), "{text}");
        }
        // One strong string among weak ones is enough.
        let hint = hint_from(&facts_with_builds(&["++Game+Live", "++Game+Live-CL-9"]));
        assert!(hint.probable());
        assert!(!hint_from(&facts_with_builds(&[])).probable());
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

        let rows = table(&processes, &hints, false, 0);
        assert_eq!(rows.len(), 2, "{rows:?}");
        assert!(rows[1].contains("game.exe"), "{rows:?}");
        assert!(rows[1].contains("yes"), "{rows:?}");
        assert!(rows[1].contains("4.25.1"), "{rows:?}");

        let rows = table(&processes, &hints, true, 0);
        assert_eq!(rows.len(), 5, "{rows:?}");
        assert!(rows[3].contains("(path hidden)"), "{rows:?}");
        assert!(
            !rows[1].contains("yes") && !rows[4].contains("yes"),
            "{rows:?}"
        );
    }

    #[test]
    fn this_process_is_never_flagged_even_when_its_file_has_an_engine_hint() {
        // The own binary and another process share a path, and the hint says
        // "engine": only the other process is a target.
        let processes = vec![
            process(10, "ufathom.exe", Some("/tools/ufathom.exe")),
            process(11, "ufathom.exe", Some("/tools/ufathom.exe")),
        ];
        let mut hints: HashMap<&str, Option<Hint>> = HashMap::new();
        hints.insert("/tools/ufathom.exe", engine_hint());

        let rows = table(&processes, &hints, false, 10);
        assert_eq!(rows.len(), 2, "{rows:?}");
        assert!(rows[1].trim_start().starts_with("11"), "{rows:?}");
        assert!(rows[1].contains("yes"), "{rows:?}");

        let rows = table(&processes, &hints, true, 10);
        assert_eq!(rows.len(), 3, "{rows:?}");
        assert!(rows[1].trim_start().starts_with("10"), "{rows:?}");
        assert!(!rows[1].contains("yes"), "{rows:?}");
        assert!(!rows[1].contains("4.25.1"), "{rows:?}");

        // Alone, it is not shown without --all.
        let alone = vec![process(10, "ufathom.exe", Some("/tools/ufathom.exe"))];
        assert_eq!(table(&alone, &hints, false, 10).len(), 1);
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
