//! One bounded pass over the read-only sections for build strings and
//! marker strings.
//!
//! The sections are read in large chunks through the reader and searched
//! locally. A chunk is read with a few bytes of context before it and a full
//! overlap after it, so strings that straddle a boundary are neither lost nor
//! found twice.

use std::ops::Range;

use memchr::memmem::Finder;
use ufathom_mem::MemoryReader;
use ufathom_model::EngineVersion;

use super::markers::{MARKERS, Marker};
use super::text::{BuildString, StringEncoding, is_build_char, parse_build_string};
use crate::limits::{
    MAX_BUILD_CANDIDATES, MAX_BUILD_STRING_LEN, MAX_SCAN_BYTES_PER_SECTION, MAX_SCAN_BYTES_TOTAL,
    SCAN_CHUNK_BYTES, SCAN_OVERLAP_BYTES,
};
use crate::pe::{PeImage, Section};

/// Bytes read before a chunk so the character in front of a match is known.
const CONTEXT_BYTES: u64 = 2;

/// A marker string found in the image.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MarkerHit {
    /// The marker string.
    pub name: &'static str,
    /// The lower bound it implies.
    pub minimum: EngineVersion,
    /// Why presence implies that bound.
    pub reason: &'static str,
    /// How the string is stored.
    pub encoding: StringEncoding,
    /// Name of the section that holds the first copy.
    pub section: String,
    /// RVA of the first copy.
    pub rva: u32,
}

/// What one scan found.
#[derive(Default)]
pub(super) struct ScanOutput {
    /// Distinct build strings, in the order found.
    pub builds: Vec<BuildString>,
    /// The first hit of each marker in each encoding, in the order found.
    pub hits: Vec<MarkerHit>,
    /// Limits reached and reads that failed.
    pub notes: Vec<String>,
}

struct MarkerFinders {
    marker: &'static Marker,
    ascii: Finder<'static>,
    wide: Finder<'static>,
}

struct Scanner {
    plus_ascii: Finder<'static>,
    plus_wide: Finder<'static>,
    markers: Vec<MarkerFinders>,
    out: ScanOutput,
    dropped_builds: u64,
}

/// Searches the read-only sections of `image`. Never fails: a section that
/// cannot be read is skipped and noted.
pub(super) fn scan_sections(mem: &dyn MemoryReader, image: &PeImage) -> ScanOutput {
    let mut scanner = Scanner {
        plus_ascii: Finder::new(b"++").into_owned(),
        plus_wide: Finder::new(b"+\0+\0").into_owned(),
        markers: MARKERS
            .iter()
            .map(|marker| MarkerFinders {
                marker,
                ascii: Finder::new(marker.name.as_bytes()).into_owned(),
                wide: Finder::new(&wide_bytes(marker.name)).into_owned(),
            })
            .collect(),
        out: ScanOutput::default(),
        dropped_builds: 0,
    };
    let mut budget = MAX_SCAN_BYTES_TOTAL;
    let mut unreadable = 0u64;
    let mut buffer = Vec::new();
    let mut scanned_any = false;
    for section in image
        .sections
        .iter()
        .filter(|section| section.is_read_only_data() && section.mapped_size() != 0)
    {
        scanned_any = true;
        let wanted = u64::from(section.mapped_size());
        let length = wanted.min(MAX_SCAN_BYTES_PER_SECTION).min(budget);
        if length < wanted {
            scanner.out.notes.push(format!(
                "section `{}` was scanned for {length:#x} of {wanted:#x} bytes (scan limit)",
                section.name
            ));
        }
        budget -= length;
        unreadable += scanner.scan_section(mem, image, section, length, &mut buffer);
    }
    if !scanned_any {
        scanner
            .out
            .notes
            .push("no read-only data section to scan for strings".to_owned());
    }
    if unreadable != 0 {
        scanner.out.notes.push(format!(
            "{unreadable:#x} bytes of read-only sections could not be read and were not scanned"
        ));
    }
    if scanner.dropped_builds != 0 {
        scanner.out.notes.push(format!(
            "{} build strings beyond the first {MAX_BUILD_CANDIDATES} were ignored",
            scanner.dropped_builds
        ));
    }
    scanner.out
}

fn wide_bytes(text: &str) -> Vec<u8> {
    text.encode_utf16().flat_map(u16::to_le_bytes).collect()
}

impl Scanner {
    /// Scans the first `length` bytes of `section`. Returns the number of
    /// bytes that could not be read.
    fn scan_section(
        &mut self,
        mem: &dyn MemoryReader,
        image: &PeImage,
        section: &Section,
        length: u64,
        buffer: &mut Vec<u8>,
    ) -> u64 {
        let mut unreadable = 0u64;
        let mut start = 0u64;
        while start < length {
            let end = (start + SCAN_CHUNK_BYTES).min(length);
            let read_start = start.saturating_sub(CONTEXT_BYTES);
            let read_end = (end + SCAN_OVERLAP_BYTES).min(length);
            let Some(addr) =
                image.address_of(u64::from(section.rva) + read_start, read_end - read_start)
            else {
                unreadable += end - start;
                start = end;
                continue;
            };
            buffer.clear();
            buffer.resize((read_end - read_start) as usize, 0);
            if let Err(error) = mem.read_exact(addr, buffer) {
                tracing::debug!(section = %section.name, %error, "chunk not readable");
                unreadable += end - start;
            } else {
                self.scan_window(section, buffer, read_start, start..end);
            }
            start = end;
        }
        unreadable
    }

