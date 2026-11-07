//! PE image parsing, from memory and from a file.
//!
//! [`PeImage::parse_from_memory`] reads the headers of a module through a
//! [`MemoryReader`], so it works the same on a live process, a minidump and a
//! [`Snapshot`](ufathom_mem::Snapshot). [`load_pe_file`] maps a PE file into a
//! snapshot as a loader would, for static analysis.
//!
//! Every value in a header is untrusted. Counts and sizes are bounded by the
//! constants in [`crate::limits`], addresses use checked arithmetic, and
//! malformed input returns a [`ScanError`].

mod load;

pub use load::{load_pe_file, load_pe_reader};

use ufathom_mem::{Addr, MemoryReader, MemoryReaderExt, Perms};

use crate::bytes::Le;
use crate::error::ScanError;
use crate::limits::{MAX_DATA_DIRECTORIES, MAX_NT_OFFSET, MAX_SECTIONS};

/// `MZ`, the first two bytes of a DOS header.
const DOS_MAGIC: u16 = 0x5A4D;
/// `PE\0\0`, the signature of the NT headers.
const NT_SIGNATURE: u32 = 0x0000_4550;
/// Size of the DOS header; `e_lfanew` is its last field.
const DOS_HEADER_SIZE: usize = 0x40;
/// Offset of `e_lfanew` in the DOS header.
const E_LFANEW_OFFSET: usize = 0x3C;
/// Size of the NT signature plus the COFF file header.
const NT_PREAMBLE_SIZE: u64 = 24;
/// Size of one section header.
const SECTION_HEADER_SIZE: usize = 40;
/// Optional header magic of a 32-bit image.
const OPTIONAL_MAGIC_PE32: u16 = 0x10B;
/// Optional header magic of a 64-bit image.
const OPTIONAL_MAGIC_PE32_PLUS: u16 = 0x20B;
/// Size of a 32-bit optional header without data directories.
const OPTIONAL_HEADER_PE32_FIXED: usize = 96;
/// Size of a 64-bit optional header without data directories.
const OPTIONAL_HEADER_PE32_PLUS_FIXED: usize = 112;

/// Index of the export table in the data directories.
pub const DIRECTORY_EXPORT: usize = 0;
/// Index of the import table in the data directories.
pub const DIRECTORY_IMPORT: usize = 1;
/// Index of the resource tree in the data directories.
pub const DIRECTORY_RESOURCE: usize = 2;

/// Section flag: the section contains code.
pub const SCN_CNT_CODE: u32 = 0x0000_0020;
/// Section flag: the section can be executed.
pub const SCN_MEM_EXECUTE: u32 = 0x2000_0000;
/// Section flag: the section can be read.
pub const SCN_MEM_READ: u32 = 0x4000_0000;
/// Section flag: the section can be written.
pub const SCN_MEM_WRITE: u32 = 0x8000_0000;

/// One entry of the section table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Section {
    /// Name, up to the first NUL. Non-UTF-8 bytes are replaced.
    pub name: String,
    /// Offset of the section from the image base once loaded.
    pub rva: u32,
    /// Size in memory. Zero means the loader uses [`Section::raw_size`].
    pub virtual_size: u32,
    /// Size of the section's data in the file.
    pub raw_size: u32,
    /// File offset of the section's data.
    pub raw_offset: u32,
    /// `IMAGE_SCN_*` flags.
    pub characteristics: u32,
}

impl Section {
    /// Bytes the section occupies in memory: the virtual size, or the raw
    /// size when the virtual size is zero, as the Windows loader does.
    #[must_use]
    pub fn mapped_size(&self) -> u32 {
        if self.virtual_size == 0 {
            self.raw_size
        } else {
            self.virtual_size
        }
    }

    /// Exclusive end of the section, as an RVA.
    #[must_use]
    pub fn end_rva(&self) -> u64 {
        u64::from(self.rva) + u64::from(self.mapped_size())
    }

