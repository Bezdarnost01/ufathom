//! Identifier rules: sanitizing, keywords, case conversion and unique names.

use std::collections::BTreeSet;

use heck::{ToShoutySnakeCase, ToSnakeCase};

/// Words that cannot be identifiers in Rust 2021 or 2024 (strict and reserved).
const KEYWORDS: &[&str] = &[
    "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else", "enum", "extern",
    "false", "fn", "for", "gen", "if", "impl", "in", "let", "loop", "match", "mod", "move", "mut",
    "pub", "ref", "return", "self", "Self", "static", "struct", "super", "trait", "true", "try",
    "type", "unsafe", "use", "where", "while", "abstract", "become", "box", "do", "final", "macro",
    "override", "priv", "typeof", "unsized", "virtual", "yield",
];

/// Keywords that cannot be written as raw identifiers.
const NO_RAW: &[&str] = &["crate", "self", "Self", "super", "_"];

/// Makes `raw` a string of ASCII letters, digits and underscores that does not
/// start with a digit.
///
/// Each other ASCII character becomes `_`. A non-ASCII character becomes
/// `u` followed by its code point in hex, between underscores, so two
/// different names stay different and the result is the same on every run.
pub(crate) fn sanitize(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for c in raw.chars() {
        if c.is_ascii_alphanumeric() || c == '_' {
            out.push(c);
        } else if c.is_ascii() {
            out.push('_');
        } else {
            out.push_str(&format!("_u{:x}_", u32::from(c)));
        }
    }
    if out.is_empty() {
        out.push('_');
    }
    if out.starts_with(|c: char| c.is_ascii_digit()) {
        out.insert(0, '_');
    }
    out
}

/// Turns a word into a usable identifier: raw when it is a keyword, with a
/// trailing underscore when it is one that cannot be raw.
pub(crate) fn keyword_safe(word: &str) -> String {
    if NO_RAW.contains(&word) {
        format!("{word}_")
    } else if KEYWORDS.contains(&word) {
        format!("r#{word}")
    } else {
        word.to_owned()
    }
}

/// Snake case of a sanitized name, never escaped. Build compound names from
/// this (`set_` + name) and escape only the finished identifier.
pub(crate) fn snake_raw(raw: &str) -> String {
    let clean = sanitize(raw);
    let mut out = clean.to_snake_case();
    if out.is_empty() {
        out = clean.to_lowercase();
    }
    if out.starts_with(|c: char| c.is_ascii_digit()) {
        out.insert(0, '_');
    }
    out
}

/// A snake case field, parameter or function name.
pub(crate) fn snake(raw: &str) -> String {
    keyword_safe(&snake_raw(raw))
}

/// Upper snake case of a sanitized name, never escaped.
pub(crate) fn shouty_raw(raw: &str) -> String {
    let clean = sanitize(raw);
    let mut out = clean.to_shouty_snake_case();
    if out.is_empty() {
        out = clean.to_uppercase();
    }
    if out.starts_with(|c: char| c.is_ascii_digit()) {
        out.insert(0, '_');
    }
    out
}

/// An upper snake case constant name.
pub(crate) fn shouty(raw: &str) -> String {
    keyword_safe(&shouty_raw(raw))
}

/// A name that keeps its case: used for types and enumerators.
pub(crate) fn plain(raw: &str) -> String {
    keyword_safe(&sanitize(raw))
}

/// Module names that would collide with a special module, a well-known crate
/// or something the generated code imports. They get a `_pkg` suffix.
const RESERVED_MODULES: &[&str] = &[
    "main",
    "lib",
    "mod",
    "std",
    "core",
    "alloc",
    "test",
    "proc_macro",
    "rt",
    "ufathom_runtime",
    "default",
    "all",
];

/// Words that stay in one piece when a package name is split into snake case.
const WHOLE_WORDS: &[(&str, &str)] = &[("UObject", "Uobject")];

/// A module or feature name: snake case, never a keyword (a trailing `_`
/// replaces the raw form, since a feature name cannot be raw) and never a
/// reserved name (a `_pkg` suffix).
pub(crate) fn module_name(raw: &str) -> String {
    let mut text = sanitize(raw);
    for (word, whole) in WHOLE_WORDS {
        text = text.replace(word, whole);
    }
    let mut out = snake_raw(&text);
    if RESERVED_MODULES.contains(&out.as_str()) {
        out.push_str("_pkg");
    } else if KEYWORDS.contains(&out.as_str()) || out == "_" {
        out.push('_');
    }
    out
}

