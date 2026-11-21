//! The list of companion DLLs: older mod DLLs loaded next to a generated SDK DLL.
//!
//! A team that already ships a mod DLL, for example through a proxy DLL such as
//! `dsound.dll`, can move to Rust one feature at a time. The generated SDK DLL
//! reads `ufathom-companions.txt` from its own folder and loads each DLL named
//! there, so the old and the new code run side by side. This module is the
//! pure half of that: it turns the text of the file into a [`CompanionList`].
//! It does no I/O and works on every platform; the generated `dll.rs` does the
//! Windows part (reading the file, `LoadLibraryExW`, `FreeLibrary`).
//!
//! # File format
//!
//! Plain text, one entry per line, in UTF-8 (a leading byte order mark and
//! CRLF line ends are accepted).
//!
//! ```text
//! # A comment. Blank lines are ignored. A comment takes a whole line.
//! legacy_mod.dll
//! legacy\second.dll
//! after-engine:needs_engine.dll
//! unload-on-exit
//! ```
//!
//! - A line that is not empty and does not start with `#` is a DLL file name,
//!   or a path relative to the folder of the SDK DLL. `/` and `\` both work.
//! - `after-engine:` before the name loads that DLL only once the engine is up.
//!   Without it a DLL loads at once, when the worker thread starts, because
//!   legacy proxies often expect to run early.
//! - The line `unload-on-exit` (a directive, not a file name) asks the SDK DLL
//!   to unload the companions when it unloads itself (`hot-unload` feature).
//!   Without it they stay loaded, because the old DLL may not survive being
//!   unloaded.
//! - An absolute path (a drive, a leading `\` or `/`, a UNC path) and any path
//!   with a `..` component are refused, so the file can only name DLLs that
//!   ship beside the SDK DLL.
//!
//! A line that cannot be used is recorded in [`CompanionList::rejected`] with
//! its line number and skipped; the other lines still count. The generated DLL
//! logs each rejected line and loads the rest.

use thiserror::Error;

/// The name of the list file, looked up in the folder of the SDK DLL.
pub const FILE_NAME: &str = "ufathom-companions.txt";

/// The prefix that delays a companion until the engine is up.
const AFTER_ENGINE_DIRECTIVE: &str = "after-engine";

/// The directive that unloads the companions with the SDK DLL.
const UNLOAD_DIRECTIVE: &str = "unload-on-exit";

/// Characters Windows does not allow in a path component, apart from `:`,
/// which is checked on its own, and the separators.
const FORBIDDEN_CHARACTERS: [char; 5] = ['*', '?', '"', '<', '>'];

/// When a companion is loaded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoadTime {
    /// When the worker thread starts, before the engine wait.
    Immediately,
    /// After the engine is up (objects loaded and, when the build has one,
    /// `GEngine` set). A build that does not wait for the engine loads these
    /// right after the immediate ones.
    AfterEngine,
}

/// One DLL to load next to the SDK DLL.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Companion {
    /// The path relative to the folder of the SDK DLL, with `\` separators.
    /// It is never absolute and has no `..` component.
    pub path: String,
    /// When to load it.
    pub load_time: LoadTime,
}

/// What a companion list file says.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CompanionList {
    /// The DLLs to load, in file order.
    pub companions: Vec<Companion>,
    /// Whether the `unload-on-exit` directive was present.
    pub unload_on_exit: bool,
    /// The lines that were refused, in file order. Each was skipped.
    pub rejected: Vec<LineError>,
}

impl CompanionList {
    /// The companions to load at `time`, in file order.
    pub fn at(&self, time: LoadTime) -> impl Iterator<Item = &Companion> {
        self.companions
            .iter()
            .filter(move |companion| companion.load_time == time)
    }
}

/// A line of the list file that was refused.
#[derive(Clone, Debug, PartialEq, Eq, Error)]
#[error("line {line}: {reason}")]
pub struct LineError {
    /// The 1-based line number.
    pub line: usize,
    /// Why the line was refused.
    pub reason: LineErrorReason,
}

/// Why a line of the list file was refused.
#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum LineErrorReason {
    /// The path is absolute or names a drive.
    #[error("`{0}` is an absolute path; use a path relative to the folder of the DLL")]
    AbsolutePath(String),
    /// The path has a `..` component.
    #[error("`{0}` has a `..` component; companions must be inside the folder of the DLL")]
    ParentComponent(String),
    /// The text before a `:` is not a known directive.
    #[error("unknown directive `{0}:`")]
    UnknownDirective(String),
    /// A directive that needs a file name has none.
    #[error("`{0}:` needs a file name after it")]
    MissingName(String),
    /// The name cannot be a file name on Windows.
    #[error("`{0}` is not a valid file name")]
    InvalidName(String),
}