    /// Whether `rva` lies inside the section.
    #[must_use]
    pub fn contains_rva(&self, rva: u32) -> bool {
        rva >= self.rva && u64::from(rva) < self.end_rva()
    }

    /// Whether the section is flagged executable.
    #[must_use]
    pub fn is_executable(&self) -> bool {
        self.characteristics & SCN_MEM_EXECUTE != 0
    }

    /// Whether the section is flagged writable.
    #[must_use]
    pub fn is_writable(&self) -> bool {
        self.characteristics & SCN_MEM_WRITE != 0
    }

    /// Whether the section is flagged readable.
    #[must_use]
    pub fn is_readable(&self) -> bool {
        self.characteristics & SCN_MEM_READ != 0
    }

    /// Whether the section holds read-only data: readable, neither writable
    /// nor executable. This is where build strings and name literals live.
    #[must_use]
    pub fn is_read_only_data(&self) -> bool {
        self.is_readable() && !self.is_writable() && !self.is_executable()
    }

    /// Access rights a loader gives the section. Write and execute imply
    /// read, since a static view of a section is only useful if it can be
    /// read. A section with none of the three flags has no access.
    #[must_use]
    pub fn perms(&self) -> Perms {
        let write = self.is_writable();
        let execute = self.is_executable();
        Perms::new(self.is_readable() || write || execute, write, execute)
    }
}

/// One data directory: where a table lives and how big it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DataDirectory {
    /// Offset of the table from the image base.
    pub rva: u32,
    /// Size of the table in bytes.
    pub size: u32,
}

/// The facts a PE image's headers declare.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct PeImage {
    /// Address the headers were read from.
    pub base: Addr,
    /// `IMAGE_FILE_MACHINE_*` value.
    pub machine: u16,
    /// Whether the optional header is PE32+ (64-bit).
    pub is_64_bit: bool,
    /// The preferred load address the file declares. [`PeImage::base`] is
    /// where the image is actually mapped.
    pub image_base: u64,
    /// COFF `Characteristics` flags.
    pub characteristics: u16,
    /// COFF `TimeDateStamp`.
    pub timestamp: u32,
    /// RVA of the entry point; zero when there is none.
    pub entry_point_rva: u32,
    /// Size of the mapped image, including headers.
    pub size_of_image: u32,
    /// Size of the headers in the file.
    pub size_of_headers: u32,
    /// File offset just past the section table.
    pub headers_end: u32,
    /// Alignment of sections in memory.
    pub section_alignment: u32,
    /// Alignment of section data in the file.
    pub file_alignment: u32,
    /// Windows subsystem value.
    pub subsystem: u16,
    /// Sections, in table order.
    pub sections: Vec<Section>,
    /// Data directories, indexed by `DIRECTORY_*`. Shorter than 16 when the
    /// optional header declares fewer.
    pub directories: Vec<DataDirectory>,
}

