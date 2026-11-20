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

/// Prints the choices and reads a list number or a PID. A bare number that is
/// both is asked about again; `#N` and `pid:N` say which is meant. Empty input
/// cancels; EOF is an error rather than an invitation to wait forever in a
/// script.
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
            "Select a process (list number or PID, #N or pid:N to be exact, empty to cancel): "
        )?;
        output.flush()?;
        let mut line = String::new();
        if input.read_line(&mut line)? == 0 {
            return Err(anyhow!("input ended before a process was selected"));
        }
        if line.trim().is_empty() {
            return Ok(None);
        }
        match selected_pid(&line, processes) {
            Selection::Pid(pid) => return Ok(Some(pid)),
            Selection::Ambiguous {
                value,
                list_name,
                pid_name,
            } => writeln!(
                output,
                "{value} is both list number {value} ({list_name}) and PID {value} ({pid_name}). \
                 Type #{value} for the list number or pid:{value} for the PID."
            )?,
            Selection::NoMatch => writeln!(
                output,
                "Enter a number from the list, the PID of a listed process, #N for a list number or pid:N for a PID."
            )?,
        }
    }
}

/// What a line typed at the prompt refers to.
#[derive(Debug, PartialEq, Eq)]
enum Selection {
    /// Exactly one listed process.
    Pid(u32),
    /// A bare number that is a valid list number of one process and the PID of
    /// another. The user has to say which with `#N` or `pid:N`.
    Ambiguous {
        value: u32,
        list_name: String,
        pid_name: String,
    },
    /// Not a number, or no listed process matches it.
    NoMatch,
}

/// Resolves `text` against the list.
///
/// `#N` is the one-based list number and `pid:N` (any letter case) is a PID of
/// a listed process. A bare `N` is either, if only one of them names a listed
/// process, or the same process by both readings. If it names two different
/// processes the result is [`Selection::Ambiguous`].
fn selected_pid(text: &str, processes: &[ProcessInfo]) -> Selection {
    let text = text.trim();
    let by_number = |number: u32| {
        let index = usize::try_from(number).ok()?.checked_sub(1)?;
        processes.get(index)
    };
    let by_pid = |pid: u32| processes.iter().find(|process| process.pid == pid);
    let found = |process: Option<&ProcessInfo>| {
        process.map_or(Selection::NoMatch, |process| Selection::Pid(process.pid))
    };

    if let Some(rest) = text.strip_prefix('#') {
        return rest
            .trim()
            .parse()
            .map_or(Selection::NoMatch, |number| found(by_number(number)));
    }
    let lowered = text.to_ascii_lowercase();
    if let Some(rest) = lowered.strip_prefix("pid:") {
        return rest
            .trim()
            .parse()
            .map_or(Selection::NoMatch, |pid| found(by_pid(pid)));
    }
    let Ok(value) = text.parse::<u32>() else {
        return Selection::NoMatch;
    };
    match (by_number(value), by_pid(value)) {
        (Some(listed), Some(owner)) if listed.pid != owner.pid => Selection::Ambiguous {
            value,
            list_name: listed.name.clone(),
            pid_name: owner.name.clone(),
        },
        (Some(process), _) | (None, Some(process)) => Selection::Pid(process.pid),
        (None, None) => Selection::NoMatch,
    }
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

    /// A long list as with `--all`: list number 128 is `Proc128.exe`, while
    /// PID 128 belongs to the process at list number 3.
    fn long_list() -> Vec<ProcessInfo> {
        let mut list: Vec<ProcessInfo> = (1..=130u32)
            .map(|number| ProcessInfo {
                pid: 10_000 + number,
                name: format!("Proc{number}.exe"),
                path: None,
            })
            .collect();
        list[2].pid = 128;
        list[2].name = "Pid128.exe".to_owned();
        list
    }

    #[test]
    fn a_list_number_or_a_listed_pid_selects_a_process() {
        let processes = processes();
        assert_eq!(selected_pid("1\n", &processes), Selection::Pid(101));
        assert_eq!(selected_pid(" 2 ", &processes), Selection::Pid(202));
        assert_eq!(selected_pid("202", &processes), Selection::Pid(202));
        assert_eq!(selected_pid("101", &processes), Selection::Pid(101));
    }

    #[test]
    fn a_number_that_is_a_list_number_and_a_different_pid_is_ambiguous() {
        let processes = long_list();
        assert_eq!(
            selected_pid("128", &processes),
            Selection::Ambiguous {
                value: 128,
                list_name: "Proc128.exe".to_owned(),
                pid_name: "Pid128.exe".to_owned(),
            }
        );
        // Numbers that are only one of the two are not ambiguous.
        assert_eq!(selected_pid("129", &processes), Selection::Pid(10_129));
        assert_eq!(selected_pid("10130", &processes), Selection::Pid(10_130));
    }

    #[test]
    fn explicit_forms_resolve_the_ambiguity() {
        let processes = long_list();
        assert_eq!(selected_pid("#128", &processes), Selection::Pid(10_128));
        assert_eq!(selected_pid(" # 128 ", &processes), Selection::Pid(10_128));
        assert_eq!(selected_pid("pid:128", &processes), Selection::Pid(128));
        assert_eq!(selected_pid("PID:128", &processes), Selection::Pid(128));
        assert_eq!(selected_pid(" Pid: 128\n", &processes), Selection::Pid(128));
        // `#` only takes list numbers and `pid:` only takes PIDs.
        assert_eq!(selected_pid("#10128", &processes), Selection::NoMatch);
        assert_eq!(selected_pid("pid:3", &processes), Selection::NoMatch);
    }

    #[test]
    fn both_readings_of_the_same_process_select_it() {
        let mut processes = processes();
        processes[1].pid = 2;
        assert_eq!(selected_pid("2", &processes), Selection::Pid(2));
    }

    #[test]
    fn out_of_range_and_garbage_match_nothing() {
        let processes = processes();
        for invalid in [
            "0",
            "3",
            "999",
            "game",
            "",
            "#",
            "#0",
            "#3",
            "#202",
            "#x",
            "pid:",
            "pid:999",
            "pid:1",
            "pid:x",
            "pid 101",
            "-1",
            "1.5",
            "4294967296",
        ] {
            assert_eq!(
                selected_pid(invalid, &processes),
                Selection::NoMatch,
                "{invalid:?}"
            );
        }
    }

    #[test]
    fn an_ambiguous_number_is_asked_again_with_both_candidates_named() {
        let processes = long_list();
        let mut input = std::io::Cursor::new("128\npid:128\n");
        let mut output = Vec::new();
        let selected = select_process(&processes, true, &mut input, &mut output).unwrap();
        assert_eq!(selected, Some(128));
        let text = String::from_utf8(output).unwrap();
        assert!(
            text.contains(
                "128 is both list number 128 (Proc128.exe) and PID 128 (Pid128.exe). \
                 Type #128 for the list number or pid:128 for the PID."
            ),
            "{text}"
        );
        assert_eq!(text.matches("Select a process").count(), 2, "{text}");
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
