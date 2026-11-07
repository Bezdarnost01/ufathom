//! Path handling: sanitized folder names and the Windows extended path form.
//!
//! Windows refuses paths of 260 characters or more unless they are given in
//! the extended form that starts with `\\?\`. That form turns off all path
//! normalization, so the path must first be made absolute, use backslashes
//! and have no `.` or `..` components. [`extended_path`] does this; the
//! pure part, [`verbatim_from_absolute`], is separate so it is tested on every
//! platform.

use std::path::{Path, PathBuf};

/// Longest folder name this crate creates. Windows allows 255 per component;
/// names here stay far below that so a long label cannot make a path unusable.
pub const MAX_COMPONENT_LEN: usize = 64;

/// Names Windows reserves for devices, with or without an extension.
const RESERVED_NAMES: &[&str] = &[
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

/// Turns any text into a name that is safe as one path component on every
/// supported system.
///
/// ASCII letters, digits and `. - _ +` are kept; anything else becomes `_`.
/// Leading and trailing dots are removed (a trailing dot is dropped by
/// Windows, a leading one hides a file), device names such as `CON` get a
/// prefix, and the result is cut to [`MAX_COMPONENT_LEN`]. Text that leaves
/// nothing becomes `fallback`.
#[must_use]
pub fn sanitize_component(raw: &str, fallback: &str) -> String {
    let name = clean_component(raw);
    if !name.is_empty() {
        return name;
    }
    let name = clean_component(fallback);
    if name.is_empty() {
        "unnamed".to_owned()
    } else {
        name
    }
}

/// [`sanitize_component`] without the fallback: empty when nothing is left.
fn clean_component(raw: &str) -> String {
    let mapped: String = raw
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | '+') {
                c
            } else {
                '_'
            }
        })
        .collect();
    let trimmed = mapped.trim_matches('.');
    let mut name: String = trimmed.chars().take(MAX_COMPONENT_LEN).collect();
    let kept = name.trim_end_matches('.').len();
    name.truncate(kept);
    if name.is_empty() {
        return name;
    }
    let stem = name.split('.').next().unwrap_or_default();
    if RESERVED_NAMES
        .iter()
        .any(|reserved| reserved.eq_ignore_ascii_case(stem))
    {
        name.insert(0, '_');
    }
    name
}

