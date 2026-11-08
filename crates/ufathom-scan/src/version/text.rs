//! Reading an engine version out of the text forms binaries carry.

use std::fmt;

use ufathom_model::EngineVersion;

use crate::limits::MAX_BUILD_STRING_LEN;

/// Prefix of the branch part of an engine build string, before the version.
const RELEASE_BRANCH_PREFIX: &str = "Release-";
/// Separator between a branch and its changelist in a build string.
const CHANGELIST_SEPARATOR: &str = "-CL-";
/// Largest minor version accepted as an engine version. Real minors stay
/// below 30; the margin keeps future ones while rejecting build numbers.
const MAX_ENGINE_MINOR: u16 = 99;

/// A version with an optional patch, as binaries state it: `4.21.1` or `5.4`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PartialVersion {
    /// Major version, 4 or 5.
    pub major: u16,
    /// Minor version.
    pub minor: u16,
    /// Patch version, when the source states one.
    pub patch: Option<u16>,
}

impl PartialVersion {
    /// The `(major, minor)` pair, which is what builds of one engine branch
    /// share.
    #[must_use]
    pub fn branch(&self) -> (u16, u16) {
        (self.major, self.minor)
    }

    /// As an [`EngineVersion`], with an unstated patch read as 0.
    #[must_use]
    pub fn to_engine_version(&self) -> EngineVersion {
        EngineVersion {
            major: self.major,
            minor: self.minor,
            patch: self.patch.unwrap_or(0),
        }
    }
}

impl fmt::Display for PartialVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}", self.major, self.minor)?;
        if let Some(patch) = self.patch {
            write!(f, ".{patch}")?;
        }
        Ok(())
    }
}

/// Reads the engine version a string states, or `None` when it states none.
///
/// Two forms are understood:
///
/// - a leading dotted number whose major is 4 or 5: `4.21.1`, `4.13.2-0+UE4`,
///   `5.4.4-1234567+++UE5+Release-5.4`. A fourth component is ignored.
/// - a release branch anywhere in the text: `++UE5+Release-5.4-CL-1234567`.
///
/// A leading number with another major, such as `1.0.0.0`, is not an engine
/// version, and the search then continues with the branch form.
///
/// The text is whatever a resource or binary carries, so a project may fill
/// `ProductVersion` with its own number. Callers must treat the result as a
/// claim, not a fact.
#[must_use]
pub fn engine_version_from_text(text: &str) -> Option<PartialVersion> {
    leading_version(text.trim()).or_else(|| release_branch_version(text))
}

/// `major.minor[.patch]` at the start of `text`, if it is an engine version.
fn leading_version(text: &str) -> Option<PartialVersion> {
    let mut rest = text;
    let major = take_number(&mut rest)?;
    let minor = take_dot_number(&mut rest)?;
    let patch = take_dot_number(&mut rest);
    plausible(major, minor).then_some(PartialVersion {
        major,
        minor,
        patch,
    })
}

/// The version in the first `Release-X.Y` of `text`.
fn release_branch_version(text: &str) -> Option<PartialVersion> {
    text.match_indices(RELEASE_BRANCH_PREFIX)
        .find_map(|(at, _)| {
            let mut rest = text.get(at + RELEASE_BRANCH_PREFIX.len()..)?;
            let major = take_number(&mut rest)?;
            let minor = take_dot_number(&mut rest)?;
            plausible(major, minor).then_some(PartialVersion {
                major,
                minor,
                patch: None,
            })
        })
}

/// Whether `major.minor` can be an Unreal Engine version.
fn plausible(major: u16, minor: u16) -> bool {
    matches!(major, 4 | 5) && minor <= MAX_ENGINE_MINOR
}

/// Consumes a leading run of at most five digits and returns its value.
fn take_number(rest: &mut &str) -> Option<u16> {
    let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
    if digits == 0 || digits > 5 {
        return None;
    }
    let (number, tail) = rest.split_at(digits);
    *rest = tail;
    number.parse().ok()
}

/// Consumes `.` and a number, leaving `rest` untouched if either is missing.
fn take_dot_number(rest: &mut &str) -> Option<u16> {
    let mut tail = rest.strip_prefix('.')?;
    let number = take_number(&mut tail)?;
    *rest = tail;
    Some(number)
}

/// How a string is stored in the image.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum StringEncoding {
    /// One byte per character.
    Ascii,
    /// Two bytes per character, little endian.
    Utf16,
}

impl fmt::Display for StringEncoding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Ascii => "ASCII",
            Self::Utf16 => "UTF-16",
        })
    }
}

