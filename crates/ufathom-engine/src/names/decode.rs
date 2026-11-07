//! Turning name entry bytes into text.

use ufathom_mem::{Addr, MemoryReader, MemoryReaderExt};

use crate::error::EngineError;
use crate::limits::MAX_NAME_LENGTH;

/// Units read per request when scanning for a wide terminator.
const WIDE_CHUNK_UNITS: usize = 64;

/// Decodes a narrow entry: UTF-8, or Latin-1 when the bytes are not UTF-8.
pub(crate) fn decode_narrow(bytes: &[u8]) -> String {
    match std::str::from_utf8(bytes) {
        Ok(text) => text.to_owned(),
        Err(_) => bytes.iter().map(|&byte| char::from(byte)).collect(),
    }
}

/// Decodes a wide entry: little-endian UTF-16, lossily.
pub(crate) fn decode_wide(bytes: &[u8]) -> String {
    let units: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|pair| {
            let low = pair.first().copied().unwrap_or(0);
            let high = pair.get(1).copied().unwrap_or(0);
            u16::from_le_bytes([low, high])
        })
        .collect();
    String::from_utf16_lossy(&units)
}

/// Decodes `bytes` as a wide or narrow entry.
pub(crate) fn decode(bytes: &[u8], wide: bool) -> String {
    if wide {
        decode_wide(bytes)
    } else {
        decode_narrow(bytes)
    }
}

/// Reads a NUL-terminated string of at most [`MAX_NAME_LENGTH`] characters.
///
/// Reads in small requests and falls back to unit reads inside a request that
/// is only partly readable, so a terminator just before an unmapped page is
/// still found.
pub(crate) fn read_terminated(
    mem: &dyn MemoryReader,
    addr: Addr,
    wide: bool,
) -> Result<String, EngineError> {
    if !wide {
        let bytes = mem
            .read_cstr(addr, MAX_NAME_LENGTH)
            .map_err(EngineError::read(addr, "name characters"))?;
        return Ok(decode_narrow(&bytes));
    }
    let mut collected: Vec<u8> = Vec::new();
    let mut cursor = addr;
    let mut units = 0usize;
    while units <= MAX_NAME_LENGTH {
        let want = WIDE_CHUNK_UNITS.min(MAX_NAME_LENGTH + 1 - units);
        let mut chunk = vec![0u8; want * 2];
        let got = if mem.read_exact(cursor, &mut chunk).is_ok() {
            want
        } else {
            read_units(mem, cursor, &mut chunk)
        };
        for pair in chunk.chunks_exact(2).take(got) {
            if pair == [0, 0] {
                return Ok(decode_wide(&collected));
            }
            collected.extend_from_slice(pair);
        }
        if got < want {
            return Err(EngineError::Read {
                addr: cursor.wrapping_add((got * 2) as u64),
                what: "wide name characters",
                source: ufathom_mem::MemError::Unreadable {
                    addr: cursor.wrapping_add((got * 2) as u64),
                    len: 2,
                },
            });
        }
        units += want;
        cursor = cursor.wrapping_add((want * 2) as u64);
    }
    Err(EngineError::Limit {
        addr,
        what: "name length",
        value: units as u64,
        limit: MAX_NAME_LENGTH as u64,
    })
}

/// Fills `chunk` unit by unit and returns how many whole units were read.
fn read_units(mem: &dyn MemoryReader, addr: Addr, chunk: &mut [u8]) -> usize {
    let mut filled = 0;
    for (n, pair) in chunk.chunks_exact_mut(2).enumerate() {
        let Some(at) = addr.checked_add((n * 2) as u64) else {
            break;
        };
        if mem.read_exact(at, pair).is_err() {
            break;
        }
        filled = n + 1;
    }
    filled
}
