//! [`Signature`]: a byte pattern with wildcards.

use super::DetourError;

/// A byte pattern in which some bytes may be anything.
///
/// Parsed from text of hexadecimal bytes separated by spaces, where `?` or
/// `??` stands for any byte: `"48 89 5C 24 ?? 57 48 83 EC 20"`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Signature {
    bytes: Vec<Option<u8>>,
}

impl Signature {
    /// Parses a signature.
    ///
    /// # Errors
    ///
    /// [`DetourError::InvalidSignature`] when the text is empty, has a token
    /// that is neither `?`, `??` nor one or two hexadecimal digits, or has no
    /// fixed byte at all (a pattern of only wildcards matches everything).
    pub fn parse(text: &str) -> Result<Self, DetourError> {
        let mut bytes = Vec::new();
        for token in text.split_whitespace() {
            if token == "?" || token == "??" {
                bytes.push(None);
                continue;
            }
            let valid =
                (1..=2).contains(&token.len()) && token.bytes().all(|b| b.is_ascii_hexdigit());
            let value = if valid {
                u8::from_str_radix(token, 16).ok()
            } else {
                None
            };
            match value {
                Some(byte) => bytes.push(Some(byte)),
                None => {
                    return Err(DetourError::InvalidSignature(format!(
                        "{token:?} is not a hexadecimal byte or a wildcard"
                    )));
                }
            }
        }
        if bytes.is_empty() {
            return Err(DetourError::InvalidSignature(
                "the signature is empty".to_owned(),
            ));
        }
        if bytes.iter().all(Option::is_none) {
            return Err(DetourError::InvalidSignature(
                "the signature has no fixed byte".to_owned(),
            ));
        }
        Ok(Self { bytes })
    }

    /// Number of bytes the signature covers, wildcards included.
    #[must_use]
    pub fn len(&self) -> usize {
        self.bytes.len()
    }

    /// Always `false`: an empty signature cannot be parsed. Present for the
    /// usual pairing with [`len`](Self::len).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }

    /// Whether `memory` starts with this pattern. Shorter memory never
    /// matches.
    #[must_use]
    pub fn matches(&self, memory: &[u8]) -> bool {
        memory.len() >= self.bytes.len() && self.first_mismatch(memory).is_none()
    }

    /// Offset of the first fixed byte that differs from `memory`, comparing
    /// as far as both reach.
    pub(crate) fn first_mismatch(&self, memory: &[u8]) -> Option<usize> {
        self.bytes
            .iter()
            .zip(memory)
            .position(|(want, have)| want.is_some_and(|want| want != *have))
    }

    /// Offsets in `haystack` where the pattern matches, in order.
    pub(crate) fn positions_in<'a>(
        &'a self,
        haystack: &'a [u8],
    ) -> impl Iterator<Item = usize> + 'a {
        haystack
            .windows(self.bytes.len())
            .enumerate()
            .filter(|(_, window)| self.first_mismatch(window).is_none())
            .map(|(offset, _)| offset)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_bytes_and_wildcards() {
        let signature = Signature::parse("48 89 5C 24 ?? 57 ? 8").unwrap();
        assert_eq!(signature.len(), 8);
        assert!(signature.matches(&[0x48, 0x89, 0x5C, 0x24, 0x99, 0x57, 0x01, 0x08]));
        assert!(!signature.matches(&[0x48, 0x89, 0x5C, 0x24, 0x99, 0x58, 0x01, 0x08]));
        assert!(!signature.matches(&[0x48, 0x89]));
    }

    #[test]
    fn rejects_bad_text() {
        for text in ["", "  ", "ZZ", "123", "48 8G", "?? ??", "0x48", "48,89"] {
            assert!(
                matches!(
                    Signature::parse(text),
                    Err(DetourError::InvalidSignature(_))
                ),
                "{text:?}"
            );
        }
    }

    #[test]
    fn finds_matches_in_a_haystack() {
        let signature = Signature::parse("01 ?? 03").unwrap();
        let haystack = [0, 1, 9, 3, 1, 8, 3, 1, 2];
        assert_eq!(
            signature.positions_in(&haystack).collect::<Vec<_>>(),
            [1, 4]
        );
        assert_eq!(signature.positions_in(&[1, 2]).count(), 0);
    }
}
