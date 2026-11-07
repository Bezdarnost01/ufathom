//! Mapping a PE file into a [`Snapshot`] as a loader would.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use ufathom_mem::{ModuleInfo, Perms, RegionKind, Snapshot};

use super::{PeImage, Section};
use crate::error::ScanError;
use crate::limits::{HEAD_BYTES, MAX_LOAD_BYTES};

/// Granularity of the regions a loaded image is split into.
const PAGE: u64 = 0x1000;
/// Size of the DOS header, the least a PE file can be.
const DOS_HEADER_BYTES: u64 = 0x40;
/// Base address the file's head is mapped at while its headers are parsed.
const HEAD_BASE: u64 = 0;

/// Loads the PE file at `path` into a [`Snapshot`], laid out as a loader
/// would map it: the headers at the image base and each section at its RVA,
/// with permissions from the section flags and [`RegionKind::Image`].
///
/// The snapshot is mapped at the `ImageBase` the file declares, and the
/// returned [`ModuleInfo`] describes it (name from the file name, base,
/// `SizeOfImage`). Reading `base` with [`PeImage::parse_from_memory`] gives
/// the headers back.
///
/// Memory use: the file is not read whole. Each section's bytes are read once,
/// straight into the buffer that becomes its region, so a 300 MB file costs
/// about 300 MB of snapshot and no more. Bytes the section occupies in memory
/// beyond its raw data are zero, as in a loaded image.
///
/// A section whose raw data runs past the end of the file is **an error**
/// ([`ScanError::TruncatedFile`]), not silently zero-filled: a truncated
/// download would otherwise yield plausible but wrong results. Only the bytes
/// the section actually maps count, so padding after a section's virtual size
/// may be missing.
///
/// The file is only read, never modified, copied or executed.
///
/// # Errors
///
/// - [`ScanError::Io`] if the file cannot be opened or read.
/// - Every error of [`PeImage::parse_from_memory`] for the headers.
/// - [`ScanError::TruncatedFile`] as above.
/// - [`ScanError::LimitExceeded`] if the regions would total more than
///   [`MAX_LOAD_BYTES`].
/// - [`ScanError::Malformed`] if the image base plus size overflows, or the
///   headers overlap the first section.
pub fn load_pe_file(path: impl AsRef<Path>) -> Result<(Snapshot, ModuleInfo), ScanError> {
    let path = path.as_ref();
    let shown = path.display().to_string();
    let mut file = File::open(path).map_err(|source| ScanError::Io {
        context: shown.clone(),
        source,
    })?;
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    load(&mut file, &name, Some(shown))
}

/// Like [`load_pe_file`], for any seekable source. `name` becomes the module
/// name and the module has no path.
///
/// # Errors
///
/// The same as [`load_pe_file`].
pub fn load_pe_reader<R: Read + Seek>(
    source: &mut R,
    name: &str,
) -> Result<(Snapshot, ModuleInfo), ScanError> {
    load(source, name, None)
}

