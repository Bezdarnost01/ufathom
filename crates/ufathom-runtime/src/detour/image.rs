//! Loaded modules: their base address, and a signature search over their
//! executable sections using the PE headers in memory.

use ufathom_mem::{MemoryReader, SelfMemory};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;

use super::{DetourError, Signature};

/// Bytes of the image read to parse its headers.
const HEADER_READ: usize = 0x1000;

/// `MZ`, at the start of every PE image.
const DOS_MAGIC: u16 = 0x5A4D;
/// Offset in the DOS header of the offset to the NT headers.
const DOS_E_LFANEW: usize = 0x3C;
/// `PE\0\0`.
const NT_SIGNATURE: u32 = 0x0000_4550;
/// `IMAGE_FILE_MACHINE_AMD64`.
const MACHINE_AMD64: u16 = 0x8664;
/// Offsets from the NT signature to fields of the file header.
const FILE_MACHINE: usize = 4;
const FILE_SECTION_COUNT: usize = 6;
const FILE_OPTIONAL_SIZE: usize = 20;
/// Size of the signature plus the file header: where the optional header
/// starts.
const OPTIONAL_HEADER: usize = 24;
/// Offset of `SizeOfImage` in the optional header of a PE32+ image.
const OPTIONAL_IMAGE_SIZE: usize = 56;
/// Size of one section header, and the offsets of its fields.
const SECTION_HEADER_SIZE: usize = 40;
const SECTION_VIRTUAL_SIZE: usize = 8;
const SECTION_VIRTUAL_ADDRESS: usize = 12;
const SECTION_RAW_SIZE: usize = 16;
const SECTION_CHARACTERISTICS: usize = 36;
/// `IMAGE_SCN_MEM_EXECUTE`.
const SECTION_EXECUTE: u32 = 0x2000_0000;

/// Bytes read at a time while scanning.
const SCAN_CHUNK: usize = 1 << 20;
/// Granule of the page-by-page retry when a chunk cannot be read.
const SCAN_PAGE: usize = 0x1000;

/// Address where a loaded module starts, for adding an RVA to.
///
/// `None` is the executable of the process. A name is matched the way
/// `GetModuleHandleW` does: `"kernel32.dll"`, with or without the extension.
///
/// # Errors
///
/// [`DetourError::ModuleNotFound`] when no such module is loaded.
pub fn module_base(name: Option<&str>) -> Result<usize, DetourError> {
    let wide: Option<Vec<u16>> = name.map(|name| name.encode_utf16().chain(Some(0)).collect());
    let pointer = wide
        .as_ref()
        .map_or(core::ptr::null(), |wide| wide.as_ptr());
    // SAFETY: `pointer` is null or a NUL-terminated string that lives
    // through the call. The call adds no reference to the module.
    let module = unsafe { GetModuleHandleW(pointer) };
    if module.is_null() {
        return Err(DetourError::ModuleNotFound(
            name.unwrap_or("<the process executable>").to_owned(),
        ));
    }
    Ok(module as usize)
}

/// Finds the one place where `signature` matches in the executable sections
/// of a loaded module, and returns its address.
///
/// `None` searches the process executable. The sections are found from the
/// module's PE headers in memory; only pages that can be read are searched.
///
/// # Errors
///
/// - [`DetourError::ModuleNotFound`] and [`DetourError::BadImage`] when the
///   module cannot be found or understood.
/// - [`DetourError::SignatureMatches`] when the signature matches nowhere or
///   in more than one place; `matches` says which. A signature that is not
///   unique must be made longer, not guessed at.
pub fn find_in_module(
    module_name: Option<&str>,
    signature: &Signature,
) -> Result<usize, DetourError> {
    let base = module_base(module_name)?;
    let memory = SelfMemory::new();
    let sections = executable_sections(&memory, base)?;
    let mut scanner = Scanner::new(signature);
    for (start, end) in sections {
        scanner.scan_range(&memory, start, end);
    }
    match (scanner.matches, scanner.first) {
        (1, Some(address)) => Ok(address),
        (matches, _) => Err(DetourError::SignatureMatches { matches }),
    }
}