impl PeImage {
    /// Reads the headers of the PE image mapped at `base`.
    ///
    /// Both PE32 and PE32+ are accepted. Only headers and the section table
    /// are read, never section contents.
    ///
    /// # Errors
    ///
    /// - [`ScanError::NotPe`] if the DOS or NT signature is missing.
    /// - [`ScanError::LimitExceeded`] if the section count is above
    ///   [`MAX_SECTIONS`].
    /// - [`ScanError::Malformed`] for an NT offset, optional header, section
    ///   table or section range that cannot be right, including a section that
    ///   leaves the image or overlaps another.
    /// - [`ScanError::Unsupported`] for an optional header that is neither
    ///   PE32 nor PE32+.
    /// - [`ScanError::Memory`] if the headers are not readable.
    pub fn parse_from_memory(mem: &dyn MemoryReader, base: Addr) -> Result<Self, ScanError> {
        let dos = mem.read_vec::<u8>(base, DOS_HEADER_SIZE)?;
        let dos = Le::new(&dos, "DOS header");
        if dos.u16(0)? != DOS_MAGIC {
            return Err(ScanError::NotPe {
                reason: "missing MZ signature".to_owned(),
            });
        }
        let e_lfanew = dos.u32(E_LFANEW_OFFSET)?;
        if !(DOS_HEADER_SIZE as u32..=MAX_NT_OFFSET).contains(&e_lfanew) {
            return Err(ScanError::malformed(
                "DOS header",
                format!(
                    "NT header offset {e_lfanew:#x} is outside {DOS_HEADER_SIZE:#x}..={MAX_NT_OFFSET:#x}"
                ),
            ));
        }
        let nt_offset = u64::from(e_lfanew);
        let nt_addr = checked_addr(base, nt_offset, "NT headers")?;

        let preamble = mem.read_vec::<u8>(nt_addr, NT_PREAMBLE_SIZE as usize)?;
        let nt = Le::new(&preamble, "NT headers");
        if nt.u32(0)? != NT_SIGNATURE {
            return Err(ScanError::NotPe {
                reason: "missing PE signature".to_owned(),
            });
        }
        let machine = nt.u16(4)?;
        let section_count = nt.u16(6)?;
        let timestamp = nt.u32(8)?;
        let optional_size = usize::from(nt.u16(20)?);
        let characteristics = nt.u16(22)?;
        if section_count > MAX_SECTIONS {
            return Err(ScanError::LimitExceeded {
                what: "section count",
                value: u64::from(section_count),
                limit: u64::from(MAX_SECTIONS),
            });
        }
        if section_count == 0 {
            return Err(ScanError::malformed(
                "COFF header",
                "the image has no sections",
            ));
        }

        let optional_addr = checked_addr(nt_addr, NT_PREAMBLE_SIZE, "optional header")?;
        let optional_bytes = mem.read_vec::<u8>(optional_addr, optional_size)?;
        let optional = Le::new(&optional_bytes, "optional header");
        let (is_64_bit, fixed_size) = match optional.u16(0)? {
            OPTIONAL_MAGIC_PE32_PLUS => (true, OPTIONAL_HEADER_PE32_PLUS_FIXED),
            OPTIONAL_MAGIC_PE32 => (false, OPTIONAL_HEADER_PE32_FIXED),
            magic => {
                return Err(ScanError::Unsupported {
                    reason: format!("optional header magic {magic:#x} is neither PE32 nor PE32+"),
                });
            }
        };
        if optional_size < fixed_size {
            return Err(ScanError::malformed(
                "optional header",
                format!("size {optional_size:#x} is below the minimum {fixed_size:#x}"),
            ));
        }
        let entry_point_rva = optional.u32(16)?;
        let image_base = if is_64_bit {
            optional.u64(24)?
        } else {
            u64::from(optional.u32(28)?)
        };
        let section_alignment = optional.u32(32)?;
        let file_alignment = optional.u32(36)?;
        let size_of_image = optional.u32(56)?;
        let size_of_headers = optional.u32(60)?;
        let subsystem = optional.u16(68)?;
        let declared_directories = optional.u32(fixed_size - 4)?;
        if size_of_image == 0 {
            return Err(ScanError::malformed(
                "optional header",
                "SizeOfImage is zero",
            ));
        }

        let room = (optional_size - fixed_size) / 8;
        let directory_count = declared_directories
            .min(MAX_DATA_DIRECTORIES)
            .min(u32::try_from(room).unwrap_or(u32::MAX));
        let mut directories = Vec::with_capacity(directory_count as usize);
        for index in 0..directory_count as usize {
            let at = fixed_size + index * 8;
            directories.push(DataDirectory {
                rva: optional.u32(at)?,
                size: optional.u32(at + 4)?,
            });
        }

        let table_addr = checked_addr(optional_addr, optional_size as u64, "section table")?;
        let table_bytes =
            mem.read_vec::<u8>(table_addr, usize::from(section_count) * SECTION_HEADER_SIZE)?;
        let table = Le::new(&table_bytes, "section table");
        let mut sections = Vec::with_capacity(usize::from(section_count));
        for index in 0..usize::from(section_count) {
            let at = index * SECTION_HEADER_SIZE;
            let raw_name = table.slice(at, 8)?;
            let name_len = raw_name
                .iter()
                .position(|&byte| byte == 0)
                .unwrap_or(raw_name.len());
            let name =
                String::from_utf8_lossy(raw_name.get(..name_len).unwrap_or_default()).into_owned();
            sections.push(Section {
                name,
                virtual_size: table.u32(at + 8)?,
                rva: table.u32(at + 12)?,
                raw_size: table.u32(at + 16)?,
                raw_offset: table.u32(at + 20)?,
                characteristics: table.u32(at + 36)?,
            });
        }
        validate_sections(&sections, size_of_image)?;

        let headers_end = nt_offset
            + NT_PREAMBLE_SIZE
            + optional_size as u64
            + (usize::from(section_count) * SECTION_HEADER_SIZE) as u64;
        Ok(Self {
            base,
            machine,
            is_64_bit,
            image_base,
            characteristics,
            timestamp,
            entry_point_rva,
            size_of_image,
            size_of_headers,
            // At most 0x1_0000 + 24 + 0xFFFF + 96 * 40, which fits in 32 bits.
            headers_end: u32::try_from(headers_end).unwrap_or(u32::MAX),
            section_alignment,
            file_alignment,
            subsystem,
            sections,
            directories,
        })
    }

