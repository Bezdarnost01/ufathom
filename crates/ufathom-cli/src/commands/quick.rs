//! `ufathom quick`: an interactive dump with safe output defaults.

use std::io::BufRead;
use std::time::SystemTime;

use anyhow::{Context, anyhow};
use ufathom_agent::create_session_folder;
use ufathom_mem::ProcessInfo;

use crate::args::{DumpArgs, QuickArgs, RuntimeArgs, Selector, TargetArgs};
use crate::commands::{dump, inspect};
use crate::error::Outcome;

/// `ufathom quick`.
pub fn quick(args: &QuickArgs) -> Outcome {
    let processes = inspect::process_candidates(args.all)?;
    if processes.is_empty() {
        return Err(anyhow!(
            "no probable Unreal Engine process found; use `ufathom quick --all` to choose from every process"
        )
        .into());
    }

    let selected = {
        let stdin = std::io::stdin();
        let stdout = std::io::stdout();
        let mut input = stdin.lock();
        let mut output = stdout.lock();
        select_process(&processes, args.all, &mut input, &mut output)?
    };
    let Some(pid) = selected else {
        println!("cancelled; nothing was written");
        return Ok(());
    };
    let process = processes
        .iter()
        .find(|process| process.pid == pid)
        .ok_or_else(|| anyhow!("the selected process disappeared from the list"))?;
    println!("selected:      {} (PID {pid})", process.name);

    let folder = create_session_folder(&args.output, SystemTime::now(), pid)
        .with_context(|| format!("cannot create a dump folder in {}", args.output.display()))?;
    println!("output folder: {}", folder.display());

    let args = DumpArgs {
        target: TargetArgs {
            selector: Selector {
                pid: Some(pid),
                process: None,
                minidump: None,
            },
            module: None,
            layout: None,
            hint: None,
        },
        output: folder.join("dump.json"),
        usmap: Some(folder.join("Mappings.usmap")),
        sdk: Some(folder.join("sdk")),
        runtime: RuntimeArgs {
            runtime_path: None,
            runtime_version: None,
        },
        threads: None,
        allow_degraded: false,
    };
    dump::dump(&args)
}

/// Prints the choices and reads a list index or a PID. Empty input cancels;
/// EOF is an error rather than an invitation to wait forever in a script.
fn select_process(
    processes: &[ProcessInfo],
    all: bool,
    input: &mut impl BufRead,
    output: &mut impl std::io::Write,
) -> anyhow::Result<Option<u32>> {
    if all {
        writeln!(output, "Running processes:")?;
    } else {
        writeln!(output, "Probable Unreal Engine processes:")?;
    }
    for (index, process) in processes.iter().enumerate() {
        writeln!(
            output,
            "  {}. {} (PID {})",
            index + 1,
            process.name,
            process.pid
        )?;
        writeln!(
            output,
            "     {}",
            process.path.as_deref().unwrap_or("path hidden")
        )?;
    }

    loop {
        write!(
            output,
            "Select a process (list number or PID, empty to cancel): "
        )?;
        output.flush()?;
        let mut line = String::new();
        if input.read_line(&mut line)? == 0 {
            return Err(anyhow!("input ended before a process was selected"));
        }
        if line.trim().is_empty() {
            return Ok(None);
        }
        if let Some(pid) = selected_pid(&line, processes) {
            return Ok(Some(pid));
        }
        writeln!(
            output,
            "Enter a number from the list or the PID of a listed process."
        )?;
    }
}

/// Resolves a one-based list index first, then a literal PID from the list.
fn selected_pid(text: &str, processes: &[ProcessInfo]) -> Option<u32> {
    let selected = text.trim().parse::<u32>().ok()?;
    let index = usize::try_from(selected).ok()?.checked_sub(1)?;
    if let Some(process) = processes.get(index) {
        return Some(process.pid);
    }
    processes
        .iter()
        .find(|process| process.pid == selected)
        .map(|process| process.pid)
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

    fn processes() -> Vec<ProcessInfo> {
        vec![
            ProcessInfo {
                pid: 101,
                name: "Game.exe".to_owned(),
                path: Some("D:/games/Game.exe".to_owned()),
            },
            ProcessInfo {
                pid: 202,
                name: "Editor.exe".to_owned(),
                path: None,
            },
        ]
    }

    #[test]
    fn a_list_number_or_a_listed_pid_selects_a_process() {
        let processes = processes();
        assert_eq!(selected_pid("1\n", &processes), Some(101));
        assert_eq!(selected_pid(" 2 ", &processes), Some(202));
        assert_eq!(selected_pid("202", &processes), Some(202));
        for invalid in ["0", "3", "999", "game", ""] {
            assert_eq!(selected_pid(invalid, &processes), None, "{invalid:?}");
        }
    }

    #[test]
    fn invalid_input_is_explained_and_empty_input_cancels() {
        let processes = processes();
        let mut input = std::io::Cursor::new("wrong\n\n");
        let mut output = Vec::new();
        let selected = select_process(&processes, false, &mut input, &mut output).unwrap();
        assert_eq!(selected, None);
        let text = String::from_utf8(output).unwrap();
        assert!(text.contains("Game.exe"), "{text}");
        assert!(text.contains("D:/games/Game.exe"), "{text}");
        assert!(text.contains("Enter a number"), "{text}");
    }

    #[test]
    fn end_of_input_is_an_error() {
        let processes = processes();
        let mut input = std::io::Cursor::new(Vec::<u8>::new());
        let mut output = Vec::new();
        let error = select_process(&processes, false, &mut input, &mut output).unwrap_err();
        assert!(error.to_string().contains("input ended"), "{error}");
    }
}