/// Parses the text of a companion list file.
///
/// Never fails as a whole: a refused line goes to [`CompanionList::rejected`]
/// and parsing continues, so one typo does not stop the other companions from
/// loading. Directive names are matched without regard to ASCII case.
#[must_use]
pub fn parse(text: &str) -> CompanionList {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut list = CompanionList::default();
    for (index, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line.eq_ignore_ascii_case(UNLOAD_DIRECTIVE) {
            list.unload_on_exit = true;
            continue;
        }
        match parse_entry(line) {
            Ok(companion) => list.companions.push(companion),
            Err(reason) => list.rejected.push(LineError {
                line: index.saturating_add(1),
                reason,
            }),
        }
    }
    list
}

/// Parses one trimmed, non-empty line that is not a comment or a bare directive.
fn parse_entry(line: &str) -> Result<Companion, LineErrorReason> {
    let (load_time, name) = match line.split_once(':') {
        // A single letter before the colon is a drive, which `relative_path`
        // reports as an absolute path.
        Some((word, rest)) if is_directive_word(word) => {
            if !word.eq_ignore_ascii_case(AFTER_ENGINE_DIRECTIVE) {
                return Err(LineErrorReason::UnknownDirective(word.to_owned()));
            }
            let name = rest.trim();
            if name.is_empty() {
                return Err(LineErrorReason::MissingName(word.to_owned()));
            }
            (LoadTime::AfterEngine, name)
        }
        _ => (LoadTime::Immediately, line),
    };
    Ok(Companion {
        path: relative_path(name)?,
        load_time,
    })
}

/// Whether `word` looks like a directive name: two or more letters, digits or
/// hyphens. This keeps `C:\x.dll` (one letter) a path.
fn is_directive_word(word: &str) -> bool {
    word.chars().count() >= 2
        && word
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '-')
}