fn read_u16(headers: &[u8], offset: usize) -> Option<u16> {
    let bytes = headers.get(offset..offset.checked_add(2)?)?;
    Some(u16::from_le_bytes(bytes.try_into().ok()?))
}

fn read_u32(headers: &[u8], offset: usize) -> Option<u32> {
    let bytes = headers.get(offset..offset.checked_add(4)?)?;
    Some(u32::from_le_bytes(bytes.try_into().ok()?))
}

/// The `[start, end)` address ranges of the sections marked executable.
fn executable_sections(
    memory: &SelfMemory,
    base: usize,
) -> Result<Vec<(usize, usize)>, DetourError> {
    let bad = |reason| DetourError::BadImage { base, reason };
    let mut headers = vec![0u8; HEADER_READ];
    memory
        .read_exact(base as u64, &mut headers)
        .map_err(|_| bad("the headers cannot be read"))?;
    if read_u16(&headers, 0) != Some(DOS_MAGIC) {
        return Err(bad("no MZ signature"));
    }
    let nt = read_u32(&headers, DOS_E_LFANEW)
        .and_then(|offset| usize::try_from(offset).ok())
        .ok_or_else(|| bad("no NT header offset"))?;
    let field = |offset: usize| nt.checked_add(offset);
    if field(0).and_then(|at| read_u32(&headers, at)) != Some(NT_SIGNATURE) {
        return Err(bad("no PE signature"));
    }
    if field(FILE_MACHINE).and_then(|at| read_u16(&headers, at)) != Some(MACHINE_AMD64) {
        return Err(bad("not an x86-64 image"));
    }
    let count = field(FILE_SECTION_COUNT)
        .and_then(|at| read_u16(&headers, at))
        .ok_or_else(|| bad("no section count"))?;
    let optional_size = field(FILE_OPTIONAL_SIZE)
        .and_then(|at| read_u16(&headers, at))
        .ok_or_else(|| bad("no optional header size"))?;
    let image_size = field(OPTIONAL_HEADER + OPTIONAL_IMAGE_SIZE)
        .and_then(|at| read_u32(&headers, at))
        .and_then(|size| usize::try_from(size).ok())
        .ok_or_else(|| bad("no image size"))?;
    let table = nt
        .checked_add(OPTIONAL_HEADER)
        .and_then(|at| at.checked_add(usize::from(optional_size)))
        .ok_or_else(|| bad("section table overflows"))?;

    let mut sections = Vec::new();
    for index in 0..usize::from(count) {
        let header = index
            .checked_mul(SECTION_HEADER_SIZE)
            .and_then(|offset| table.checked_add(offset))
            .ok_or_else(|| bad("section table overflows"))?;
        let get = |offset: usize| {
            header
                .checked_add(offset)
                .and_then(|at| read_u32(&headers, at))
        };
        let characteristics = get(SECTION_CHARACTERISTICS)
            .ok_or_else(|| bad("the section table extends past the first page"))?;
        if characteristics & SECTION_EXECUTE == 0 {
            continue;
        }
        let address = get(SECTION_VIRTUAL_ADDRESS).ok_or_else(|| bad("truncated section"))?;
        let virtual_size = get(SECTION_VIRTUAL_SIZE).ok_or_else(|| bad("truncated section"))?;
        let raw_size = get(SECTION_RAW_SIZE).ok_or_else(|| bad("truncated section"))?;
        let size = if virtual_size == 0 {
            raw_size
        } else {
            virtual_size
        };
        let start = usize::try_from(address).map_err(|_| bad("section address"))?;
        let end = start
            .checked_add(usize::try_from(size).map_err(|_| bad("section size"))?)
            .ok_or_else(|| bad("section end overflows"))?
            .min(image_size);
        if start < end {
            sections.push((
                base.checked_add(start)
                    .ok_or_else(|| bad("section start"))?,
                base.checked_add(end).ok_or_else(|| bad("section end"))?,
            ));
        }
    }
    Ok(sections)
}

