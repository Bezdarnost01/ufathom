//! zstd decoding of the payload, bounded by the size the header declares.

use std::io::Read;

use ruzstd::decoding::StreamingDecoder;

use crate::error::UsmapError;

/// Largest initial allocation made on the strength of a header claim. The
/// buffer grows past this only as real data arrives.
const MAX_PREALLOCATION: usize = 16 * 1024 * 1024;

/// Decodes a single zstd frame that must produce exactly `declared` bytes.
///
/// The caller has already checked `declared` against
/// [`MAX_DECOMPRESSED_SIZE`](crate::MAX_DECOMPRESSED_SIZE). Decoding stops one
/// byte past `declared`, so a stream that inflates far beyond its header is
/// never fully expanded. The decoder's own window limit bounds the rest of its
/// memory use.
pub(crate) fn decode_zstd(compressed: &[u8], declared: u32) -> Result<Vec<u8>, UsmapError> {
    let wanted = usize::try_from(declared).unwrap_or(usize::MAX);
    let mut decoder =
        StreamingDecoder::new(compressed).map_err(|error| UsmapError::Decompress {
            message: error.to_string(),
        })?;
    let mut output = Vec::with_capacity(wanted.min(MAX_PREALLOCATION));
    let mut limited = (&mut decoder).take(u64::from(declared).saturating_add(1));
    limited
        .read_to_end(&mut output)
        .map_err(|error| UsmapError::Decompress {
            message: error.to_string(),
        })?;
    if output.len() != wanted {
        return Err(UsmapError::DecompressedSizeMismatch {
            declared,
            actual: output.len(),
        });
    }
    Ok(output)
}
