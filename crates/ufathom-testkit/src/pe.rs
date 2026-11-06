//! A fake PE module: DOS and NT headers, four sections and a real
//! `VS_VERSIONINFO` resource tree.
//!
//! The headers follow the PE/COFF specification for a 64-bit executable. The
//! resource directory has the usual three levels (type, name, language) and a
//! leaf pointing at a `VS_VERSIONINFO` block, so a reader that walks the
//! resource data directory finds `ProductVersion` the same way it would in a
//! real game executable.

use crate::profile::EngineIdentity;
use crate::space::{Addresses, DATA_RVA, RDATA_RVA, RSRC_RVA, TEXT_RVA};

/// Size of the mapped header page.
pub const HEADER_SIZE: usize = 0x1000;
/// File offset where the first section would start.
const SIZE_OF_HEADERS: u32 = 0x400;
/// Offset of the NT headers: `e_lfanew`.
pub const NT_OFFSET: usize = 0x80;
/// `IMAGE_DIRECTORY_ENTRY_RESOURCE`.
pub const DIRECTORY_RESOURCE: usize = 2;
/// Resource type id of `RT_VERSION`.
pub const RT_VERSION: u32 = 16;
/// Resource name id of the version block.
const VERSION_RESOURCE_ID: u32 = 1;
/// Language id: US English.
const LANG_EN_US: u32 = 0x409;
/// Set on a resource directory entry whose target is another directory.
const SUBDIRECTORY_BIT: u32 = 0x8000_0000;

/// The sections of the fake module, in order.
pub const SECTION_NAMES: [&str; 4] = [".text", ".rdata", ".data", ".rsrc"];

struct Writer {
    bytes: Vec<u8>,
}

impl Writer {
    fn new(size: usize) -> Self {
        Self {
            bytes: vec![0; size],
        }
    }

    fn put(&mut self, offset: usize, data: &[u8]) {
        if let Some(slot) = self.bytes.get_mut(offset..offset + data.len()) {
            slot.copy_from_slice(data);
        }
    }

    fn u16(&mut self, offset: usize, value: u16) {
        self.put(offset, &value.to_le_bytes());
    }

    fn u32(&mut self, offset: usize, value: u32) {
        self.put(offset, &value.to_le_bytes());
    }

    fn u64(&mut self, offset: usize, value: u64) {
        self.put(offset, &value.to_le_bytes());
    }
}

fn rva32(value: u64) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

