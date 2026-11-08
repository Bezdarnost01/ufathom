//! The `--layout FILE` option: values the user states instead of solving.
//!
//! The file is TOML or JSON, chosen by its extension. It has two optional
//! parts:
//!
//! ```toml
//! pointer_transform = [{ Xor = 255 }, "Not"]
//!
//! [overrides]
//! "uobject.class" = 16
//! ustruct.super_struct = 48   # a dotted key is the same as the quoted one
//! ```
//!
//! `overrides` maps the dotted field names of
//! [`LayoutOverrides`](ufathom_scan::LayoutOverrides) to offsets or sizes.
//! `pointer_transform` is the list of steps that turns the stored object array
//! pointer into the real one, as in `ufathom.toml`. Anything else in the file
//! is an error, so a typo is not silent.

use std::path::Path;

use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Map, Value};
use ufathom_model::layout::TransformOp;
use ufathom_scan::LayoutOverrides;

/// What a layout file states.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LayoutFile {
    /// Fields stated instead of solved.
    pub overrides: LayoutOverrides,
    /// Steps that decode the object array pointer.
    pub pointer_transform: Vec<TransformOp>,
}

/// Reads and checks the layout file at `path`.
///
/// # Errors
///
/// The file cannot be read or parsed, has an unknown key or override name, or
/// a value that is not a number that fits in 32 bits.
pub fn load(path: &Path) -> Result<LayoutFile> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("cannot read the layout file {}", path.display()))?;
    let is_json = path
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("json"));
    let is_toml = path
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("toml"));
    if !is_json && !is_toml {
        bail!(
            "the layout file {} must end in .toml or .json",
            path.display()
        );
    }
    parse(&text, is_json).with_context(|| format!("invalid layout file {}", path.display()))
}

/// Parses layout text. `json` selects the syntax.
fn parse(text: &str, json: bool) -> Result<LayoutFile> {
    let value: Value = if json {
        serde_json::from_str(text).map_err(|error| anyhow!("not valid JSON: {error}"))?
    } else {
        toml::from_str(text).map_err(|error| anyhow!("not valid TOML: {error}"))?
    };
    let Value::Object(top) = value else {
        bail!("the file must hold an object with `overrides` and/or `pointer_transform`");
    };
    let mut file = LayoutFile::default();
    for (key, value) in top {
        match key.as_str() {
            "overrides" => {
                let Value::Object(table) = value else {
                    bail!("`overrides` must be a table of field names and numbers");
                };
                let mut flat = Vec::new();
                flatten("", &table, &mut flat);
                for (name, value) in flat {
                    let number = value
                        .as_u64()
                        .and_then(|number| u32::try_from(number).ok())
                        .ok_or_else(|| {
                            anyhow!("override `{name}` must be a number from 0 to 4294967295")
                        })?;
                    if !file.overrides.set(&name, number) {
                        bail!(
                            "unknown override `{name}`; the names are: {}",
                            LayoutOverrides::NAMES.join(", ")
                        );
                    }
                }
            }
            "pointer_transform" => {
                file.pointer_transform = serde_json::from_value(value).map_err(|error| {
                    anyhow!(
                        "`pointer_transform` must be a list of steps such as {{ Xor = 255 }} or \"Not\": {error}"
                    )
                })?;
            }
            other => bail!("unknown key `{other}`; use `overrides` and `pointer_transform`"),
        }
    }
    Ok(file)
}

/// Joins nested tables into dotted names, so `uobject.class` works both
/// quoted and as a TOML dotted key.
fn flatten<'a>(prefix: &str, table: &'a Map<String, Value>, out: &mut Vec<(String, &'a Value)>) {
    for (key, value) in table {
        let name = if prefix.is_empty() {
            key.clone()
        } else {
            format!("{prefix}.{key}")
        };
        match value {
            Value::Object(inner) => flatten(&name, inner, out),
            other => out.push((name, other)),
        }
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
    fn toml_reads_quoted_and_dotted_names_and_a_transform() {
        let file = parse(
            r#"
            pointer_transform = [{ Xor = 255 }, "Not"]
            [overrides]
            "uobject.class" = 16
            ustruct.super_struct = 48
            "#,
            false,
        )
        .unwrap();
        assert_eq!(file.overrides.uobject_class, Some(16));
        assert_eq!(file.overrides.ustruct_super_struct, Some(48));
        assert_eq!(
            file.pointer_transform,
            vec![TransformOp::Xor(255), TransformOp::Not]
        );
    }

    #[test]
    fn json_reads_the_same_things() {
        let file = parse(
            r#"{ "overrides": { "uobject.class": 16, "fname": { "size": 12 } },
                 "pointer_transform": [{ "RotateLeft": 3 }] }"#,
            true,
        )
        .unwrap();
        assert_eq!(file.overrides.uobject_class, Some(16));
        assert_eq!(file.overrides.fname_size, Some(12));
        assert_eq!(file.pointer_transform, vec![TransformOp::RotateLeft(3)]);
    }

    #[test]
    fn an_empty_file_states_nothing() {
        assert_eq!(parse("", false).unwrap(), LayoutFile::default());
        assert_eq!(parse("{}", true).unwrap(), LayoutFile::default());
    }

    #[test]
    fn an_unknown_override_is_refused_and_the_valid_names_are_listed() {
        let error = parse("[overrides]\n\"uobject.klass\" = 1", false).unwrap_err();
        let text = error.to_string();
        assert!(text.contains("uobject.klass"), "{text}");
        assert!(text.contains("uobject.class"), "{text}");
    }

    #[test]
    fn an_unknown_top_level_key_is_refused() {
        let error = parse("overides = 1", false).unwrap_err();
        assert!(error.to_string().contains("overides"), "{error}");
    }

    #[test]
    fn a_value_that_is_not_a_32_bit_number_is_refused() {
        for bad in ["-1", "4294967296", "\"16\"", "1.5", "true"] {
            let text = format!("[overrides]\n\"uobject.class\" = {bad}");
            let error = parse(&text, false).unwrap_err();
            assert!(
                error.to_string().contains("uobject.class"),
                "{bad}: {error}"
            );
        }
    }

    #[test]
    fn a_bad_transform_is_refused() {
        assert!(parse(r#"pointer_transform = ["Rot13"]"#, false).is_err());
        assert!(parse(r#"pointer_transform = 3"#, false).is_err());
    }

    #[test]
    fn broken_syntax_and_a_non_object_are_refused() {
        assert!(parse("= =", false).is_err());
        assert!(parse("[1, 2]", true).is_err());
        assert!(parse("{", true).is_err());
    }

    #[test]
    fn the_extension_decides_the_syntax_and_must_be_known() {
        let dir = tempfile::tempdir().unwrap();
        let toml_path = dir.path().join("layout.TOML");
        std::fs::write(&toml_path, "[overrides]\n\"uobject.class\" = 16").unwrap();
        assert_eq!(load(&toml_path).unwrap().overrides.uobject_class, Some(16));

        let json_path = dir.path().join("layout.json");
        std::fs::write(&json_path, r#"{"overrides":{"uobject.class":8}}"#).unwrap();
        assert_eq!(load(&json_path).unwrap().overrides.uobject_class, Some(8));

        let other = dir.path().join("layout.txt");
        std::fs::write(&other, "{}").unwrap();
        let error = load(&other).unwrap_err();
        assert!(format!("{error:#}").contains(".toml or .json"), "{error:#}");

        let missing = dir.path().join("none.toml");
        let error = load(&missing).unwrap_err();
        assert!(format!("{error:#}").contains("cannot read"), "{error:#}");
    }
}