fn load<R: Read + Seek>(
    source: &mut R,
    name: &str,
    path: Option<String>,
) -> Result<(Snapshot, ModuleInfo), ScanError> {
    let context = path.clone().unwrap_or_else(|| name.to_owned());
    let io = |error: std::io::Error| ScanError::Io {
        context: context.clone(),
        source: error,
    };
    let file_len = source.seek(SeekFrom::End(0)).map_err(io)?;
    let head_len = file_len.min(HEAD_BYTES);
    source.seek(SeekFrom::Start(0)).map_err(io)?;
    let mut head = zeroed(head_len, "file head")?;
    source.read_exact(&mut head).map_err(io)?;
    if head_len < DOS_HEADER_BYTES {
        return Err(ScanError::NotPe {
            reason: format!("the file is {head_len} bytes, shorter than a DOS header"),
        });
    }
    let head_map = Snapshot::builder()
        .region_with(HEAD_BASE, head.clone(), Perms::R, RegionKind::Image)
        .build()?;
    let image = PeImage::parse_from_memory(&head_map, HEAD_BASE)?;

    let image_base = image.image_base;
    let size_of_image = u64::from(image.size_of_image);
    if image_base.checked_add(size_of_image).is_none() {
        return Err(ScanError::malformed(
            "optional header",
            format!("ImageBase {image_base:#x} + SizeOfImage {size_of_image:#x} overflows"),
        ));
    }

    let mut builder = Snapshot::builder();
    let mut mapped_total = 0u64;
    let mut charge = |bytes: u64| -> Result<(), ScanError> {
        mapped_total = mapped_total.saturating_add(bytes);
        if mapped_total > MAX_LOAD_BYTES {
            return Err(ScanError::LimitExceeded {
                what: "mapped image bytes",
                value: mapped_total,
                limit: MAX_LOAD_BYTES,
            });
        }
        Ok(())
    };

    // Sections that occupy memory, in address order, with where the next
    // one starts so a region never runs into its neighbour.
    let mut mapped: Vec<&Section> = image
        .sections
        .iter()
        .filter(|section| section.mapped_size() != 0)
        .collect();
    mapped.sort_by_key(|section| section.rva);

    let first_rva = mapped
        .first()
        .map_or(size_of_image, |section| u64::from(section.rva));
    let header_len = u64::from(image.size_of_headers.max(image.headers_end));
    if header_len > file_len {
        return Err(ScanError::TruncatedFile {
            section: "headers".to_owned(),
            needed_end: header_len,
            file_len,
        });
    }
    if header_len > head_len {
        return Err(ScanError::LimitExceeded {
            what: "size of headers",
            value: header_len,
            limit: HEAD_BYTES,
        });
    }
    if header_len > first_rva {
        return Err(ScanError::malformed(
            "section table",
            format!("headers end at {header_len:#x}, inside the first section at {first_rva:#x}"),
        ));
    }
    let header_region = round_up(header_len, PAGE).min(first_rva);
    charge(header_region)?;
    let mut header = zeroed(header_region, "headers")?;
    let header_bytes = head
        .get(..header_len as usize)
        .ok_or_else(|| ScanError::malformed("headers", "head shorter than its headers"))?;
    if let Some(slot) = header.get_mut(..header_bytes.len()) {
        slot.copy_from_slice(header_bytes);
    }
    drop(head);
    builder = builder.region_with(image_base, header, Perms::R, RegionKind::Image);

    for (index, section) in mapped.iter().enumerate() {
        let limit = mapped
            .get(index + 1)
            .map_or(size_of_image, |next| u64::from(next.rva));
        let room = limit - u64::from(section.rva);
        let region_len = round_up(u64::from(section.mapped_size()), PAGE).min(room);
        charge(region_len)?;
        let mut data = zeroed(region_len, "section")?;
        let copy_len = u64::from(section.raw_size.min(section.mapped_size()));
        if copy_len != 0 {
            let needed_end = u64::from(section.raw_offset) + copy_len;
            if needed_end > file_len {
                return Err(ScanError::TruncatedFile {
                    section: section.name.clone(),
                    needed_end,
                    file_len,
                });
            }
            source
                .seek(SeekFrom::Start(u64::from(section.raw_offset)))
                .map_err(io)?;
            let target = data.get_mut(..copy_len as usize).ok_or_else(|| {
                ScanError::malformed("section", "raw data larger than its region")
            })?;
            source.read_exact(target).map_err(io)?;
        }
        builder = builder.region_with(
            image_base + u64::from(section.rva),
            data,
            section.perms(),
            RegionKind::Image,
        );
    }

    let module = ModuleInfo {
        name: name.to_owned(),
        path,
        base: image_base,
        size: size_of_image,
    };
    builder = match &module.path {
        Some(full) => builder.module_with_path(full, module.base, module.size),
        None => builder.module(name, module.base, module.size),
    };
    Ok((builder.build()?, module))
}

/// `value` rounded up to a multiple of `unit`, saturating.
fn round_up(value: u64, unit: u64) -> u64 {
    value.div_ceil(unit).saturating_mul(unit)
}

/// A zero-filled buffer of `len` bytes. Allocation failure is an error, not
/// an abort, because `len` comes from an untrusted header.
fn zeroed(len: u64, what: &'static str) -> Result<Vec<u8>, ScanError> {
    let len = usize::try_from(len).map_err(|_| ScanError::LimitExceeded {
        what,
        value: len,
        limit: usize::MAX as u64,
    })?;
    let mut buffer = Vec::new();
    buffer
        .try_reserve_exact(len)
        .map_err(|_| ScanError::LimitExceeded {
            what,
            value: len as u64,
            limit: MAX_LOAD_BYTES,
        })?;
    buffer.resize(len, 0);
    Ok(buffer)
}