/// Builds the header page. `lens` are the used lengths of `.text`, `.rdata`,
/// `.data` and `.rsrc`; `entry_rva` is the address of the entry point.
#[must_use]
pub fn build_headers(addrs: &Addresses, lens: [usize; 4], entry_rva: u64) -> Vec<u8> {
    let mut w = Writer::new(HEADER_SIZE);
    // IMAGE_DOS_HEADER
    w.put(0, b"MZ");
    w.u16(2, 0x90); // bytes on last page
    w.u16(4, 3); // pages
    w.u16(8, 4); // header paragraphs
    w.u16(0xC, 0xFFFF); // max extra paragraphs
    w.u16(0x10, 0xB8); // initial sp
    w.u16(0x18, 0x40); // relocation table offset
    w.u32(0x3C, NT_OFFSET as u32);
    // The classic DOS stub.
    let stub: [u8; 14] = [
        0x0E, 0x1F, 0xBA, 0x0E, 0x00, 0xB4, 0x09, 0xCD, 0x21, 0xB8, 0x01, 0x4C, 0xCD, 0x21,
    ];
    w.put(0x40, &stub);
    w.put(0x4E, b"This program cannot be run in DOS mode.\r\r\n$");

    // IMAGE_NT_HEADERS64
    let nt = NT_OFFSET;
    w.put(nt, b"PE\0\0");
    let file = nt + 4;
    w.u16(file, 0x8664); // Machine: AMD64
    w.u16(file + 2, 4); // NumberOfSections
    w.u32(file + 4, 0x6000_0000); // TimeDateStamp: fixed, never the clock
    w.u16(file + 16, 0xF0); // SizeOfOptionalHeader
    w.u16(file + 18, 0x0022); // EXECUTABLE_IMAGE | LARGE_ADDRESS_AWARE

    let opt = file + 20;
    let size_of_image = RSRC_RVA + lens[3].next_multiple_of(0x1000) as u64;
    w.u16(opt, 0x20B); // PE32+
    w.put(opt + 2, &[14, 29]); // linker version
    w.u32(opt + 4, rva32(lens[0].next_multiple_of(0x200) as u64)); // SizeOfCode
    w.u32(
        opt + 8,
        rva32((lens[1] + lens[2] + lens[3]).next_multiple_of(0x200) as u64),
    ); // SizeOfInitializedData
    w.u32(opt + 16, rva32(entry_rva)); // AddressOfEntryPoint
    w.u32(opt + 20, rva32(TEXT_RVA)); // BaseOfCode
    w.u64(opt + 24, addrs.module_base); // ImageBase
    w.u32(opt + 32, 0x1000); // SectionAlignment
    w.u32(opt + 36, 0x200); // FileAlignment
    w.u16(opt + 40, 6); // MajorOperatingSystemVersion
    w.u16(opt + 48, 6); // MajorSubsystemVersion
    w.u32(opt + 56, rva32(size_of_image)); // SizeOfImage
    w.u32(opt + 60, SIZE_OF_HEADERS); // SizeOfHeaders
    w.u16(opt + 68, 2); // Subsystem: Windows GUI
    w.u16(opt + 70, 0x8160); // HIGH_ENTROPY_VA | DYNAMIC_BASE | NX_COMPAT | TS_AWARE
    w.u64(opt + 72, 0x10_0000); // SizeOfStackReserve
    w.u64(opt + 80, 0x1000); // SizeOfStackCommit
    w.u64(opt + 88, 0x10_0000); // SizeOfHeapReserve
    w.u64(opt + 96, 0x1000); // SizeOfHeapCommit
    w.u32(opt + 108, 16); // NumberOfRvaAndSizes
    let directories = opt + 112;
    let rsrc_dir = directories + DIRECTORY_RESOURCE * 8;
    w.u32(rsrc_dir, rva32(RSRC_RVA));
    w.u32(rsrc_dir + 4, rva32(lens[3] as u64));

    // IMAGE_SECTION_HEADERs
    let table = opt + 0xF0;
    let rvas = [TEXT_RVA, RDATA_RVA, DATA_RVA, RSRC_RVA];
    let characteristics = [0x6000_0020u32, 0x4000_0040, 0xC000_0040, 0x4000_0040];
    let mut raw_pointer = SIZE_OF_HEADERS;
    for (i, name) in SECTION_NAMES.iter().enumerate() {
        let header = table + i * 40;
        let mut padded = [0u8; 8];
        for (dst, src) in padded.iter_mut().zip(name.bytes()) {
            *dst = src;
        }
        let used = lens.get(i).copied().unwrap_or(0);
        let raw_size = rva32(used.next_multiple_of(0x200) as u64);
        w.put(header, &padded);
        w.u32(header + 8, rva32(used as u64)); // VirtualSize
        w.u32(header + 12, rva32(rvas.get(i).copied().unwrap_or(0))); // VirtualAddress
        w.u32(header + 16, raw_size);
        w.u32(header + 20, raw_pointer);
        w.u32(header + 36, characteristics.get(i).copied().unwrap_or(0));
        raw_pointer += raw_size;
    }
    w.bytes
}

fn utf16z(text: &str) -> Vec<u8> {
    text.encode_utf16()
        .chain(std::iter::once(0))
        .flat_map(u16::to_le_bytes)
        .collect()
}

fn pad4(bytes: &mut Vec<u8>) {
    while bytes.len() % 4 != 0 {
        bytes.push(0);
    }
}

