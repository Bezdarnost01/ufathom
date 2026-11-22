//! Hotkey names and the decision logic that turns key state into an action.
//!
//! This module is plain logic: reading the keyboard and asking which window is
//! in the foreground are operating system calls that belong to the DLL shell.
//! What is decided here is how a name maps to a virtual-key code, and when a
//! sampled key state counts as a press that may act.

use std::fmt;

use serde::{Serialize, Serializer};

use crate::error::HotkeyError;

/// A single key that triggers an action. Modifier combinations are not
/// supported: one key, sampled with `GetAsyncKeyState` by the shell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hotkey {
    /// Windows virtual-key code.
    virtual_key: u8,
}

/// Names that are not a function key, a letter or a digit, with their codes.
const NAMED_KEYS: &[(&str, u8)] = &[
    ("Backspace", 0x08),
    ("Tab", 0x09),
    ("Pause", 0x13),
    ("PageUp", 0x21),
    ("PageDown", 0x22),
    ("End", 0x23),
    ("Home", 0x24),
    ("Insert", 0x2D),
    ("Delete", 0x2E),
    ("ScrollLock", 0x91),
];

/// Virtual-key code of `F1`. `F2` to `F24` follow it.
const VK_F1: u8 = 0x70;
/// Number of function keys a name can refer to.
const FUNCTION_KEYS: u8 = 24;

impl Hotkey {
    /// Parses a key name such as `F8`, `Insert`, `K` or `7`. Matching ignores
    /// case. Returns `Ok(None)` for `none` and the empty string, which switch
    /// the hotkey off.
    ///
    /// # Errors
    ///
    /// [`HotkeyError`] for a name that is none of the supported keys.
    pub fn parse(name: &str) -> Result<Option<Self>, HotkeyError> {
        let trimmed = name.trim();
        if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("none") {
            return Ok(None);
        }
        if let Some(code) = function_key(trimmed) {
            return Ok(Some(Self { virtual_key: code }));
        }
        if let Some((_, code)) = NAMED_KEYS
            .iter()
            .find(|(candidate, _)| candidate.eq_ignore_ascii_case(trimmed))
        {
            return Ok(Some(Self { virtual_key: *code }));
        }
        let mut chars = trimmed.chars();
        if let (Some(only), None) = (chars.next(), chars.next())
            && only.is_ascii_alphanumeric()
        {
            // Virtual-key codes of letters and digits equal their upper-case ASCII value.
            let code = u8::try_from(only.to_ascii_uppercase()).unwrap_or(0);
            return Ok(Some(Self { virtual_key: code }));
        }
        Err(HotkeyError {
            name: trimmed.to_owned(),
        })
    }

    /// The Windows virtual-key code to sample.
    #[must_use]
    pub fn virtual_key(self) -> u8 {
        self.virtual_key
    }
}

fn function_key(name: &str) -> Option<u8> {
    let digits = name.strip_prefix(['F', 'f'])?;
    let number: u8 = digits.parse().ok()?;
    (1..=FUNCTION_KEYS)
        .contains(&number)
        .then(|| VK_F1.saturating_add(number.saturating_sub(1)))
}

impl fmt::Display for Hotkey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let code = self.virtual_key;
        if (VK_F1..VK_F1.saturating_add(FUNCTION_KEYS)).contains(&code) {
            return write!(f, "F{}", code.saturating_sub(VK_F1).saturating_add(1));
        }
        if let Some((name, _)) = NAMED_KEYS.iter().find(|(_, named)| *named == code) {
            return f.write_str(name);
        }
        write!(f, "{}", char::from(code))
    }
}

