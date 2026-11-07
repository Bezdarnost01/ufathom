//! Hand-built PE files for tests: headers, sections, resource trees and
//! `VS_VERSIONINFO` blocks written byte by byte, independently of the code
//! under test and of the testkit.

#![allow(
    dead_code,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "test helpers: each test crate uses a subset, and a mistake in a fixture may panic"
)]

use std::io::Cursor;

use ufathom_mem::{ModuleInfo, Snapshot};
use ufathom_scan::{PeImage, ScanError, VersionFacts, detect_version, load_pe_reader};

/// Preferred load address of the hand-built images.
pub const IMAGE_BASE: u64 = 0x1_4000_0000;
/// File alignment of the hand-built images.
pub const FILE_ALIGN: usize = 0x200;
/// Size of the headers of the hand-built images.
pub const HEADERS_SIZE: usize = 0x400;
/// Offset of the NT headers.
pub const NT_OFFSET: usize = 0x80;
/// Set in a resource entry that points to a directory.
pub const SUBDIR: u32 = 0x8000_0000;

/// Section flags: code, read-only data, writable data.
pub const CODE: u32 = 0x6000_0020;
pub const RDATA: u32 = 0x4000_0040;
pub const DATA: u32 = 0xC000_0040;

pub fn utf16z(text: &str) -> Vec<u8> {
    text.encode_utf16()
        .chain(std::iter::once(0))
        .flat_map(u16::to_le_bytes)
        .collect()
}

pub fn utf16(text: &str) -> Vec<u8> {
    text.encode_utf16().flat_map(u16::to_le_bytes).collect()
}

/// One section of a hand-built image.
#[derive(Clone)]
pub struct SectionSpec {
    pub name: String,
    pub rva: u32,
    pub data: Vec<u8>,
    pub virtual_size: Option<u32>,
    pub flags: u32,
}

/// A hand-built PE file.
#[derive(Clone)]
pub struct PeSpec {
    pub pe32: bool,
    pub image_base: u64,
    pub sections: Vec<SectionSpec>,
    /// Resource data directory: RVA and size.
    pub resource: Option<(u32, u32)>,
    pub entry: u32,
}

impl PeSpec {
    pub fn new() -> Self {
        Self {
            pe32: false,
            image_base: IMAGE_BASE,
            sections: Vec::new(),
            resource: None,
            entry: 0,
        }
    }

    pub fn section(mut self, name: &str, rva: u32, data: Vec<u8>, flags: u32) -> Self {
        self.sections.push(SectionSpec {
            name: name.to_owned(),
            rva,
            data,
            virtual_size: None,
            flags,
        });
        self
    }

    pub fn resource_dir(mut self, rva: u32, size: u32) -> Self {
        self.resource = Some((rva, size));
        self
    }

    /// `SizeOfImage`: the end of the last section, rounded to a page.
    pub fn size_of_image(&self) -> u32 {
        self.sections
            .iter()
            .map(|s| {
                s.rva
                    + s.virtual_size
                        .filter(|v| *v != 0)
                        .unwrap_or(s.data.len() as u32)
            })
            .max()
            .unwrap_or(0)
            .next_multiple_of(0x1000)
            .max(0x1000)
    }