/// The last `/` or `.` separated segment of a package path.
pub(crate) fn last_segment(path: &str) -> &str {
    path.rsplit(['/', '.'])
        .find(|s| !s.is_empty())
        .unwrap_or(path)
}

/// Hands out unique names: the first claim of a name keeps it, later ones get
/// `_1`, `_2`, ... in the order they are requested.
#[derive(Debug, Default)]
pub(crate) struct Namespace {
    used: BTreeSet<String>,
}

impl Namespace {
    /// A namespace with `names` already taken.
    pub(crate) fn with_reserved(names: &[&str]) -> Self {
        Self {
            used: names.iter().map(|n| (*n).to_owned()).collect(),
        }
    }

    /// Takes `base`, or `base_N` for the smallest free `N`.
    pub(crate) fn claim(&mut self, base: &str) -> String {
        if self.used.insert(base.to_owned()) {
            return base.to_owned();
        }
        let mut n = 1u32;
        loop {
            let candidate = format!("{base}_{n}");
            if self.used.insert(candidate.clone()) {
                return candidate;
            }
            n += 1;
        }
    }
}

/// Makes text safe for a `///` line: no newlines or control characters.
pub(crate) fn doc_text(text: &str) -> String {
    text.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_replaces_invalid_characters_deterministically() {
        assert_eq!(sanitize("Foo Bar-Baz"), "Foo_Bar_Baz");
        assert_eq!(sanitize("9lives"), "_9lives");
        assert_eq!(sanitize(""), "_");
        assert_eq!(sanitize("a\u{4e2d}b"), "a_u4e2d_b");
        assert_eq!(sanitize("a\u{4e2d}b"), sanitize("a\u{4e2d}b"));
        assert_ne!(sanitize("\u{4e2d}"), sanitize("\u{6587}"));
    }

    #[test]
    fn keywords_become_raw_or_suffixed() {
        assert_eq!(snake("Type"), "r#type");
        assert_eq!(snake("Loop"), "r#loop");
        assert_eq!(snake("Self"), "self_");
        assert_eq!(snake("Super"), "super_");
        assert_eq!(snake("gen"), "r#gen");
        assert_eq!(plain("Self"), "Self_");
        assert_eq!(module_name("Type"), "type_");
        assert_eq!(module_name("Core"), "core_pkg");
    }

    #[test]
    fn reserved_and_known_module_names() {
        assert_eq!(module_name("Main"), "main_pkg");
        assert_eq!(module_name("Lib"), "lib_pkg");
        assert_eq!(module_name("Std"), "std_pkg");
        assert_eq!(module_name("Alloc"), "alloc_pkg");
        assert_eq!(module_name("Rt"), "rt_pkg");
        assert_eq!(module_name("Self"), "self_");
        assert_eq!(module_name("Crate"), "crate_");
        assert_eq!(module_name("CoreUObject"), "core_uobject");
        assert_eq!(module_name("Engine"), "engine");
        assert_eq!(module_name("AIModule"), "ai_module");
    }

    #[test]
    fn compound_names_use_the_unescaped_word() {
        // `set_` + the raw word is a valid identifier; escaping applies last.
        assert_eq!(snake("Loop"), "r#loop");
        assert_eq!(snake_raw("Loop"), "loop");
        assert_eq!(format!("set_{}", snake_raw("bLoop")), "set_b_loop");
        assert_eq!(format!("set_{}", snake_raw("Loop")), "set_loop");
        assert_eq!(shouty("Loop"), "LOOP");
    }

    #[test]
    fn snake_case_of_engine_names() {
        assert_eq!(snake("bHidden"), "b_hidden");
        assert_eq!(snake("RootComponent"), "root_component");
        assert_eq!(snake("K2_DestroyActor"), "k2_destroy_actor");
        assert_eq!(shouty("RootComponent"), "ROOT_COMPONENT");
        assert_eq!(snake("__"), "__");
    }

    #[test]
    fn namespace_suffixes_in_request_order() {
        let mut ns = Namespace::with_reserved(&["base"]);
        assert_eq!(ns.claim("base"), "base_1");
        assert_eq!(ns.claim("a"), "a");
        assert_eq!(ns.claim("a"), "a_1");
        assert_eq!(ns.claim("a"), "a_2");
        assert_eq!(ns.claim("a_1"), "a_1_1");
    }

    #[test]
    fn last_segment_of_package_paths() {
        assert_eq!(last_segment("/Script/Engine"), "Engine");
        assert_eq!(last_segment("/Game/Maps/Foo.Foo"), "Foo");
    }
}