/// Checks that `name` stays inside the folder and returns it with `\` separators.
fn relative_path(name: &str) -> Result<String, LineErrorReason> {
    let mut characters = name.chars();
    let first = characters.next();
    let second = characters.next();
    let rooted = matches!(first, Some('/' | '\\'));
    let drive = matches!(
        (first, second),
        (Some(letter), Some(':')) if letter.is_ascii_alphabetic()
    );
    if rooted || drive {
        return Err(LineErrorReason::AbsolutePath(name.to_owned()));
    }
    let components: Vec<&str> = name.split(['/', '\\']).collect();
    // Windows drops trailing dots and spaces from a component, so `...` and
    // `.. ` reach the parent folder the same way `..` does.
    let leaves_the_folder = components.iter().any(|component| {
        *component != "."
            && !component.is_empty()
            && component
                .chars()
                .all(|character| character == '.' || character == ' ')
    });
    if leaves_the_folder {
        return Err(LineErrorReason::ParentComponent(name.to_owned()));
    }
    let empty_component = components.iter().any(|component| component.is_empty());
    let forbidden = name.chars().any(|character| {
        character == ':' || character.is_control() || FORBIDDEN_CHARACTERS.contains(&character)
    });
    if empty_component || forbidden {
        return Err(LineErrorReason::InvalidName(name.to_owned()));
    }
    Ok(components.join("\\"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(list: &CompanionList) -> Vec<&str> {
        list.companions
            .iter()
            .map(|companion| companion.path.as_str())
            .collect()
    }

    #[test]
    fn an_empty_file_lists_nothing() {
        assert_eq!(parse(""), CompanionList::default());
        assert_eq!(parse("\n\r\n   \n"), CompanionList::default());
    }

    #[test]
    fn comments_and_blank_lines_are_ignored() {
        let list = parse("# first\n\n   # indented comment\nlegacy.dll\n  \n# last");
        assert_eq!(paths(&list), ["legacy.dll"]);
        assert!(list.rejected.is_empty());
    }

    #[test]
    fn crlf_and_a_byte_order_mark_are_accepted() {
        let list = parse("\u{feff}# note\r\na.dll\r\nb.dll\r\n");
        assert_eq!(paths(&list), ["a.dll", "b.dll"]);
        assert!(list.rejected.is_empty());
    }

    #[test]
    fn a_byte_order_mark_does_not_turn_the_first_line_into_a_name() {
        let list = parse("\u{feff}b.dll");
        assert_eq!(paths(&list), ["b.dll"]);
    }

    #[test]
    fn entries_keep_file_order_and_surrounding_spaces_are_trimmed() {
        let list = parse("  z.dll  \na.dll\nsub/dir\\m.dll");
        assert_eq!(paths(&list), ["z.dll", "a.dll", "sub\\dir\\m.dll"]);
        assert!(
            list.companions
                .iter()
                .all(|companion| companion.load_time == LoadTime::Immediately)
        );
    }

    #[test]
    fn the_after_engine_prefix_delays_one_entry() {
        let list = parse("early.dll\nafter-engine:late.dll\nAfter-Engine:  spaced.dll\n");
        assert_eq!(paths(&list), ["early.dll", "late.dll", "spaced.dll"]);
        let late: Vec<&str> = list
            .at(LoadTime::AfterEngine)
            .map(|companion| companion.path.as_str())
            .collect();
        assert_eq!(late, ["late.dll", "spaced.dll"]);
        let early: Vec<&str> = list
            .at(LoadTime::Immediately)
            .map(|companion| companion.path.as_str())
            .collect();
        assert_eq!(early, ["early.dll"]);
    }

    #[test]
    fn after_engine_without_a_name_is_refused() {
        let list = parse("after-engine:\nafter-engine:   \n");
        assert!(list.companions.is_empty());
        assert_eq!(list.rejected.len(), 2);
        assert!(matches!(
            list.rejected.first().map(|error| &error.reason),
            Some(LineErrorReason::MissingName(_))
        ));
    }

    #[test]
    fn unload_on_exit_is_a_directive_not_a_file() {
        let list = parse("a.dll\nunload-on-exit\n");
        assert!(list.unload_on_exit);
        assert_eq!(paths(&list), ["a.dll"]);
        assert!(!parse("a.dll").unload_on_exit);
        assert!(parse("UNLOAD-ON-EXIT").unload_on_exit);
    }

    #[test]
    fn absolute_paths_are_refused_with_their_line() {
        let list = parse(
            "ok.dll\nC:\\mods\\x.dll\n\\\\server\\share\\x.dll\n/x.dll\n\\x.dll\nc:x.dll\nafter-engine:D:\\x.dll\n",
        );
        assert_eq!(paths(&list), ["ok.dll"]);
        let lines: Vec<usize> = list.rejected.iter().map(|error| error.line).collect();
        assert_eq!(lines, [2, 3, 4, 5, 6, 7]);
        assert!(
            list.rejected
                .iter()
                .all(|error| matches!(error.reason, LineErrorReason::AbsolutePath(_)))
        );
    }

    #[test]
    fn parent_components_are_refused() {
        let list = parse(
            "..\\x.dll\nsub\\..\\..\\x.dll\n../x.dll\nafter-engine:..\\x.dll\n...\\x.dll\n.. \\x.dll\nok.dll\n",
        );
        assert_eq!(paths(&list), ["ok.dll"]);
        assert_eq!(list.rejected.len(), 6);
        assert!(
            list.rejected
                .iter()
                .all(|error| matches!(error.reason, LineErrorReason::ParentComponent(_)))
        );
        assert_eq!(list.rejected.first().map(|error| error.line), Some(1));
    }

    #[test]
    fn a_dot_component_and_dots_inside_names_are_fine() {
        let list = parse(".\\x.dll\nmod..v2.dll\n..hidden.dll\n");
        assert_eq!(paths(&list), [".\\x.dll", "mod..v2.dll", "..hidden.dll"]);
    }

    #[test]
    fn an_unknown_directive_is_refused_with_its_line() {
        let list = parse("a.dll\n# note\nbefore-engine:b.dll\nc.dll\n");
        assert_eq!(paths(&list), ["a.dll", "c.dll"]);
        assert_eq!(
            list.rejected,
            [LineError {
                line: 3,
                reason: LineErrorReason::UnknownDirective("before-engine".to_owned()),
            }]
        );
        assert_eq!(
            list.rejected.first().map(ToString::to_string).as_deref(),
            Some("line 3: unknown directive `before-engine:`")
        );
    }

    #[test]
    fn names_windows_cannot_open_are_refused() {
        let list = parse("a*.dll\nsub\\\nsub\\\\x.dll\nx.dll:stream\nbad\u{1}.dll\n");
        assert!(list.companions.is_empty());
        assert_eq!(list.rejected.len(), 5);
        assert!(
            list.rejected
                .iter()
                .all(|error| matches!(error.reason, LineErrorReason::InvalidName(_)))
        );
    }

    #[test]
    fn a_refused_line_does_not_stop_the_rest() {
        let list = parse("..\\a.dll\nb.dll\nC:\\c.dll\nafter-engine:d.dll\nunload-on-exit\n");
        assert_eq!(paths(&list), ["b.dll", "d.dll"]);
        assert_eq!(list.rejected.len(), 2);
        assert!(list.unload_on_exit);
    }
}
