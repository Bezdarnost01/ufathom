//! Turning `--pid`, `--process` and `--minidump` into a reader, and the
//! discovery settings from `--module`, `--layout` and `--hint`.

use std::fmt::Write as _;

use anyhow::{Context, Result, anyhow, bail};
use rayon::ThreadPool;
use ufathom_mem::{
    MemError, MemoryReader, MinidumpMemory, ProcessInfo, ProcessMemory, list_processes,
};
use ufathom_model::EngineVersion;
use ufathom_model::layout::TransformOp;
use ufathom_scan::version::engine_version_from_text;
use ufathom_scan::{DiscoverOptions, Discovery, LayoutOverrides, discover};

use crate::args::{Selector, TargetArgs};
use crate::layout_file;

/// A target that is open for reading.
pub struct Target {
    /// Reads the target's memory. Read-only by construction.
    pub reader: Box<dyn MemoryReader>,
    /// What the target is, for messages: `process 4242 (Game.exe)`.
    pub description: String,
    settings: Settings,
}

/// The parts of [`DiscoverOptions`] that do not borrow.
#[derive(Debug, Clone, Default)]
struct Settings {
    module: Option<String>,
    overrides: LayoutOverrides,
    pointer_transform: Vec<TransformOp>,
    hint_version: Option<EngineVersion>,
}

impl Target {
    /// Opens the target and reads its settings.
    ///
    /// # Errors
    ///
    /// The process is gone, access is denied, no or several processes match a
    /// name, the minidump cannot be read, or a settings file or value is bad.
    pub fn open(args: &TargetArgs) -> Result<Self> {
        let mut settings = Settings {
            module: args.module.clone(),
            ..Settings::default()
        };
        if let Some(path) = &args.layout {
            let file = layout_file::load(path)?;
            settings.overrides = file.overrides;
            settings.pointer_transform = file.pointer_transform;
        }
        if let Some(text) = &args.hint {
            settings.hint_version = Some(parse_hint(text)?);
        }
        let (reader, description) = open_reader(&args.selector)?;
        Ok(Self {
            reader,
            description,
            settings,
        })
    }

    /// The options for [`discover`], with `pool` for the parallel parts.
    pub fn discover_options<'a>(&self, pool: Option<&'a ThreadPool>) -> DiscoverOptions<'a> {
        DiscoverOptions {
            module: self.settings.module.clone(),
            overrides: self.settings.overrides.clone(),
            pointer_transform: self.settings.pointer_transform.clone(),
            hint_version: self.settings.hint_version,
            pool,
        }
    }

    /// Solves the layout of the target.
    ///
    /// # Errors
    ///
    /// The reason discovery failed, with what to try.
    pub fn discover(&self, pool: Option<&ThreadPool>) -> Result<Discovery> {
        discover(&*self.reader, &self.discover_options(pool)).map_err(|error| {
            anyhow!(
                "discovery failed on {}: {error}\n  try: --module NAME if the engine is in another module, \
                 --layout FILE to state offsets, or `ufathom doctor` for a report",
                self.description
            )
        })
    }
}

fn open_reader(selector: &Selector) -> Result<(Box<dyn MemoryReader>, String)> {
    if let Some(pid) = selector.pid {
        let memory = open_process(pid)?;
        return Ok((Box::new(memory), format!("process {pid}")));
    }
    if let Some(name) = &selector.process {
        let found = find_process(name)?;
        let memory = open_process(found.pid)?;
        return Ok((
            Box::new(memory),
            format!("process {} ({})", found.pid, found.name),
        ));
    }
    if let Some(path) = &selector.minidump {
        let memory = MinidumpMemory::open(path)
            .with_context(|| format!("cannot read the minidump {}", path.display()))?;
        return Ok((Box::new(memory), format!("minidump {}", path.display())));
    }
    bail!("no target: use --pid, --process or --minidump")
}