/// Counts the matches of a signature in memory read piece by piece, carrying
/// the end of one piece into the next so a match across the join is seen.
struct Scanner<'a> {
    signature: &'a Signature,
    /// How many places matched.
    matches: usize,
    /// Address of the first match.
    first: Option<usize>,
    /// The last bytes of the previous piece, fewer than the signature.
    tail: Vec<u8>,
    /// Address just after `tail`.
    tail_end: usize,
    scratch: Vec<u8>,
}

impl<'a> Scanner<'a> {
    fn new(signature: &'a Signature) -> Self {
        Self {
            signature,
            matches: 0,
            first: None,
            tail: Vec::new(),
            tail_end: 0,
            scratch: Vec::new(),
        }
    }

    /// Searches `[start, end)`, skipping what cannot be read.
    fn scan_range(&mut self, memory: &SelfMemory, start: usize, end: usize) {
        self.tail.clear();
        let mut chunk = Vec::new();
        let mut cursor = start;
        while cursor < end {
            let len = (end - cursor).min(SCAN_CHUNK);
            chunk.resize(len, 0);
            if memory.read_exact(cursor as u64, &mut chunk).is_ok() {
                self.feed(cursor, &chunk);
            } else {
                self.scan_pages(memory, cursor, cursor + len);
            }
            cursor += len;
        }
    }

    /// The slow path for a chunk with an unreadable page in it.
    fn scan_pages(&mut self, memory: &SelfMemory, start: usize, end: usize) {
        let mut page = vec![0u8; SCAN_PAGE];
        let mut cursor = start;
        while cursor < end {
            let len = (end - cursor).min(SCAN_PAGE);
            page.truncate(len);
            if memory.read_exact(cursor as u64, &mut page).is_ok() {
                self.feed(cursor, &page);
            } else {
                self.tail.clear();
            }
            cursor += len;
        }
    }

    /// Searches `bytes`, which lie at `address`.
    fn feed(&mut self, address: usize, bytes: &[u8]) {
        let joined_at = if self.tail_end == address {
            address - self.tail.len()
        } else {
            self.tail.clear();
            address
        };
        self.scratch.clear();
        self.scratch.extend_from_slice(&self.tail);
        self.scratch.extend_from_slice(bytes);
        for offset in self.signature.positions_in(&self.scratch) {
            self.matches += 1;
            if self.first.is_none() {
                self.first = Some(joined_at + offset);
            }
        }
        // Any match found later starts in the last `len - 1` bytes at most.
        let keep = self
            .signature
            .len()
            .saturating_sub(1)
            .min(self.scratch.len());
        self.tail.clear();
        if let Some(rest) = self.scratch.get(self.scratch.len() - keep..) {
            self.tail.extend_from_slice(rest);
        }
        self.tail_end = address + bytes.len();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_match_across_two_pieces_is_found_once() {
        let signature = Signature::parse("11 22 33 44").unwrap();
        let mut scanner = Scanner::new(&signature);
        scanner.feed(0x1000, &[0, 0, 0x11, 0x22]);
        scanner.feed(0x1004, &[0x33, 0x44, 0]);
        assert_eq!(scanner.matches, 1);
        assert_eq!(scanner.first, Some(0x1002));
    }

    #[test]
    fn a_gap_breaks_a_match() {
        let signature = Signature::parse("11 22 33 44").unwrap();
        let mut scanner = Scanner::new(&signature);
        scanner.feed(0x1000, &[0, 0, 0x11, 0x22]);
        scanner.feed(0x2000, &[0x33, 0x44, 0]);
        assert_eq!(scanner.matches, 0);
    }

    #[test]
    fn unknown_modules_are_reported() {
        assert!(matches!(
            module_base(Some("no-such-module-for-tests.dll")),
            Err(DetourError::ModuleNotFound(_))
        ));
        assert!(module_base(None).is_ok());
    }
}