/// A `++Project+Branch` build string found in a read-only section.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildString {
    /// The whole string, for example `++Project+Branch-CL-123`.
    pub text: String,
    /// How it is stored.
    pub encoding: StringEncoding,
    /// The project part, between `++` and the next `+`.
    pub project: String,
    /// The branch part, after the project and before any changelist.
    pub branch: String,
    /// The number after `-CL-`, when the string ends in one.
    pub changelist: Option<u64>,
    /// The engine version, when this is the engine's own branch string
    /// (`++UE4+Release-4.21`, `++UE5+Release-5.4`) and names a release.
    pub engine_release: Option<PartialVersion>,
    /// Name of the section that holds the first copy.
    pub section: String,
    /// RVA of the first copy.
    pub rva: u32,
}

impl BuildString {
    /// Whether the string is strong evidence of an engine build, not just a
    /// `++A+B` shape: it ends in a changelist (`-CL-<digits>`), or its branch
    /// starts with `UE4`, `UE5` or `Release-`.
    ///
    /// Two alphanumeric runs around one `+` also occur in unrelated data such
    /// as base64 text, so the shape alone proves nothing.
    #[must_use]
    pub fn is_engine_grade(&self) -> bool {
        self.changelist.is_some()
            || ["UE4", "UE5", "Release-"]
                .iter()
                .any(|prefix| self.branch.starts_with(prefix))
    }
}

/// Whether `byte` can be part of a build string.
pub(crate) fn is_build_char(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'+' | b'-' | b'.')
}

/// Fewest characters a project or branch name of a build string has. A
/// shorter name is how a stray `++C+C` in an unrelated binary looks.
const MIN_BUILD_NAME_LEN: usize = 2;

/// Whether `project` can be the project part of a build string: at least
/// [`MIN_BUILD_NAME_LEN`] characters of `A-Z a-z 0-9 _`.
fn is_plausible_project(project: &str) -> bool {
    project.len() >= MIN_BUILD_NAME_LEN
        && project
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

/// Whether `branch` can be the branch part of a build string: at least
/// [`MIN_BUILD_NAME_LEN`] characters of `A-Z a-z 0-9 _ . -`.
fn is_plausible_branch(branch: &str) -> bool {
    branch.len() >= MIN_BUILD_NAME_LEN
        && branch
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b'-'))
}