fn open_process(pid: u32) -> Result<ProcessMemory> {
    ProcessMemory::open(pid).map_err(|error| match error {
        MemError::ProcessGone { .. } => anyhow!(
            "process {pid} does not exist or has exited; `ufathom ps` lists the running ones"
        ),
        MemError::AccessDenied { .. } => anyhow!(
            "access denied opening process {pid} for reading; if it runs elevated or is protected, \
             start ufathom the same way. ufathom never works around a refusal"
        ),
        other => anyhow!("cannot open process {pid}: {other}"),
    })
}

/// The one process whose executable name is `name`, in any letter case, with
/// or without `.exe`.
fn find_process(name: &str) -> Result<ProcessInfo> {
    let processes = list_processes().context("cannot list the running processes")?;
    pick_process(name, processes)
}

fn pick_process(name: &str, processes: Vec<ProcessInfo>) -> Result<ProcessInfo> {
    let wanted = name.to_ascii_lowercase();
    let wanted_exe = format!("{wanted}.exe");
    let mut matches: Vec<ProcessInfo> = processes
        .into_iter()
        .filter(|process| {
            let found = process.name.to_ascii_lowercase();
            found == wanted || found == wanted_exe
        })
        .collect();
    match matches.len() {
        0 => bail!("no running process is named {name}; `ufathom ps` lists the running ones"),
        1 => matches
            .pop()
            .ok_or_else(|| anyhow!("process list changed while it was read")),
        count => {
            let mut message = format!("{count} processes are named {name}; pick one with --pid:");
            for process in &matches {
                let path = process.path.as_deref().unwrap_or("path hidden");
                let _ = write!(message, "\n  pid {}  {path}", process.pid);
            }
            bail!("{message}")
        }
    }
}

/// Reads `--hint`: `4.25`, `4.25.1` or `5`.
fn parse_hint(text: &str) -> Result<EngineVersion> {
    // `engine_version_from_text` wants major.minor; a bare major gets minor 0.
    let candidate = if text.trim().chars().all(|c| c.is_ascii_digit()) {
        format!("{}.0", text.trim())
    } else {
        text.trim().to_owned()
    };
    engine_version_from_text(&candidate)
        .map(|version| version.to_engine_version())
        .ok_or_else(|| {
            anyhow!("--hint {text:?} is not an engine version; use 4 or 5 followed by the minor version, such as 4.25 or 5.4.1")
        })
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

    #[test]
    fn a_name_matches_ignoring_case_and_the_exe_suffix() {
        let list = || {
            vec![
                process(1, "Other.exe", None),
                process(2, "Game.exe", Some("a")),
            ]
        };
        for name in ["Game.exe", "game.EXE", "game", "GAME"] {
            assert_eq!(pick_process(name, list()).unwrap().pid, 2, "{name}");
        }
    }

    #[test]
    fn no_match_is_an_error_that_points_at_ps() {
        let error = pick_process("nothing", vec![process(1, "a.exe", None)]).unwrap_err();
        assert!(error.to_string().contains("ufathom ps"), "{error}");
    }

    #[test]
    fn several_matches_are_refused_and_listed_with_their_pids() {
        let list = vec![
            process(10, "Game.exe", Some("C:\\one\\Game.exe")),
            process(11, "game.exe", None),
        ];
        let error = pick_process("Game.exe", list).unwrap_err().to_string();
        assert!(error.contains("2 processes"), "{error}");
        assert!(error.contains("pid 10"), "{error}");
        assert!(error.contains("pid 11"), "{error}");
        assert!(error.contains("--pid"), "{error}");
    }

    #[test]
    fn hints_read_as_engine_versions() {
        let v = |text| parse_hint(text).unwrap();
        assert_eq!(v("4.25").to_string(), "4.25.0");
        assert_eq!(v("4.21.1").to_string(), "4.21.1");
        assert_eq!(v("5").to_string(), "5.0.0");
        assert_eq!(v(" 5.4 ").to_string(), "5.4.0");
        for bad in ["", "x", "3.1", "4.", "four"] {
            assert!(parse_hint(bad).is_err(), "{bad:?}");
        }
    }
}