    /// The first section called `name`.
    #[must_use]
    pub fn section_named(&self, name: &str) -> Option<&Section> {
        self.sections.iter().find(|section| section.name == name)
    }

    /// The section that contains `rva`.
    #[must_use]
    pub fn section_at_rva(&self, rva: u32) -> Option<&Section> {
        self.sections
            .iter()
            .find(|section| section.contains_rva(rva))
    }

    /// The data directory at `index`, or `None` when the optional header has
    /// no such entry or the entry is empty (its RVA is zero).
    #[must_use]
    pub fn directory(&self, index: usize) -> Option<DataDirectory> {
        self.directories
            .get(index)
            .copied()
            .filter(|directory| directory.rva != 0)
    }

    /// The address of `rva` in memory, if the `len` bytes from it lie inside
    /// the image and the address does not overflow.
    #[must_use]
    pub fn address_of(&self, rva: u64, len: u64) -> Option<Addr> {
        let end = rva.checked_add(len)?;
        if end > u64::from(self.size_of_image) {
            return None;
        }
        let addr = self.base.checked_add(rva)?;
        addr.checked_add(len)?;
        Some(addr)
    }
}

/// `base + offset`, or a [`ScanError::Malformed`] naming `what`.
fn checked_addr(base: Addr, offset: u64, what: &'static str) -> Result<Addr, ScanError> {
    base.checked_add(offset).ok_or_else(|| {
        ScanError::malformed(
            what,
            format!("address {base:#x} + {offset:#x} overflows the address space"),
        )
    })
}

/// Checks that every section lies inside the image and that none overlap.
fn validate_sections(sections: &[Section], size_of_image: u32) -> Result<(), ScanError> {
    for section in sections {
        if section.end_rva() > u64::from(size_of_image) {
            return Err(ScanError::malformed(
                "section table",
                format!(
                    "section `{}` ends at {:#x}, past SizeOfImage {size_of_image:#x}",
                    section.name,
                    section.end_rva()
                ),
            ));
        }
    }
    let mut ordered: Vec<&Section> = sections
        .iter()
        .filter(|section| section.mapped_size() != 0)
        .collect();
    ordered.sort_by_key(|section| section.rva);
    for pair in ordered.windows(2) {
        if let [first, second] = pair
            && first.end_rva() > u64::from(second.rva)
        {
            return Err(ScanError::malformed(
                "section table",
                format!("sections `{}` and `{}` overlap", first.name, second.name),
            ));
        }
    }
    Ok(())
}