/// One `VS_VERSIONINFO`-family block: header, key, value and children.
fn version_block(
    key: &str,
    value: &[u8],
    value_len: u16,
    text: bool,
    children: &[Vec<u8>],
) -> Vec<u8> {
    let mut block = vec![0u8; 6];
    block.extend(utf16z(key));
    pad4(&mut block);
    block.extend_from_slice(value);
    for child in children {
        pad4(&mut block);
        block.extend_from_slice(child);
    }
    let length = u16::try_from(block.len()).unwrap_or(u16::MAX);
    for (offset, field) in [(0, length), (2, value_len), (4, u16::from(text))] {
        if let Some(slot) = block.get_mut(offset..offset + 2) {
            slot.copy_from_slice(&field.to_le_bytes());
        }
    }
    block
}

fn string_entry(key: &str, value: &str) -> Vec<u8> {
    let bytes = utf16z(value);
    let chars = u16::try_from(bytes.len() / 2).unwrap_or(0);
    version_block(key, &bytes, chars, true, &[])
}

/// The `VS_VERSIONINFO` resource for `identity`.
#[must_use]
pub fn version_info(identity: &EngineIdentity) -> Vec<u8> {
    let (major, minor, patch) = identity.version_parts();
    let ms = (u32::from(major) << 16) | u32::from(minor);
    let ls = u32::from(patch) << 16;
    let mut fixed = Vec::with_capacity(52);
    for word in [
        0xFEEF_04BDu32, // dwSignature
        0x0001_0000,    // dwStrucVersion
        ms,             // dwFileVersionMS
        ls,             // dwFileVersionLS
        ms,             // dwProductVersionMS
        ls,             // dwProductVersionLS
        0x3F,           // dwFileFlagsMask
        0,              // dwFileFlags
        0x0004_0004,    // dwFileOS: VOS_NT_WINDOWS32
        1,              // dwFileType: VFT_APP
        0,              // dwFileSubtype
        0,              // dwFileDateMS
        0,              // dwFileDateLS
    ] {
        fixed.extend_from_slice(&word.to_le_bytes());
    }
    let strings = [
        string_entry("CompanyName", "Synthetic Test Studio"),
        string_entry("FileDescription", "Synthetic Unreal Engine test image"),
        string_entry("FileVersion", &identity.product_version),
        string_entry("InternalName", "Game-Win64-Shipping"),
        string_entry("OriginalFilename", "Game-Win64-Shipping.exe"),
        string_entry("ProductName", "Synthetic Game"),
        string_entry("ProductVersion", &identity.product_version),
    ];
    let table = version_block("040904B0", &[], 0, true, &strings);
    let string_info = version_block("StringFileInfo", &[], 0, true, &[table]);
    let translation = version_block("Translation", &[0x09, 0x04, 0xB0, 0x04], 4, false, &[]);
    let var_info = version_block("VarFileInfo", &[], 0, true, &[translation]);
    version_block(
        "VS_VERSION_INFO",
        &fixed,
        52,
        false,
        &[string_info, var_info],
    )
}

/// Builds the `.rsrc` section: the three directory levels, the data entry and
/// the version block. Returns the bytes (not page-rounded).
#[must_use]
pub fn build_rsrc(identity: &EngineIdentity) -> Vec<u8> {
    let data = version_info(identity);
    let dir_size = 16 + 8;
    let level1 = 0usize;
    let level2 = level1 + dir_size;
    let level3 = level2 + dir_size;
    let entry = level3 + dir_size;
    let blob = (entry + 16).next_multiple_of(4);
    let mut w = Writer::new(blob + data.len());
    for (dir, id, target, subdirectory) in [
        (level1, RT_VERSION, level2, true),
        (level2, VERSION_RESOURCE_ID, level3, true),
        (level3, LANG_EN_US, entry, false),
    ] {
        // IMAGE_RESOURCE_DIRECTORY: one ID entry.
        w.u16(dir + 14, 1);
        w.u32(dir + 16, id);
        let target = u32::try_from(target).unwrap_or(0);
        w.u32(
            dir + 20,
            if subdirectory {
                target | SUBDIRECTORY_BIT
            } else {
                target
            },
        );
    }
    // IMAGE_RESOURCE_DATA_ENTRY
    w.u32(entry, rva32(RSRC_RVA + blob as u64));
    w.u32(entry + 4, u32::try_from(data.len()).unwrap_or(0));
    w.u32(entry + 8, 1200); // code page: UTF-16
    w.put(blob, &data);
    w.bytes
}