    /// Searches `window`, which starts `window_start` bytes into the section,
    /// and accepts only matches that begin inside `accept` (offsets from the
    /// section start).
    fn scan_window(
        &mut self,
        section: &Section,
        window: &[u8],
        window_start: u64,
        accept: Range<u64>,
    ) {
        let in_range = |index: usize| accept.contains(&(window_start + index as u64));
        let rva_of = |index: usize| -> u32 {
            u32::try_from(u64::from(section.rva) + window_start + index as u64).unwrap_or(u32::MAX)
        };
        let even = |index: usize| (u64::from(section.rva) + window_start + index as u64) % 2 == 0;

        let ascii: Vec<usize> = self
            .plus_ascii
            .find_iter(window)
            .filter(|i| in_range(*i))
            .collect();
        for index in ascii {
            if let Some(text) = ascii_token(window, index) {
                self.add_build(&text, StringEncoding::Ascii, section, rva_of(index));
            }
        }
        let wide: Vec<usize> = self
            .plus_wide
            .find_iter(window)
            .filter(|i| in_range(*i) && even(*i))
            .collect();
        for index in wide {
            if let Some(text) = wide_token(window, index) {
                self.add_build(&text, StringEncoding::Utf16, section, rva_of(index));
            }
        }

        for finders in &self.markers {
            for (encoding, finder) in [
                (StringEncoding::Ascii, &finders.ascii),
                (StringEncoding::Utf16, &finders.wide),
            ] {
                let already = self
                    .out
                    .hits
                    .iter()
                    .any(|hit| hit.name == finders.marker.name && hit.encoding == encoding);
                if already {
                    continue;
                }
                let found = finder.find_iter(window).find(|index| {
                    in_range(*index)
                        && (encoding == StringEncoding::Ascii || even(*index))
                        && is_whole_word(window, *index, finder.needle().len(), encoding)
                });
                if let Some(index) = found {
                    self.out.hits.push(MarkerHit {
                        name: finders.marker.name,
                        minimum: EngineVersion {
                            major: finders.marker.minimum.0,
                            minor: finders.marker.minimum.1,
                            patch: 0,
                        },
                        reason: finders.marker.reason,
                        encoding,
                        section: section.name.clone(),
                        rva: rva_of(index),
                    });
                }
            }
        }
    }

    fn add_build(&mut self, text: &str, encoding: StringEncoding, section: &Section, rva: u32) {
        let Some(mut build) = parse_build_string(text, encoding) else {
            return;
        };
        if self
            .out
            .builds
            .iter()
            .any(|known| known.text == build.text && known.encoding == encoding)
        {
            return;
        }
        if self.out.builds.len() >= MAX_BUILD_CANDIDATES {
            self.dropped_builds += 1;
            return;
        }
        build.section = section.name.clone();
        build.rva = rva;
        self.out.builds.push(build);
    }
}

/// The ASCII build-string token starting at `index`, if it is delimited and
/// within the length limit.
fn ascii_token(window: &[u8], index: usize) -> Option<String> {
    if index > 0
        && window
            .get(index - 1)
            .is_some_and(|byte| is_build_char(*byte))
    {
        return None;
    }
    let mut end = index;
    while window.get(end).is_some_and(|byte| is_build_char(*byte)) {
        end += 1;
        if end - index > MAX_BUILD_STRING_LEN {
            return None;
        }
    }
    String::from_utf8(window.get(index..end)?.to_vec()).ok()
}

/// The UTF-16 build-string token starting at `index`.
fn wide_token(window: &[u8], index: usize) -> Option<String> {
    if index >= 2 {
        let high = window.get(index - 1).copied()?;
        let low = window.get(index - 2).copied()?;
        if high == 0 && is_build_char(low) {
            return None;
        }
    }
    let mut text = String::new();
    let mut at = index;
    loop {
        match (window.get(at).copied(), window.get(at + 1).copied()) {
            (Some(low), Some(0)) if is_build_char(low) => {
                text.push(char::from(low));
                at += 2;
                if text.len() > MAX_BUILD_STRING_LEN {
                    return None;
                }
            }
            _ => break,
        }
    }
    Some(text)
}

/// Whether the `len` bytes at `index` are not part of a longer word.
fn is_whole_word(window: &[u8], index: usize, len: usize, encoding: StringEncoding) -> bool {
    let word = |byte: u8| byte.is_ascii_alphanumeric() || byte == b'_';
    match encoding {
        StringEncoding::Ascii => {
            let before = index
                .checked_sub(1)
                .and_then(|at| window.get(at))
                .is_some_and(|byte| word(*byte));
            let after = window.get(index + len).is_some_and(|byte| word(*byte));
            !before && !after
        }
        StringEncoding::Utf16 => {
            let wide_word = |at: Option<usize>| {
                at.and_then(|at| Some((window.get(at)?, window.get(at + 1)?)))
                    .is_some_and(|(low, high)| *high == 0 && word(*low))
            };
            let before = wide_word(index.checked_sub(2));
            let after = wide_word(Some(index + len));
            !before && !after
        }
    }
}