/// The extended (`\\?\`) form of an absolute Windows path, or `None` when the
/// path is relative.
///
/// Forward slashes become backslashes, repeated separators collapse and `.`
/// and `..` components are resolved, since the extended form does not do that.
/// A path that already starts with `\\?\` or `\\.\` is returned unchanged. A
/// UNC path `\\server\share\x` becomes `\\?\UNC\server\share\x`.
#[must_use]
pub fn verbatim_from_absolute(path: &str) -> Option<String> {
    if path.starts_with(r"\\?\") || path.starts_with(r"\\.\") {
        return Some(path.to_owned());
    }
    let unified = path.replace('/', r"\");
    if let Some(rest) = unified.strip_prefix(r"\\") {
        let (root, tail) = split_unc(rest)?;
        return Some(format!(r"\\?\UNC\{root}{}", join_components(tail)));
    }
    let mut chars = unified.chars();
    let (drive, colon, slash) = (chars.next()?, chars.next()?, chars.next()?);
    if drive.is_ascii_alphabetic() && colon == ':' && slash == '\\' {
        let tail = unified.get(3..)?;
        return Some(format!(r"\\?\{drive}:{}", join_components(tail)));
    }
    None
}

/// Splits `server\share\rest` into `server\share` and `rest`.
fn split_unc(rest: &str) -> Option<(String, &str)> {
    let mut parts = rest.splitn(3, '\\');
    let server = parts.next().filter(|part| !part.is_empty())?;
    let share = parts.next().filter(|part| !part.is_empty())?;
    Some((format!(r"{server}\{share}"), parts.next().unwrap_or("")))
}

/// Resolves `.` and `..` in `tail` and returns the components, each preceded
/// by a backslash. `..` never climbs above the root.
fn join_components(tail: &str) -> String {
    let mut kept: Vec<&str> = Vec::new();
    for component in tail.split('\\') {
        match component {
            "" | "." => {}
            ".." => {
                kept.pop();
            }
            other => kept.push(other),
        }
    }
    if kept.is_empty() {
        return r"\".to_owned();
    }
    kept.iter().fold(String::new(), |mut joined, component| {
        joined.push('\\');
        joined.push_str(component);
        joined
    })
}

/// The path in the form file operations should be given.
///
/// On Windows the path is made absolute and returned in the extended form, so
/// paths of any length work. On other systems it is returned unchanged.
#[must_use]
pub fn extended_path(path: &Path) -> PathBuf {
    #[cfg(windows)]
    {
        let absolute = std::path::absolute(path).unwrap_or_else(|_| path.to_owned());
        match absolute.to_str().and_then(verbatim_from_absolute) {
            Some(text) => PathBuf::from(text),
            None => absolute,
        }
    }
    #[cfg(not(windows))]
    {
        path.to_owned()
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

    #[test]
    fn safe_text_is_kept() {
        assert_eq!(sanitize_component("dump-001_a.b+c", "x"), "dump-001_a.b+c");
    }

    #[test]
    fn separators_and_odd_characters_are_replaced() {
        assert_eq!(sanitize_component("a/b\\c:d*e?f", "x"), "a_b_c_d_e_f");
        assert_eq!(sanitize_component("naïve name", "x"), "na_ve_name");
        assert_eq!(sanitize_component("..\\..\\evil", "x"), "_.._evil");
    }

    #[test]
    fn dots_at_the_ends_are_removed() {
        assert_eq!(sanitize_component("...a.b...", "x"), "a.b");
        assert_eq!(sanitize_component("....", "fallback"), "fallback");
    }

    #[test]
    fn a_name_that_leaves_nothing_uses_the_fallback() {
        assert_eq!(sanitize_component("", "dump"), "dump");
        assert_eq!(sanitize_component("...", "_x"), "_x");
        assert_eq!(sanitize_component("...", "///"), "___");
        assert_eq!(sanitize_component("", ""), "unnamed");
    }

    #[test]
    fn device_names_are_prefixed_whatever_the_case_or_extension() {
        assert_eq!(sanitize_component("con", "x"), "_con");
        assert_eq!(sanitize_component("NUL.txt", "x"), "_NUL.txt");
        assert_eq!(sanitize_component("lpt3", "x"), "_lpt3");
        assert_eq!(sanitize_component("console", "x"), "console");
    }

    #[test]
    fn long_names_are_cut() {
        let long = "a".repeat(500);
        assert_eq!(sanitize_component(&long, "x").len(), MAX_COMPONENT_LEN);
    }

    #[test]
    fn a_drive_path_gets_the_extended_prefix() {
        assert_eq!(
            verbatim_from_absolute(r"C:\Games\Game\Binaries").unwrap(),
            r"\\?\C:\Games\Game\Binaries"
        );
    }

    #[test]
    fn slashes_and_dot_components_are_normalized() {
        assert_eq!(
            verbatim_from_absolute("C:/a//b/./c/../d").unwrap(),
            r"\\?\C:\a\b\d"
        );
        assert_eq!(
            verbatim_from_absolute(r"C:\a\..\..\b").unwrap(),
            r"\\?\C:\b"
        );
        assert_eq!(verbatim_from_absolute(r"C:\").unwrap(), r"\\?\C:\");
    }

    #[test]
    fn a_unc_path_uses_the_unc_form() {
        assert_eq!(
            verbatim_from_absolute(r"\\server\share\dir\file").unwrap(),
            r"\\?\UNC\server\share\dir\file"
        );
        assert_eq!(verbatim_from_absolute(r"\\server").map(|_| ()), None);
    }

    #[test]
    fn an_extended_path_is_left_alone() {
        let path = r"\\?\C:\a\..\b";
        assert_eq!(verbatim_from_absolute(path).unwrap(), path);
        let device = r"\\.\pipe\x";
        assert_eq!(verbatim_from_absolute(device).unwrap(), device);
    }

    #[test]
    fn a_relative_path_has_no_extended_form() {
        assert_eq!(verbatim_from_absolute(r"dir\file"), None);
        assert_eq!(verbatim_from_absolute(r"\rooted"), None);
        assert_eq!(verbatim_from_absolute(""), None);
    }

    #[test]
    fn a_path_over_260_characters_stays_intact() {
        let deep = format!(r"C:\{}", ["segment-of-some-length"; 14].join(r"\"));
        assert!(deep.len() > 260);
        let extended = verbatim_from_absolute(&deep).unwrap();
        assert_eq!(extended, format!(r"\\?\{deep}"));
    }

    #[cfg(windows)]
    #[test]
    fn extended_path_makes_a_relative_path_absolute() {
        let extended = extended_path(Path::new("some-relative-folder"));
        let text = extended.to_str().unwrap();
        assert!(text.starts_with(r"\\?\"), "{text}");
        assert!(text.ends_with("some-relative-folder"), "{text}");
    }
}