impl Serialize for Hotkey {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

/// Whether a window that belongs to this process is in the foreground.
///
/// `foreground_process` is the process that owns the foreground window, or
/// `None` when there is no foreground window or its owner cannot be told.
#[must_use]
pub fn is_own_process_foreground(foreground_process: Option<u32>, own_process: u32) -> bool {
    foreground_process == Some(own_process)
}

/// Turns samples of one key into presses.
///
/// A press is the transition from up to down. It acts only if the process was
/// in the foreground at that sample. A key already held when the window comes
/// to the front does not act, and a key held down does not repeat.
#[derive(Debug, Default, Clone, Copy)]
pub struct HotkeyTracker {
    was_down: bool,
}

impl HotkeyTracker {
    /// A tracker that has seen the key up.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Feeds one sample. Returns `true` when this sample is a press that may
    /// act. The key state is tracked even while the process is in the
    /// background.
    pub fn update(&mut self, key_down: bool, process_in_foreground: bool) -> bool {
        let pressed = key_down && !self.was_down;
        self.was_down = key_down;
        pressed && process_in_foreground
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

    fn code(name: &str) -> Option<u8> {
        Hotkey::parse(name).unwrap().map(Hotkey::virtual_key)
    }

    #[test]
    fn the_default_keys_parse() {
        assert_eq!(code("F8"), Some(0x77));
        assert_eq!(code("F6"), Some(0x75));
    }

    #[test]
    fn function_keys_cover_f1_to_f24_only() {
        assert_eq!(code("F1"), Some(0x70));
        assert_eq!(code("f24"), Some(0x87));
        assert!(Hotkey::parse("F0").is_err());
        assert!(Hotkey::parse("F25").is_err());
        assert_eq!(code("F"), Some(b'F'), "a lone F is the letter");
        assert!(Hotkey::parse("F8x").is_err());
    }

    #[test]
    fn letters_digits_and_names_parse_case_insensitively() {
        assert_eq!(code("k"), Some(b'K'));
        assert_eq!(code("7"), Some(b'7'));
        assert_eq!(code("insert"), Some(0x2D));
        assert_eq!(code(" PageDown "), Some(0x22));
    }

    #[test]
    fn none_and_empty_switch_the_key_off() {
        assert_eq!(code("none"), None);
        assert_eq!(code(""), None);
        assert_eq!(code("  "), None);
    }

    #[test]
    fn unknown_names_are_errors() {
        assert!(Hotkey::parse("Ctrl+F8").is_err());
        assert!(Hotkey::parse("Spacebar").is_err());
        assert!(Hotkey::parse("é").is_err());
    }

    #[test]
    fn display_round_trips_through_parse() {
        for name in ["F1", "F8", "F24", "Insert", "Home", "K", "7"] {
            let key = Hotkey::parse(name).unwrap().unwrap();
            assert_eq!(key.to_string(), name);
            assert_eq!(Hotkey::parse(&key.to_string()).unwrap(), Some(key));
        }
    }

    #[test]
    fn only_the_owner_of_the_foreground_window_counts() {
        assert!(is_own_process_foreground(Some(42), 42));
        assert!(!is_own_process_foreground(Some(43), 42));
        assert!(!is_own_process_foreground(None, 42));
    }

    #[test]
    fn a_press_in_the_foreground_acts_once() {
        let mut tracker = HotkeyTracker::new();
        assert!(!tracker.update(false, true));
        assert!(tracker.update(true, true));
        assert!(!tracker.update(true, true), "holding must not repeat");
        assert!(!tracker.update(false, true));
        assert!(tracker.update(true, true), "a new press acts again");
    }

    #[test]
    fn a_press_in_the_background_never_acts() {
        let mut tracker = HotkeyTracker::new();
        assert!(!tracker.update(true, false));
        assert!(!tracker.update(false, false));
    }

    #[test]
    fn a_key_held_while_the_window_comes_to_the_front_does_not_act() {
        let mut tracker = HotkeyTracker::new();
        assert!(!tracker.update(true, false));
        assert!(!tracker.update(true, true));
        assert!(!tracker.update(false, true));
        assert!(tracker.update(true, true));
    }

    #[test]
    fn leaving_the_foreground_mid_press_does_not_act_on_release_and_repress() {
        let mut tracker = HotkeyTracker::new();
        assert!(tracker.update(true, true));
        assert!(!tracker.update(true, false));
        assert!(!tracker.update(false, false));
        assert!(!tracker.update(true, false));
    }
}