/// Parses `text` as `++Project+Branch[-CL-N]`. Returns the fields without
/// location; the caller fills `section` and `rva`.
///
/// Text that cannot be a build string is refused, not kept as weak evidence:
/// a project or branch shorter than two characters, a project with anything
/// but letters, digits and `_`, or a branch with anything but those, `.` and
/// `-`. Random bytes in a binary, such as base64, otherwise pass for one.
pub(crate) fn parse_build_string(text: &str, encoding: StringEncoding) -> Option<BuildString> {
    if text.len() > MAX_BUILD_STRING_LEN {
        return None;
    }
    let body = text.strip_prefix("++")?;
    let (project, rest) = body.split_once('+')?;
    let (branch, changelist) = match rest.rfind(CHANGELIST_SEPARATOR) {
        Some(at) => {
            let digits = rest
                .get(at + CHANGELIST_SEPARATOR.len()..)
                .unwrap_or_default();
            let valid = !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit());
            match (valid, digits.parse::<u64>()) {
                (true, Ok(number)) => (rest.get(..at).unwrap_or_default(), Some(number)),
                _ => (rest, None),
            }
        }
        None => (rest, None),
    };
    if !is_plausible_project(project) || !is_plausible_branch(branch) {
        return None;
    }
    let engine_release = match project {
        "UE4" | "UE5" => engine_version_from_text(branch).filter(|version| {
            let expected = if project == "UE4" { 4 } else { 5 };
            version.major == expected
        }),
        _ => None,
    };
    Some(BuildString {
        text: text.to_owned(),
        encoding,
        project: project.to_owned(),
        branch: branch.to_owned(),
        changelist,
        engine_release,
        section: String::new(),
        rva: 0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strings_that_only_look_like_build_strings_are_refused() {
        for text in [
            // Base64-like noise: `/` and `=` are not name characters.
            "++Ab12Cd34/Ef+Gh56Ij78",
            "++Ab12Cd34+Ef56Gh78==",
            // One-letter parts.
            "++C+C",
            "++C+Live",
            "++Game+L",
            // Characters outside the allowed sets.
            "++Ga-me+Live",
            "++Game+Li+ve",
            "++Game+Li=ve",
            // Empty parts.
            "++Game+",
            "+++Live",
        ] {
            assert!(
                parse_build_string(text, StringEncoding::Utf16).is_none(),
                "{text:?}"
            );
        }
    }

    #[test]
    fn only_some_build_strings_are_strong_evidence_of_an_engine() {
        let strong = |text: &str| {
            parse_build_string(text, StringEncoding::Ascii)
                .unwrap_or_else(|| panic!("{text:?} is refused"))
                .is_engine_grade()
        };
        // A changelist, or an engine style branch.
        assert!(strong("++Game+Live-CL-123"));
        assert!(strong("++UE5+Release-5.4-CL-1"));
        assert!(strong("++Game+Release-1.2"));
        assert!(strong("++Game+UE4-Main"));
        // A bare `++A+B` is not enough: two alphanumeric runs around a `+`
        // are common in base64 noise.
        assert!(!strong("++Game+Live"));
        assert!(!strong("++VQLHqe8RT6q9OKPv+RKY9ji9i0qVQBDb6Thi"));
        assert!(!strong("++Ab+Cd"));
    }

    #[test]
    fn real_shaped_build_strings_are_kept() {
        for (text, project, branch, changelist) in [
            ("++Game+Live-CL-123", "Game", "Live", Some(123)),
            ("++UE5+Release-5.4-CL-1", "UE5", "Release-5.4", Some(1)),
            ("++Game+Live", "Game", "Live", None),
            ("++my_game2+Dev_Main-1.2", "my_game2", "Dev_Main-1.2", None),
        ] {
            let b = parse_build_string(text, StringEncoding::Ascii)
                .unwrap_or_else(|| panic!("{text:?} is refused"));
            assert_eq!(
                (b.project.as_str(), b.branch.as_str(), b.changelist),
                (project, branch, changelist),
                "{text:?}"
            );
        }
    }

    fn version(text: &str) -> Option<String> {
        engine_version_from_text(text).map(|v| v.to_string())
    }

    #[test]
    fn reads_the_forms_real_binaries_carry() {
        assert_eq!(version("4.21.1").as_deref(), Some("4.21.1"));
        assert_eq!(version("4.13.2-0+UE4").as_deref(), Some("4.13.2"));
        assert_eq!(
            version("++UE5+Release-5.4-CL-1234567").as_deref(),
            Some("5.4")
        );
        assert_eq!(
            version("4.25.1-1234567+++UE4+Release-4.25").as_deref(),
            Some("4.25.1")
        );
        assert_eq!(version("  4.27.2.0 ").as_deref(), Some("4.27.2"));
        assert_eq!(version("5.4").as_deref(), Some("5.4"));
    }

    #[test]
    fn rejects_text_that_is_not_an_engine_version() {
        for text in [
            "",
            "1.0.0.0",
            "2.6.4",
            "4",
            "4.",
            "4.x",
            "4.100",
            "hello",
            "123456.2",
            "++Game+Release-1.2-CL-5",
        ] {
            assert_eq!(version(text), None, "{text:?}");
        }
    }

    #[test]
    fn a_foreign_leading_number_falls_back_to_the_branch() {
        assert_eq!(
            version("1.0.0.0 built on ++UE4+Release-4.22").as_deref(),
            Some("4.22")
        );
    }

    #[test]
    fn build_strings_split_into_project_branch_and_changelist() {
        let b = parse_build_string("++Project+Main-CL-4242", StringEncoding::Utf16).unwrap();
        assert_eq!(
            (b.project.as_str(), b.branch.as_str(), b.changelist),
            ("Project", "Main", Some(4242))
        );
        assert_eq!(b.engine_release, None);

        let b = parse_build_string("++UE5+Release-5.4-CL-1234567", StringEncoding::Ascii).unwrap();
        assert_eq!(
            b.engine_release.map(|v| v.to_string()).as_deref(),
            Some("5.4")
        );
        assert_eq!(b.changelist, Some(1_234_567));

        let b = parse_build_string("++UE4+Release-4.21", StringEncoding::Ascii).unwrap();
        assert_eq!(b.changelist, None);
        assert_eq!(
            b.engine_release.map(|v| v.to_string()).as_deref(),
            Some("4.21")
        );

        // A release branch of a project is not an engine claim.
        let b = parse_build_string("++Game+Release-4.4-CL-9", StringEncoding::Ascii).unwrap();
        assert_eq!(b.engine_release, None);
        // The project and the version must agree.
        let b = parse_build_string("++UE4+Release-5.1", StringEncoding::Ascii).unwrap();
        assert_eq!(b.engine_release, None);
    }

    #[test]
    fn malformed_build_strings_are_refused() {
        for text in [
            "++",
            "++Game",
            "++Game+",
            "+++Game+Live",
            "++Game+-CL-5",
            "Game+Live-CL-5",
            "C++",
        ] {
            assert!(
                parse_build_string(text, StringEncoding::Ascii).is_none(),
                "{text:?}"
            );
        }
        let long = format!("++Game+{}", "x".repeat(MAX_BUILD_STRING_LEN));
        assert!(parse_build_string(&long, StringEncoding::Ascii).is_none());
        // A changelist that is not all digits is not one.
        let b = parse_build_string("++Game+Live-CL-12ab", StringEncoding::Ascii).unwrap();
        assert_eq!(b.changelist, None);
        assert_eq!(b.branch, "Live-CL-12ab");
    }
}