    /// The file's bytes.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut file = vec![0u8; HEADERS_SIZE];
        put(&mut file, 0, b"MZ");
        put32(&mut file, 0x3C, NT_OFFSET as u32);
        let optional_size: usize = if self.pe32 { 0xE0 } else { 0xF0 };
        put(&mut file, NT_OFFSET, b"PE\0\0");
        let coff = NT_OFFSET + 4;
        put16(&mut file, coff, if self.pe32 { 0x14C } else { 0x8664 });
        put16(&mut file, coff + 2, self.sections.len() as u16);
        put32(&mut file, coff + 4, 0x6000_0000);
        put16(&mut file, coff + 16, optional_size as u16);
        put16(&mut file, coff + 18, 0x22);
        let opt = coff + 20;
        put16(&mut file, opt, if self.pe32 { 0x10B } else { 0x20B });
        put32(&mut file, opt + 16, self.entry);
        let dirs = if self.pe32 {
            put32(&mut file, opt + 28, self.image_base as u32);
            put32(&mut file, opt + 92, 16);
            opt + 96
        } else {
            put64(&mut file, opt + 24, self.image_base);
            put32(&mut file, opt + 108, 16);
            opt + 112
        };
        put32(&mut file, opt + 32, 0x1000);
        put32(&mut file, opt + 36, FILE_ALIGN as u32);
        put32(&mut file, opt + 56, self.size_of_image());
        put32(&mut file, opt + 60, HEADERS_SIZE as u32);
        put16(&mut file, opt + 68, 2);
        if let Some((rva, size)) = self.resource {
            put32(&mut file, dirs + 16, rva);
            put32(&mut file, dirs + 20, size);
        }
        let table = opt + optional_size;
        let mut raw_offset = HEADERS_SIZE;
        for (index, section) in self.sections.iter().enumerate() {
            let at = table + index * 40;
            let mut name = [0u8; 8];
            name[..section.name.len().min(8)]
                .copy_from_slice(&section.name.as_bytes()[..section.name.len().min(8)]);
            put(&mut file, at, &name);
            let raw_size = section.data.len().next_multiple_of(FILE_ALIGN);
            put32(
                &mut file,
                at + 8,
                section.virtual_size.unwrap_or(section.data.len() as u32),
            );
            put32(&mut file, at + 12, section.rva);
            put32(&mut file, at + 16, raw_size as u32);
            put32(&mut file, at + 20, raw_offset as u32);
            put32(&mut file, at + 36, section.flags);
            raw_offset += raw_size;
        }
        for section in &self.sections {
            let raw_size = section.data.len().next_multiple_of(FILE_ALIGN);
            let start = file.len();
            file.extend_from_slice(&section.data);
            file.resize(start + raw_size, 0);
        }
        file
    }
}

pub fn put(buf: &mut [u8], at: usize, data: &[u8]) {
    buf[at..at + data.len()].copy_from_slice(data);
}
pub fn put16(buf: &mut [u8], at: usize, value: u16) {
    put(buf, at, &value.to_le_bytes());
}
pub fn put32(buf: &mut [u8], at: usize, value: u32) {
    put(buf, at, &value.to_le_bytes());
}
pub fn put64(buf: &mut [u8], at: usize, value: u64) {
    put(buf, at, &value.to_le_bytes());
}

/// Loads file bytes the way `load_pe_file` loads a file.
pub fn load(bytes: &[u8]) -> Result<(Snapshot, ModuleInfo), ScanError> {
    load_pe_reader(&mut Cursor::new(bytes), "test.exe")
}

/// Loads, parses and detects.
pub fn facts_of(bytes: &[u8]) -> VersionFacts {
    let (snapshot, module) = load(bytes).unwrap();
    let image = PeImage::parse_from_memory(&snapshot, module.base).unwrap();
    detect_version(&snapshot, &image)
}

// ---- resources ----------------------------------------------------------

/// An `IMAGE_RESOURCE_DIRECTORY` with the given ID entries `(id, target)`.
pub fn directory(entries: &[(u32, u32)]) -> Vec<u8> {
    let mut bytes = vec![0u8; 16];
    bytes[14..16].copy_from_slice(&(entries.len() as u16).to_le_bytes());
    for (id, target) in entries {
        bytes.extend_from_slice(&id.to_le_bytes());
        bytes.extend_from_slice(&target.to_le_bytes());
    }
    bytes
}

/// A `.rsrc` section holding one `RT_VERSION` resource per `(language,
/// blob)`, under type 16 and name 1. `rva` is where the section is mapped.
pub fn rsrc_with_versions(rva: u32, languages: &[(u32, Vec<u8>)]) -> Vec<u8> {
    let lang_dir_at = 0x30usize;
    let entries_at = lang_dir_at + 16 + 8 * languages.len();
    let blobs_at = entries_at + 16 * languages.len();
    let mut blob_offsets = Vec::new();
    let mut cursor = blobs_at;
    for (_, blob) in languages {
        blob_offsets.push(cursor);
        cursor = (cursor + blob.len()).next_multiple_of(4);
    }
    let mut out = directory(&[(16, SUBDIR | 0x18)]);
    out.extend(directory(&[(1, SUBDIR | lang_dir_at as u32)]));
    let lang_entries: Vec<(u32, u32)> = languages
        .iter()
        .enumerate()
        .map(|(i, (lang, _))| (*lang, (entries_at + 16 * i) as u32))
        .collect();
    out.extend(directory(&lang_entries));
    assert_eq!(out.len(), entries_at);
    for (i, (_, blob)) in languages.iter().enumerate() {
        out.extend_from_slice(&(rva + blob_offsets[i] as u32).to_le_bytes());
        out.extend_from_slice(&(blob.len() as u32).to_le_bytes());
        out.extend_from_slice(&1200u32.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
    }
    assert_eq!(out.len(), blobs_at);
    for (i, (_, blob)) in languages.iter().enumerate() {
        out.resize(blob_offsets[i], 0);
        out.extend_from_slice(blob);
    }
    out
}

fn pad4(bytes: &mut Vec<u8>) {
    while bytes.len() % 4 != 0 {
        bytes.push(0);
    }
}

/// One version block: header, key, value and children.
pub fn block(key: &str, value: &[u8], value_len: u16, text: bool, children: &[Vec<u8>]) -> Vec<u8> {
    let mut out = vec![0u8; 6];
    out.extend(utf16z(key));
    pad4(&mut out);
    out.extend_from_slice(value);
    for child in children {
        pad4(&mut out);
        out.extend_from_slice(child);
    }
    let length = out.len() as u16;
    out[0..2].copy_from_slice(&length.to_le_bytes());
    out[2..4].copy_from_slice(&value_len.to_le_bytes());
    out[4..6].copy_from_slice(&u16::from(text).to_le_bytes());
    out
}

pub fn string_block(key: &str, value: &str) -> Vec<u8> {
    let bytes = utf16z(value);
    block(key, &bytes, (bytes.len() / 2) as u16, true, &[])
}

/// A `VS_FIXEDFILEINFO` for `(file, product)`, each four parts.
pub fn fixed_info(file: [u16; 4], product: [u16; 4]) -> Vec<u8> {
    let ms = |p: [u16; 4]| (u32::from(p[0]) << 16) | u32::from(p[1]);
    let ls = |p: [u16; 4]| (u32::from(p[2]) << 16) | u32::from(p[3]);
    let mut out = Vec::new();
    for word in [
        0xFEEF_04BDu32,
        0x0001_0000,
        ms(file),
        ls(file),
        ms(product),
        ls(product),
        0x3F,
        0,
        0x0004_0004,
        1,
        0,
        0,
        0,
    ] {
        out.extend_from_slice(&word.to_le_bytes());
    }
    out
}

/// A whole `VS_VERSIONINFO` block with a single string table.
pub fn version_blob(fixed: Option<([u16; 4], [u16; 4])>, strings: &[(&str, &str)]) -> Vec<u8> {
    let entries: Vec<Vec<u8>> = strings.iter().map(|(k, v)| string_block(k, v)).collect();
    let table = block("040904B0", &[], 0, true, &entries);
    let info = block("StringFileInfo", &[], 0, true, &[table]);
    let value = fixed.map(|(file, product)| fixed_info(file, product));
    let value_bytes = value.clone().unwrap_or_default();
    block(
        "VS_VERSION_INFO",
        &value_bytes,
        value_bytes.len() as u16,
        false,
        &[info],
    )
}

/// RVAs of the standard layout: `.text`, `.rdata`, `.data`, `.rsrc`.
pub const TEXT_RVA: u32 = 0x1000;
pub const RDATA_RVA: u32 = 0x2000;
pub const DATA_RVA: u32 = 0x3000;
pub const RSRC_RVA: u32 = 0x4000;

/// A standard four-section image. `rdata` and `data` are the contents of
/// those sections, `rsrc` the resource section when there is one.
pub fn standard_pe(rdata: Vec<u8>, data: Vec<u8>, rsrc: Option<Vec<u8>>) -> PeSpec {
    let mut spec = PeSpec::new()
        .section(".text", TEXT_RVA, vec![0xC3; 0x100], CODE)
        .section(".rdata", RDATA_RVA, rdata, RDATA)
        .section(".data", DATA_RVA, data, DATA);
    if let Some(rsrc) = rsrc {
        let size = rsrc.len() as u32;
        spec = spec
            .section(".rsrc", RSRC_RVA, rsrc, RDATA)
            .resource_dir(RSRC_RVA, size);
    }
    spec
}

/// A standard image with one version resource built from `strings`.
pub fn pe_with_version(strings: &[(&str, &str)]) -> PeSpec {
    let blob = version_blob(None, strings);
    let rsrc = rsrc_with_versions(RSRC_RVA, &[(0x409, blob)]);
    standard_pe(vec![0u8; 0x100], vec![0u8; 0x100], Some(rsrc))
}

/// Pads `rdata` text: `items` placed at `(offset, bytes)` in a zeroed buffer.
pub fn rdata_with(len: usize, items: &[(usize, Vec<u8>)]) -> Vec<u8> {
    let mut rdata = vec![0u8; len];
    for (offset, bytes) in items {
        rdata[*offset..*offset + bytes.len()].copy_from_slice(bytes);
    }
    rdata
}
